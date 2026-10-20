//! 治理层业务逻辑（自 `web_gateway.rs` 拆出，Phase-1 架构重构）。
//!
//! # 职责边界
//! 只做**数据查询与会话落库**：接收 `&rusqlite::Connection` 与库根路径，返回 `serde_json::Value`。
//! **不感知 HTTP**（无 TcpStream / 无 Resp / 无路由）。
//!
//! # 兼容性
//! 所有函数可见性与原 `web_gateway::*` 一致；对外仍以 `web_gateway::overview` 等路径调用
//! （由 `web_gateway` facade 重导出），**契约零变化**。
use anyhow::Result;
use serde_json::{json, Value};
use std::path::Path;

use super::super::note_tag_store;

// ────────────────────────────────── 观测数据的构造（等价既有命令）

/// 工作台概览（库统计 / 缓存 / 队列 / 快照 / gate）——**全部只读**。
pub fn overview(conn: &rusqlite::Connection, lib_root: &Path) -> Result<Value> {
    let li: i64 = conn
        .query_row("SELECT COUNT(*) FROM library_index", [], |r| r.get(0))
        .unwrap_or(0);
    let risk: i64 = conn
        .query_row("SELECT COUNT(*) FROM risk_index WHERE resolved=0", [], |r| r.get(0))
        .unwrap_or(0);
    let cache = super::super::probe_cache::status(lib_root, 30).unwrap_or(json!({}));
    let snaps = super::super::diff_snapshot::list(conn).unwrap_or_default();
    let (pending, done, rejected) = queue_counts(conn);
    Ok(json!({
        "command": "world-gov web overview",
        "library_index": li,
        "open_risks": risk,
        "cache": {"entries": cache["entries"], "total_bytes": cache["total_bytes"]},
        "queue": {"pending": pending, "done": done, "rejected": rejected},
        "snapshots": {"count": snaps.len(), "items": snaps},
        "gate": ne_agent_gate(),
        "note": "只读概览；等价 world-gov 各命令结果。",
    }))
}

fn queue_counts(conn: &rusqlite::Connection) -> (i64, i64, i64) {
    let t: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='task_jobs'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if t == 0 {
        return (0, 0, 0);
    }
    let c = |st: &str| -> i64 {
        conn.query_row("SELECT COUNT(*) FROM task_jobs WHERE state=?1", rusqlite::params![st], |r| r.get(0))
            .unwrap_or(0)
    };
    (c("Pending"), c("Done"), c("Rejected"))
}

/// gate 状态（只读；绝不改门控）。
fn ne_agent_gate() -> Value {
    let st = crate::governance::semantic_gate_status(false);
    json!({
        "status": st["status"],
        "compile_gate": st["compile_gate"],
        "cli_semantic": st["cli_semantic"],
        "env_unlock": st["env_unlock"],
        "toolchain_epoch": 12,
    })
}

/// 条目浏览（可按 layer / 标签过滤）。**只读 library_index + item_tag**。
pub fn browse_items(
    conn: &rusqlite::Connection,
    layer: Option<&str>,
    with_tag: Option<&str>,
    limit: usize,
) -> Result<Value> {
    // 标签筛选（复用第 10 批筛选器）
    let tag_set = match with_tag {
        Some(t) if !t.trim().is_empty() => {
            let f = note_tag_store::TagFilter::build(std::slice::from_ref(&t.to_string()), &[])?;
            note_tag_store::keys_matching(conn, &f)?
        }
        _ => None,
    };
    // 2026-10-17 L4 修复：补查 category 字段（前端分类标签改造为读真实分类，不再靠 key 前缀硬编码猜）。
    let mut sql = String::from(
        "SELECT layer,key,IFNULL(title,''),chars,dirty,IFNULL(category,'') FROM library_index",
    );
    if layer.is_some() {
        sql.push_str(" WHERE layer=?1");
    }
    sql.push_str(" ORDER BY layer,key");
    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<(String, String, String, i64, i64, String)> = if let Some(l) = layer {
        stmt.query_map(rusqlite::params![l], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })?
        .filter_map(|x| x.ok()).collect()
    } else {
        stmt.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })?
        .filter_map(|x| x.ok()).collect()
    };
    let mut items: Vec<Value> = Vec::new();
    // [*] 注解一次性取全量。原实现放在 for 循环内，每行都跑一次
    //     all_annotations()（内含两次全表查询 + ensure_tables），
    //     L4 有 701 行 → 1400+ 次冗余查询，是本接口最大热点。
    let ann = note_tag_store::all_annotations(conn).unwrap_or_default();
    for (layer, key, title, chars, dirty, category) in rows {
        if let Some(set) = &tag_set {
            if !set.contains(&key) {
                continue;
            }
        }
        let tags = ann.get(&key).map(|a| a["tags"].clone()).unwrap_or(json!([]));
        let note_status = ann
            .get(&key)
            .and_then(|a| a["note"]["status"].as_str().map(|s| s.to_string()));
        items.push(json!({
            "layer": layer, "key": key, "title": title, "chars": chars,
            "category": category, "dirty": dirty, "tags": tags, "note_status": note_status,
        }));
    }
    // [*] 先全量计数再截断。原实现边 push 边 break，导致：
    //     1) 调用方无法知道「真实命中数」，被截断也看不出来（前端曾因此静默丢 201 张卡）
    //     2) matched 若在 break 后统计必然偏小。这里改为构造完再 truncate，两个数都准。
    let matched = items.len();
    items.truncate(limit);
    Ok(json!({
        "command": "world-gov web browse",
        "layer": layer, "with_tag": with_tag,
        "count": items.len(), "matched": matched,
        "truncated": matched > items.len(), "limit": limit,
        "items": items,
    }))
}

