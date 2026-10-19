//! 持久任务层（world-gov 可选）—— 基于 `novel.db` 的 `task_jobs` 表。
//!
//! # 定位
//! - **可选、可关闭**：默认走 `pipeline.rs` 的纯内存队列；仅 `--persist` 才落库。
//! - **无常驻消费者**：不 fork、不 spawn 后台线程、不做 daemon/worker 自旋；
//!   任务推进只由显式命令 `world-gov queue step` 拉起（一步一推）。
//! - **复用 novel.db**：不新建库文件；`task_jobs` 幂等建表。
//! - **不发真实请求**：`queue step` 只做状态流转 + 沙盘模拟；真实推理留 TODO+bail!。
//! - 所有真实外呼点必经 `governance::get_semantic_gate()`（本模块不例外）。
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use super::pipeline::{JobKind, JobState, SemanticJob};

/// 幂等建表（启动自动 ensure；不 --persist 的路径不会调用，保持旧行为 100% 不变）。
pub fn ensure_task_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS task_jobs (
            id          TEXT PRIMARY KEY,
            kind        TEXT NOT NULL,
            slice_m     INTEGER NULL,
            slice_n     INTEGER NULL,
            volume      TEXT NULL,
            probe_path  TEXT NOT NULL DEFAULT '',
            state       TEXT NOT NULL,
            created_at  INTEGER NOT NULL,
            updated_at  INTEGER NOT NULL,
            meta_json   TEXT NOT NULL DEFAULT '{}'
        );
        CREATE INDEX IF NOT EXISTS idx_task_jobs_state ON task_jobs(state);
        CREATE INDEX IF NOT EXISTS idx_task_jobs_created ON task_jobs(created_at);",
    )?;
    Ok(())
}

fn now_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn kind_from_str(s: &str) -> Option<JobKind> {
    match s {
        "ScanLibrary" => Some(JobKind::ScanLibrary),
        "CheckOutline" => Some(JobKind::CheckOutline),
        "InspectCard" => Some(JobKind::InspectCard),
        _ => None,
    }
}

fn state_from_str(s: &str) -> Option<JobState> {
    match s {
        "Pending" => Some(JobState::Pending),
        "Probing" => Some(JobState::Probing),
        "ReadyForInferring" => Some(JobState::ReadyForInferring),
        "Done" => Some(JobState::Done),
        "Rejected" => Some(JobState::Rejected),
        _ => None,
    }
}

/// 存/更新一条任务（UPSERT）。`created_at` 仅在首次插入时写入。
pub fn job_persist(conn: &Connection, job: &SemanticJob, meta: Option<&Value>) -> Result<()> {
    let (sn, sm) = match job.slice {
        Some((n, m)) => (Some(n as i64), Some(m as i64)),
        None => (None, None),
    };
    let meta_s = meta
        .map(|m| m.to_string())
        .unwrap_or_else(|| "{}".to_string());
    let _ts = now_ts();
    conn.execute(
        "INSERT INTO task_jobs(id,kind,slice_n,slice_m,volume,probe_path,state,created_at,updated_at,meta_json)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?8,?9)
         ON CONFLICT(id) DO UPDATE SET
            kind=excluded.kind, slice_n=excluded.slice_n, slice_m=excluded.slice_m,
            volume=excluded.volume, probe_path=excluded.probe_path, state=excluded.state,
            updated_at=excluded.updated_at, meta_json=excluded.meta_json",
        params![
            job.id,
            job.kind.as_str(),
            sn,
            sm,
            job.volume,
            job.probe_path,
            job.state.as_str(),
            job.created as i64,
            meta_s
        ],
    )?;
    Ok(())
}