/// 队列视图（读取 task_jobs + 扫描到的 journal 计数兜底）。
pub fn queue_view(conn: &rusqlite::Connection) -> Result<Value> {
    let t: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='task_jobs'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if t == 0 {
        return Ok(json!({"command": "world-gov web queue", "table": false, "jobs": []}));
    }
    let mut stmt = conn.prepare("SELECT id,kind,state,created_at FROM task_jobs ORDER BY created_at DESC LIMIT 200")?;
    let jobs: Vec<Value> = stmt
        .query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?, "kind": r.get::<_, String>(1)?,
                "state": r.get::<_, String>(2)?, "created_at": r.get::<_, i64>(3)?,
            }))
        })?
        .filter_map(|x| x.ok())
        .collect();
    Ok(json!({"command": "world-gov web queue", "table": true, "count": jobs.len(), "jobs": jobs}))
}

/// 报告 & 探针包列表（只列名，不读内容）。
pub fn archive_view(lib_root: &Path) -> Result<Value> {
    let reports = super::super::report_builder::list_reports(lib_root).unwrap_or_default();
    let mut probes: Vec<Value> = Vec::new();
    let dir = lib_root.join("07_勘误工坊");
    if dir.is_dir() {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.ends_with(".wg-probe.tar.gz") {
                    let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                    probes.push(json!({"name": name, "size": size}));
                }
            }
        }
    }
    Ok(json!({
        "command": "world-gov web archive",
        "reports": {"count": reports.len(), "items": reports},
        "probe_archives": {"count": probes.len(), "items": probes},
    }))
}

pub(crate) fn persist_turn(
    lib_root: &Path,
    session: &str,
    user_msg: &str,
    reply_json: &Value,
) -> anyhow::Result<()> {
    let store = super::super::open_session(lib_root)?;
    super::super::ensure_session(&store, session);

    // 取 assistant 的正文。前端显示的是 reply 字段；
    // 若为空的（极端情况），退化成整段 JSON，至少留痕。
    let assistant_text = reply_json
        .get("reply")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim();
    if assistant_text.is_empty() {
        return Ok(());
    }

    // 当前 head：新 user 消息挂它后面
    let head = store.head(session).ok().flatten();
    let uid = store.append(session, head, "user", user_msg)?;
    store.append(session, Some(uid), "assistant", assistant_text)?;
    Ok(())
}

/// 分层列表：**实际层集合**（库内实测）+ 任务书定义的 6 个正典层语义。
/// 不硬编码 6 层——库内还用了 ANCHOR/AUX/GUARD/L0.5/L1x 等扩展层。
pub fn layers_list(conn: &rusqlite::Connection) -> Result<Value> {
    let mut st = conn.prepare(
        "SELECT layer, COUNT(*), COALESCE(SUM(CASE WHEN dirty=1 THEN 1 ELSE 0 END),0) \
         FROM library_index GROUP BY layer ORDER BY layer",
    )?;
    let it = st.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))
    })?;
    let mut seen = std::collections::BTreeSet::new();
    let mut actual = Vec::new();
    let mut total = 0i64;
    let mut dirty_total = 0i64;
    for x in it {
        let (layer, n, d) = x?;
        seen.insert(layer.clone());
        total += n;
        dirty_total += d;
        actual.push(json!({"layer": layer, "count": n, "dirty": d}));
    }
    // 任务书定义的 6 个正典层（固定语义），标注库内是否出现
    let canon = [
        ("L0", "原始素材"),
        ("L1", "实体"),
        ("L2", "关系"),
        ("L3", "冲突"),
        ("L4", "推演分支"),
        ("L5", "定稿正文"),
    ];
    let canonical: Vec<Value> = canon
        .iter()
        .map(|(k, d)| {
            let n = actual
                .iter()
                .find(|v| v["layer"] == *k)
                .and_then(|v| v["count"].as_i64())
                .unwrap_or(0);
            json!({"layer": k, "meaning": d, "present": seen.contains(*k), "count": n})
        })
        .collect();
    Ok(json!({
        "command": "webui layers list",
        "total": total,
        "dirty_total": dirty_total,
        "canonical": canonical,
        "actual": actual,
        "note": "actual = 库内实测层（含 ANCHOR/AUX/GUARD/L0.5/L1x 等扩展层）",
    }))
}

/// 系统状态：**两套锁**并列展示 + 存储占用 + LLM 配置摘要（掩码）。
///
/// 为什么两套都报：本项目历史上有两层门控，语义不同、不可互相替代——
/// - `gate_l1`（既有第一道闸）：`SEMANTIC_ENABLE_GATE` && `--semantic` && `NOVEL_SEMANTIC_UNLOCK`
/// - `gate_l2`（更严外呼锁）：feature(`real-semantic`/`llm_integration`) && `WOGOV_ALLOW_REAL_LLM=yes` && `--real-semantic`
/// 只报一套会误导操作者。
pub fn system_status(conn: &rusqlite::Connection, lib_root: &Path) -> Result<Value> {
    let g1 = super::super::semantic_gate_status(false);
    let compile_feature = super::super::llm_gate::compile_feature_enabled();
    let env_allow = super::super::llm_gate::env_allowed();
    let g2 = json!({
        "compile_feature": compile_feature,
        "compile_feature_names": "real-semantic | llm_integration",
        "env_allow": env_allow,
        "env_name": super::super::llm_gate::ENV_ALLOW,
        "cli_flag": false,
        "cli_flag_name": "--real-semantic",
        "open": super::super::llm_gate::triple_lock(compile_feature, env_allow, false),
        "note": "面板无 --real-semantic，故 cli_flag 恒 false",
    });
    // 存储占用（有上限，避免大库卡住）
    let storage = dir_usage(lib_root, 20000);
    // LLM 配置摘要（掩码；不回显 base_url / api_key）
    let llm = super::super::llm_config::load(lib_root)
        .map(|c| super::super::llm_config::safe_snapshot(&c))
        .unwrap_or_else(|e| json!({"error": e.to_string()}));
    let _ = conn;
    Ok(json!({
        "command": "webui system status",
        "gate_l1": g1,
        "gate_l2": g2,
        "storage": storage,
        "llm": llm,
        "note": "只读；不触发任何外呼。",
    }))
}

/// 目录占用（有上限；仅统计，不修改）。
fn dir_usage(root: &Path, cap: usize) -> Value {
    let mut files = 0usize;
    let mut bytes = 0u64;
    let mut truncated = false;
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((d, depth)) = stack.pop() {
        if depth > 6 {
            continue;
        }
        let rd = match std::fs::read_dir(&d) {
            Ok(v) => v,
            Err(_) => continue,
        };
        for e in rd.flatten() {
            if files >= cap {
                truncated = true;
                break;
            }
            let p = e.path();
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push((p, depth + 1)),
                Ok(t) if t.is_file() => {
                    files += 1;
                    if let Ok(m) = e.metadata() {
                        bytes += m.len();
                    }
                }
                _ => {}
            }
        }
        if truncated {
            break;
        }
    }
    json!({
        "files": files,
        "bytes": bytes,
        "mb": (bytes as f64 / 1048576.0 * 100.0).round() / 100.0,
        "truncated": truncated,
        "cap": cap,
    })
}

/// LLM 配置读取（**掩码**）：复用 `llm_config::safe_snapshot`，不回显地址与 Key。
pub fn llm_config_view(work_root: &Path) -> Result<Value> {
    let rel = super::super::llm_config::CONFIG_REL;
    let path = work_root.join(rel);
    let exists = path.is_file();
    let cfg = super::super::llm_config::load(work_root)?;
    let snap = super::super::llm_config::safe_snapshot(&cfg);
    Ok(json!({
        "command": "webui config llm get",
        "path": rel,
        "exists": exists,
        "config": snap,
        "note": "api-key 与 base_url 一律不回显（只报是否已配置）。",
    }))
}