/// 任务是否已存在（跨进程幂等判定；只查 id，不读全行）。
pub fn job_exists(conn: &Connection, id: &str) -> Result<bool> {
    ensure_task_table(conn)?;
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM task_jobs WHERE id=?1",
        params![id],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// 按 id 加载一条任务。找不到返回 None；kind/state 非法（脏数据）也返回 None。
pub fn job_load(conn: &Connection, id: &str) -> Result<Option<SemanticJob>> {
    let row = conn
        .query_row(
            "SELECT id,kind,slice_n,slice_m,volume,probe_path,state,created_at
             FROM task_jobs WHERE id=?1",
            params![id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, i64>(7)?,
                ))
            },
        )
        .optional()?;
    let Some((id, kind_s, sn, sm, volume, probe_path, state_s, created)) = row else {
        return Ok(None);
    };
    let (Some(kind), Some(state)) = (kind_from_str(&kind_s), state_from_str(&state_s)) else {
        return Ok(None); // 脏数据：不 panic，视为不存在
    };
    let slice = match (sn, sm) {
        (Some(n), Some(m)) => Some((n as u32, m as u32)),
        _ => None,
    };
    Ok(Some(SemanticJob {
        id,
        kind,
        slice,
        volume,
        probe_path,
        created: created as u64,
        state,
    }))
}

/// 按状态筛选（`state=None` = 全部），按 created_at 升序。
pub fn job_list(conn: &Connection, state: Option<JobState>) -> Result<Vec<SemanticJob>> {
    let rows: Vec<(String, String, Option<i64>, Option<i64>, Option<String>, String, String, i64)> =
        match state {
            Some(st) => {
                let mut stmt = conn.prepare(
                    "SELECT id,kind,slice_n,slice_m,volume,probe_path,state,created_at
                     FROM task_jobs WHERE state=?1 ORDER BY created_at, id",
                )?;
                let it = stmt.query_map(params![st.as_str()], |r| {
                    Ok((
                        r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?,
                    ))
                })?;
                it.filter_map(|x| x.ok()).collect()
            }
            None => {
                let mut stmt = conn.prepare(
                    "SELECT id,kind,slice_n,slice_m,volume,probe_path,state,created_at
                     FROM task_jobs ORDER BY created_at, id",
                )?;
                let it = stmt.query_map([], |r| {
                    Ok((
                        r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?,
                    ))
                })?;
                it.filter_map(|x| x.ok()).collect()
            }
        };
    let mut out = Vec::new();
    for (id, kind_s, sn, sm, volume, probe_path, state_s, created) in rows {
        let (Some(kind), Some(state)) = (kind_from_str(&kind_s), state_from_str(&state_s)) else {
            continue;
        };
        let slice = match (sn, sm) {
            (Some(n), Some(m)) => Some((n as u32, m as u32)),
            _ => None,
        };
        out.push(SemanticJob {
            id,
            kind,
            slice,
            volume,
            probe_path,
            created: created as u64,
            state,
        });
    }
    Ok(out)
}

/// 删除单条任务（仅 task_jobs，不碰 risk_index / 资料库正文）。
pub fn job_delete(conn: &Connection, id: &str) -> Result<bool> {
    let n = conn.execute("DELETE FROM task_jobs WHERE id=?1", params![id])?;
    Ok(n > 0)
}

/// 清理超过 7 天的旧任务（安全清理：**只动 task_jobs**）。
/// 返回删除条数。
pub fn job_prune(conn: &Connection, older_than_days: u64) -> Result<usize> {
    let cutoff = now_ts() - (older_than_days as i64) * 86400;
    let n = conn.execute("DELETE FROM task_jobs WHERE created_at < ?1", params![cutoff])?;
    Ok(n)
}

/// 更新单条任务状态（持久层 + 合法性校验）。
pub fn job_set_state(conn: &Connection, id: &str, next: JobState) -> Result<JobState> {
    let cur = job_load(conn, id)?.ok_or_else(|| anyhow::anyhow!("未知 job id: {id}"))?;
    if !cur.state.can_transition_to(next) {
        anyhow::bail!(
            "非法状态流转：{} → {}",
            cur.state.as_str(),
            next.as_str()
        );
    }
    conn.execute(
        "UPDATE task_jobs SET state=?1, updated_at=?2 WHERE id=?3",
        params![next.as_str(), now_ts(), id],
    )?;
    Ok(next)
}

/// 向前推进**一步**（Pending→Probing→ReadyForInferring→Done）。
///
/// `ReadyForInferring` 步**经 [`super::semantic_runtime::run_semantic_infer`] 收口**：
/// - `Success` → findings → `Done`
/// - `Timeout`/`RetriesExhausted`/`ProtocolError` → `Rejected`（并回传失败说明，供上层写结构性风险）
/// - `GateLocked` → `Rejected`（门控锁死）
///
/// `gate_open` 由上层经 `governance::get_semantic_gate()` 求得；`dry_run=true` 走沙盘替身。
/// 其余步只做状态流转，无 findings。
pub fn job_step_forward(
    conn: &Connection,
    id: &str,
    gate_open: bool,
    dry_run: bool,
) -> Result<(JobState, Vec<Value>, Option<String>)> {
    let job = job_load(conn, id)?.ok_or_else(|| anyhow::anyhow!("未知 job id: {id}"))?;
    match job.state {
        JobState::Pending => {
            job_set_state(conn, id, JobState::Probing)?;
            Ok((JobState::Probing, Vec::new(), None))
        }
        JobState::Probing => {
            job_set_state(conn, id, JobState::ReadyForInferring)?;
            Ok((JobState::ReadyForInferring, Vec::new(), None))
        }
        JobState::ReadyForInferring => {
            // 组装请求（prompt 由探针侧提供；此处只承载 probe_path 摘要，不生成全文）
            let req = super::semantic_runtime::SemanticRequest {
                job_id: job.id.clone(),
                law_fingerprint: String::new(), // 由上层可注入真实指纹；本占位留空
                prompt_text: format!("probe:{}", job.probe_path),
                max_tokens: 2048,
                temperature: 0.2,
            };
            let cfg = super::semantic_runtime::SemanticEndpointConfig::from_env();
            let outcome = super::semantic_runtime::run_semantic_infer(&req, &cfg, gate_open, dry_run)?;
            match outcome {
                super::semantic_runtime::RuntimeOutcome::Success(resp) => {
                    let findings = super::semantic_runtime::findings_to_json(&resp.findings);
                    job_set_state(conn, id, JobState::Done)?;
                    Ok((JobState::Done, findings, None))
                }
                super::semantic_runtime::RuntimeOutcome::GateLocked => {
                    job_set_state(conn, id, JobState::Rejected)?;
                    Ok((JobState::Rejected, Vec::new(), Some("GATE_LOCKED: 语义门控锁死，未发起任何推理".into())))
                }
                super::semantic_runtime::RuntimeOutcome::Timeout => {
                    job_set_state(conn, id, JobState::Rejected)?;
                    Ok((JobState::Rejected, Vec::new(), Some("TIMEOUT: 推理超时".into())))
                }
                super::semantic_runtime::RuntimeOutcome::RetriesExhausted => {
                    job_set_state(conn, id, JobState::Rejected)?;
                    Ok((JobState::Rejected, Vec::new(), Some("RETRIES_EXHAUSTED: 重试耗尽".into())))
                }
                super::semantic_runtime::RuntimeOutcome::ProtocolError(e) => {
                    job_set_state(conn, id, JobState::Rejected)?;
                    Ok((JobState::Rejected, Vec::new(), Some(format!("PROTOCOL_ERROR: {e}"))))
                }
            }
        }
        JobState::Done | JobState::Rejected => {
            anyhow::bail!("任务 {id} 已是终态 {}，无法继续推进", job.state.as_str());
        }
    }
}

/// 任务表统计（供 queue list 概览）。
pub fn job_stats(conn: &Connection) -> Result<Value> {
    let mut by_state: Vec<Value> = Vec::new();
    for st in [
        JobState::Pending,
        JobState::Probing,
        JobState::ReadyForInferring,
        JobState::Done,
        JobState::Rejected,
    ] {
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM task_jobs WHERE state=?1",
            params![st.as_str()],
            |r| r.get(0),
        )?;
        by_state.push(json!({"state": st.as_str(), "count": n}));
    }
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM task_jobs", [], |r| r.get(0))?;
    Ok(json!({"total": total, "by_state": by_state}))
}

// ────────────────────────────────── 单测

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_job(id: &str, kind: JobKind, state: JobState) -> SemanticJob {
        SemanticJob {
            id: id.into(),
            kind,
            slice: Some((1, 4)),
            volume: None,
            probe_path: "07_勘误工坊/scan_probe_1of4.json".into(),
            created: now_ts() as u64,
            state,
        }
    }

    fn fresh_conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        ensure_task_table(&c).unwrap();
        c
    }

    #[test]
    fn ensure_task_table_is_idempotent() {
        let c = Connection::open_in_memory().unwrap();
        ensure_task_table(&c).unwrap();
        ensure_task_table(&c).unwrap(); // 第二次不应报错
        let n: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='task_jobs'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "幂等建表不重复创建");
    }

    #[test]
    fn persist_load_roundtrip() {
        let c = fresh_conn();
        let j = mk_job("j1", JobKind::ScanLibrary, JobState::ReadyForInferring);
        job_persist(&c, &j, None).unwrap();
        let got = job_load(&c, "j1").unwrap().unwrap();
        assert_eq!(got.kind, JobKind::ScanLibrary);
        assert_eq!(got.state, JobState::ReadyForInferring);
        assert_eq!(got.slice, Some((1, 4)));
        assert_eq!(got.probe_path, "07_勘误工坊/scan_probe_1of4.json");
    }

    #[test]
    fn persist_upsert_updates_state_and_keeps_created() {
        let c = fresh_conn();
        let mut j = mk_job("j1", JobKind::InspectCard, JobState::Pending);
        j.created = 1000;
        job_persist(&c, &j, None).unwrap();
        j.state = JobState::Probing;
        job_persist(&c, &j, None).unwrap();
        let got = job_load(&c, "j1").unwrap().unwrap();
        assert_eq!(got.state, JobState::Probing);
        assert_eq!(got.created, 1000, "created_at 首次写入后不再变");
        let cnt: i64 = c.query_row("SELECT COUNT(*) FROM task_jobs", [], |r| r.get(0)).unwrap();
        assert_eq!(cnt, 1, "UPSERT 不产生重复行");
    }

    #[test]
    fn load_missing_and_dirty_returns_none() {
        let c = fresh_conn();
        assert!(job_load(&c, "nope").unwrap().is_none());
        // 脏数据（非法 state）不 panic，返回 None
        c.execute(
            "INSERT INTO task_jobs(id,kind,state,created_at,updated_at) VALUES('bad','ScanLibrary','WAT',1,1)",
            [],
        )
        .unwrap();
        assert!(job_load(&c, "bad").unwrap().is_none(), "非法 state 视为不存在，不 panic");
    }

    #[test]
    fn list_filter_and_delete() {
        let c = fresh_conn();
        job_persist(&c, &mk_job("a", JobKind::ScanLibrary, JobState::Pending), None).unwrap();
        job_persist(&c, &mk_job("b", JobKind::CheckOutline, JobState::Done), None).unwrap();
        assert_eq!(job_list(&c, None).unwrap().len(), 2);
        let ready = job_list(&c, Some(JobState::Pending)).unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "a");
        assert!(job_delete(&c, "a").unwrap());
        assert!(!job_delete(&c, "a").unwrap(), "重复删除返回 false");
        assert_eq!(job_list(&c, None).unwrap().len(), 1);
    }

    #[test]
    fn step_forward_one_state_at_a_time() {
        let c = fresh_conn();
        job_persist(&c, &mk_job("j", JobKind::ScanLibrary, JobState::Pending), None).unwrap();
        // gate 关 + dry_run 关：ReadyForInferring 步会 GateLocked → Rejected
        let (s1, f1, r1) = job_step_forward(&c, "j", false, false).unwrap();
        assert_eq!(s1, JobState::Probing);
        assert!(f1.is_empty() && r1.is_none(), "非终态步不产 findings");
        let (s2, _, _) = job_step_forward(&c, "j", false, false).unwrap();
        assert_eq!(s2, JobState::ReadyForInferring);
        // 第 3 步：ReadyForInferring → 门控锁死 → Rejected
        let (s3, f3, r3) = job_step_forward(&c, "j", false, false).unwrap();
        assert_eq!(s3, JobState::Rejected, "门控锁死必须 Rejected");
        assert!(f3.is_empty());
        assert!(r3.unwrap().contains("GATE_LOCKED"));
        // 终态不可再推
        assert!(job_step_forward(&c, "j", false, false).is_err(), "终态推进必须报错，不能自旋");
    }

    #[test]
    fn step_gate_open_dryrun_reaches_done_with_mock() {
        let c = fresh_conn();
        job_persist(&c, &mk_job("j", JobKind::ScanLibrary, JobState::Pending), None).unwrap();
        job_step_forward(&c, "j", true, true).unwrap(); // → Probing
        job_step_forward(&c, "j", true, true).unwrap(); // → ReadyForInferring
        let (s3, findings, fail) = job_step_forward(&c, "j", true, true).unwrap(); // → Done（沙盘）
        assert_eq!(s3, JobState::Done);
        assert!(fail.is_none());
        assert_eq!(findings.len(), 2, "沙盘替身 2 条 findings");
        assert_eq!(findings[0]["verdict"], "OK");
        assert_eq!(findings[1]["verdict"], "UNCERTAIN");
    }

    #[test]
    fn step_gate_open_real_call_is_rejected_not_done() {
        // 门控开 + 非沙盘 → run_semantic_infer bail!(REAL_LLM_NOT_IMPLEMENTED) → 冒泡为 Err
        let c = fresh_conn();
        job_persist(&c, &mk_job("j", JobKind::InspectCard, JobState::ReadyForInferring), None).unwrap();
        let e = job_step_forward(&c, "j", true, false).unwrap_err();
        assert!(e.to_string().contains("REAL_LLM_NOT_IMPLEMENTED"));
        // 状态未被改成 Done（保持 ReadyForInferring）
        assert_eq!(job_load(&c, "j").unwrap().unwrap().state, JobState::ReadyForInferring);
    }

    #[test]
    fn prune_only_removes_old_jobs() {
        let c = fresh_conn();
        // 老任务（8 天前）
        c.execute(
            "INSERT INTO task_jobs(id,kind,state,created_at,updated_at) VALUES('old','ScanLibrary','Done',?1,?1)",
            params![now_ts() - 8 * 86400],
        )
        .unwrap();
        // 新任务
        job_persist(&c, &mk_job("new", JobKind::ScanLibrary, JobState::Done), None).unwrap();
        let removed = job_prune(&c, 7).unwrap();
        assert_eq!(removed, 1, "只清 7 天前");
        let ids: Vec<String> = job_list(&c, None).unwrap().into_iter().map(|j| j.id).collect();
        assert_eq!(ids, vec!["new"], "新任务保留");
    }

    #[test]
    fn state_machine_rejects_illegal_transition() {
        let c = fresh_conn();
        job_persist(&c, &mk_job("j", JobKind::ScanLibrary, JobState::Done), None).unwrap();
        assert!(job_set_state(&c, "j", JobState::Pending).is_err(), "终态不可回退");
        assert!(job_set_state(&c, "nope", JobState::Probing).is_err(), "未知 id 报错");
    }

    #[test]
    fn persist_isolated_from_memory_queue() {
        // --persist 落库 vs 纯内存：两者互不干扰
        let c = fresh_conn();
        job_persist(&c, &mk_job("dbjob", JobKind::ScanLibrary, JobState::Pending), None).unwrap();
        // 内存队列不含该任务
        let mem_has = super::super::pipeline::with_queue(|q| q.peek("dbjob").is_some());
        assert!(!mem_has, "持久任务不得出现在内存队列");
        assert_eq!(job_list(&c, None).unwrap().len(), 1, "db 任务表独立存在");
    }

    #[test]
    fn job_exists_detects_presence_for_cross_process_idempotency() {
        let c = fresh_conn();
        assert!(!job_exists(&c, "job-abc").unwrap(), "不存在 → false");
        job_persist(&c, &mk_job("job-abc", JobKind::InspectCard, JobState::Pending), None).unwrap();
        assert!(job_exists(&c, "job-abc").unwrap(), "已存在 → true（幂等基础）");
        // 再次 UPSERT 同 id：不新增行（任务表不膨胀）
        job_persist(&c, &mk_job("job-abc", JobKind::InspectCard, JobState::Pending), None).unwrap();
        assert_eq!(job_list(&c, None).unwrap().len(), 1, "同 id UPSERT 不膨胀");
    }
}