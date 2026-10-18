//! 任务提交（P4）—— **高危操作只生成指令，转发 MCP 执行**。
//!
//! # 任务书约束（读写分离）
//! > 批量 LLM 推演、大规模管线运算属于高危操作，UI 仅提交任务，交由 MCP 校验三重锁后执行
//!
//! 因此本模块**只做**：
//! 1. 校验双重门控是否满足（不满足 → 拒绝，UI 按钮置灰）
//! 2. 生成任务指令记录（落 `webui_task` 表）+ 审计
//! 3. 提供 list / result 查询
//!
//! **不做**：不真正跑管线、不调模型。真正执行由 MCP（或 CLI）读取该指令后自行推进。
use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use serde_json::{json, Value};

use super::category_store;

pub fn ensure_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS webui_task (
    id          TEXT PRIMARY KEY,
    kind        TEXT NOT NULL,
    payload     TEXT NOT NULL DEFAULT '{}',
    state       TEXT NOT NULL DEFAULT 'pending',
    gate_l1     INTEGER NOT NULL DEFAULT 0,
    gate_l2     INTEGER NOT NULL DEFAULT 0,
    created_ts  INTEGER NOT NULL DEFAULT (strftime('%s','now')),
    updated_ts  INTEGER NOT NULL DEFAULT (strftime('%s','now')),
    result      TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_webui_task_ts ON webui_task(created_ts DESC);
-- 任务书 #6：失败归档（多次重试/修复回合失败后落此）
CREATE TABLE IF NOT EXISTS webui_task_fail (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id     TEXT NOT NULL,
    kind        TEXT NOT NULL,
    reason      TEXT NOT NULL DEFAULT '',
    attempts    INTEGER NOT NULL DEFAULT 0,
    last_raw    TEXT NOT NULL DEFAULT '',
    created_ts  INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);
CREATE INDEX IF NOT EXISTS idx_webui_task_fail_ts ON webui_task_fail(created_ts DESC);
"#,
    )?;
    Ok(())
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
/// 纳秒时间戳 + 进程内原子序号，保证同一秒内多次提交也不撞主键。
fn new_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("wt-{nanos:x}-{seq:x}")
}

/// 允许的任务类型（白名单，防任意命令注入）。
/// `chapter_gen`（2026-10-17 新增）：L5 章节生成派发——**不走语义门控**，
/// 提交即 `pending`；真正出网与否由 ne-mcp 的 `/sdcard/wogov_real_llm.allow` 文件锁在**执行时**判定。
const KINDS: [&str; 10] = [
    "scan", "pipeline", "semantic_pipeline", "outline_rebuild",
    "inspect_batch", "cardgen", "self_check", "custom", "chapter_gen",
    // `sync`（2026-10-17 P0 修）：面板 webui_panel.html 的「落库同步」按钮
    // 一直发 kind='sync'，但白名单里从来没有它 → 点一下必得「kind 不在白名单」。
    // 补进白名单让它至少能登记（真正执行仍由执行器决定）。
    "sync",
];

/// 是否免语义门控（提交即 pending），改由 `/sdcard/wogov_real_llm.allow` 文件锁在**执行时**管。
///
/// - `chapter_gen`：L5 写出正文
/// - `cardgen`（2026-10-17 P0 修）：批量建卡。它和 L5 是同一类"写类操作"，
///   守的是同一把文件锁（`cardgen.py` 的 `call_llm()` 里也校验 ALLOW）。
///   早先只放行 `chapter_gen`，导致面板提交 cardgen 走 `l1 && l2` 语义双重门控——
///   那套需要 `SEMANTIC_ENABLE_GATE && --semantic && NOVEL_SEMANTIC_UNLOCK=yes`，
///   日常是关的，于是建卡按钮永远是灰的。口径错配，此处纠正。
pub fn is_file_gated(kind: &str) -> bool {
    matches!(kind, "chapter_gen" | "cardgen")
}

/// 双重门控状态（两套都要满足才算解锁）。
pub fn gate_state() -> (bool, bool) {
    let l1 = super::get_semantic_gate(true);
    let compile_feature = super::llm_gate::compile_feature_enabled();
    let env_allow = super::llm_gate::env_allowed();
    let l2 = super::llm_gate::triple_lock(compile_feature, env_allow, true);
    (l1, l2)
}

/// 提交任务（**只登记指令**，不执行）。
pub fn submit(conn: &Connection, body: &str) -> Result<Value> {
    ensure_tables(conn)?;
    let v: Value = serde_json::from_str(body.trim()).unwrap_or(json!({}));
    let kind = v["kind"].as_str().unwrap_or("").trim().to_string();
    if kind.is_empty() {
        bail!("需要 kind（{}）", KINDS.join("/"));
    }
    if !KINDS.contains(&kind.as_str()) {
        bail!("kind 不在白名单：{kind}");
    }
    let payload = v.get("payload").cloned().unwrap_or(json!({}));
    let (l1, l2) = gate_state();
    let file_gated = is_file_gated(&kind);
    // chapter_gen：文件锁在执行时（ne-mcp）判定，提交即 pending；其余仍走语义双重门控。
    let unlocked = if file_gated { true } else { l1 && l2 };
    let id = new_id();
    conn.execute(
        "INSERT INTO webui_task(id,kind,payload,state,gate_l1,gate_l2) VALUES(?1,?2,?3,?4,?5,?6)",
        params![
            id, kind, payload.to_string(),
            if unlocked { "pending" } else { "blocked" },
            l1 as i64, l2 as i64
        ],
    )?;
    category_store::log_op(
        conn, "task.submit", &id,
        &format!("kind={kind} gate_l1={l1} gate_l2={l2} file_gated={file_gated}"), "",
    )?;
    if !unlocked {
        return Ok(json!({
            "ok": false, "id": id, "kind": kind,
            "state": "blocked",
            "gate_l1": l1, "gate_l2": l2,
            "error": "双重门控未同时满足，任务已登记但不会被执行（UI 应置灰按钮）",
            "hint": "① 第一道闸需 SEMANTIC_ENABLE_GATE(编译期) && --semantic && NOVEL_SEMANTIC_UNLOCK=yes；\
                     ② 外呼锁需 feature(real-semantic|llm_integration) && WOGOV_ALLOW_REAL_LLM=yes && --real-semantic",
        }));
    }
    Ok(json!({
        "ok": true, "id": id, "kind": kind, "state": "pending",
        "gate_l1": l1, "gate_l2": l2,
        "file_gated": file_gated,
        "note": if file_gated {
            if kind == "cardgen" {
                "cardgen：已登记为待执行指令，不受语义门控；真实外呼由执行器拉起 cardgen.py 时检查 /sdcard/wogov_real_llm.allow 文件锁（real=false 时空跑不落盘）。"
            } else {
                "chapter_gen：已登记为待执行指令，不受语义门控；真实外呼由 ne-mcp 执行时检查 /sdcard/wogov_real_llm.allow 文件锁。"
            }
        } else {
            "已登记为待执行指令；真正执行由 MCP / CLI 读取该指令后推进（WebUI 不执行）。"
        },
    }))
}

/// 把 cardgen 的 spec 拆成「人能看懂的卡片清单」。
///
/// spec 格式（由提交端约定）：`类别:数量:卡名1|卡名2|卡名3`
/// 例如：`功法:3:太玄九转引气诀·炼气篇|太玄剑典|太玄护山阵诀`
/// 也有省略数量的写法：`文明:1,势力:2,地点:2`（混沌帝国那条）
///
/// 返回值直接给前端渲染，前端不再需要理解 spec 语法：
///   { "kind_cn": "功法", "items": ["太玄剑典", ...], "count": 3 }
fn parse_cardgen_spec(spec: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for seg in spec.split(',') {
        let seg = seg.trim();
        if seg.is_empty() {
            continue;
        }
        // 三段式：类别:数量:卡名列表
        let parts: Vec<&str> = seg.splitn(3, ':').collect();
        if parts.len() == 3 {
            let kind = parts[0].trim();
            let n: i64 = parts[1].trim().parse().unwrap_or(0);
            let names: Vec<String> = parts[2]
                .split('|')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            out.push(json!({
                "kind_cn": kind,
                "count": if names.is_empty() { n } else { names.len() as i64 },
                "items": names,
            }));
        } else if parts.len() == 2 {
            // 两段式：类别:数量（没给具体卡名，只能给数量）
            let kind = parts[0].trim();
            let n: i64 = parts[1].trim().parse().unwrap_or(0);
            out.push(json!({ "kind_cn": kind, "count": n, "items": [] }));
        }
    }
    out
}

/// 任务列表。
pub fn list(conn: &Connection, limit: usize) -> Result<Value> {
    ensure_tables(conn)?;
    let lim = limit.clamp(1, 500) as i64;
    // 把 payload 也取出来：前端要把「任务编号」翻译成「要建哪几张卡」。
    // 只取 payload 而仍然截断 result（result 可能很长），避免响应变大。
    let mut st = conn.prepare(
        "SELECT id,kind,state,gate_l1,gate_l2,created_ts,updated_ts,\
                CASE WHEN result='' THEN '' ELSE substr(result,1,2000) END,\
                payload \
         FROM webui_task ORDER BY created_ts DESC LIMIT ?1",
    )?;
    let rows: Vec<Value> = st
        .query_map(params![lim], |r| {
            Ok((
                r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?, r.get::<_, i64>(4)?, r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?, r.get::<_, String>(7)?, r.get::<_, String>(8)?,
            ))
        })?
        .filter_map(|x| x.ok())
        .map(|(id, kind, state, g1, g2, cts, uts, rp, payload)| {
            let pv: Value = serde_json::from_str(&payload).unwrap_or(json!({}));
            let civ = pv.get("civ").and_then(|x| x.as_str()).unwrap_or("");
            let spec = pv.get("spec").and_then(|x| x.as_str()).unwrap_or("");
            let groups = parse_cardgen_spec(spec);
            // 「人话摘要」：一句能直接看懂的说明，前端优先显示它
            let summary = if kind == "cardgen" {
                if groups.is_empty() {
                    if civ.is_empty() {
                        "建卡任务".to_string()
                    } else {
                        format!("{civ} · 建卡")
                    }
                } else {
                    let parts: Vec<String> = groups
                        .iter()
                        .map(|g| {
                            let k = g.get("kind_cn").and_then(|x| x.as_str()).unwrap_or("");
                            let n = g.get("count").and_then(|x| x.as_i64()).unwrap_or(0);
                            format!("{k} {n} 张")
                        })
                        .collect();
                    if civ.is_empty() {
                        parts.join(" · ")
                    } else {
                        format!("{civ} · {}", parts.join(" · "))
                    }
                }
            } else if kind == "chapter_gen" {
                let vol = pv.get("volume").and_then(|x| x.as_str()).unwrap_or("");
                let ch = pv.get("chapter").and_then(|x| x.as_i64()).unwrap_or(0);
                if vol.is_empty() {
                    "写正文".to_string()
                } else if ch > 0 {
                    format!("{vol} 第 {ch} 章 · 写正文")
                } else {
                    format!("{vol} · 写正文")
                }
            } else {
                kind.clone()
            };
            // ── 实时进度：从 result JSON 里抽 msg / current_name / done / total ──
            let rj: Value = serde_json::from_str(&rp).unwrap_or(json!({}));
            let prog = json!({
                "msg": rj.get("msg").and_then(|x| x.as_str()).unwrap_or(""),
                "current": rj.get("current").and_then(|x| x.as_str()).unwrap_or(""),
                "current_name": rj.get("current_name").and_then(|x| x.as_str()).unwrap_or(""),
                "current_category": rj.get("current_category").and_then(|x| x.as_str()).unwrap_or(""),
                "phase": rj.get("phase").and_then(|x| x.as_str()).unwrap_or(""),
                                "done": rj.get("done").and_then(|x| x.as_i64()).unwrap_or(0),
                "total": rj.get("total").and_then(|x| x.as_i64()).unwrap_or(0),
                "failed": rj.get("failed").and_then(|x| x.as_i64()).unwrap_or(0),
            });
            json!({
                "id": id,
                "kind": kind,
                "state": state,
                "progress": prog,
                "gate_l1": g1 != 0,
                "gate_l2": g2 != 0,
                "created_ts": cts,
                "updated_ts": uts,
                "result_preview": rp,
                // ── 2026-10-17 新增：给人看的字段 ──
                "civ": civ,
                "summary": summary,
                "card_groups": groups,
                "real": pv.get("real").and_then(|x| x.as_bool()).unwrap_or(false),
            })
        })
        .collect();
    let (l1, l2) = gate_state();
    Ok(json!({
        "command": "webui tasks list",
        "count": rows.len(), "tasks": rows,
        "gate_l1": l1, "gate_l2": l2, "unlocked": l1 && l2,
    }))
}

/// 单任务结果。
pub fn result(conn: &Connection, id: &str) -> Result<Value> {
    ensure_tables(conn)?;
    let row: Option<(String, String, String, String, i64, i64, String)> = conn
        .query_row(
            "SELECT kind,state,payload,result,gate_l1,gate_l2,id FROM webui_task WHERE id=?1",
            params![id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
        )
        .optional()?;
    let Some((kind, state, payload, result, g1, g2, tid)) = row else {
        bail!("任务不存在：{id}");
    };
    Ok(json!({
        "command": "webui task result",
        "id": tid, "kind": kind, "state": state,
        "gate_l1": g1 != 0, "gate_l2": g2 != 0,
        "payload": serde_json::from_str::<Value>(&payload).unwrap_or(json!({})),
        "result": result,
        "note": "result 由执行方（MCP/CLI）回写；WebUI 只读展示。",
    }))
}

/// 任务失败归档（任务书 #6）：多次重试/修复回合失败后落档，
/// 并把原任务置为 failed，供 WebUI 队列面板展示终止原因。
pub fn archive_failure(
    conn: &Connection,
    task_id: &str,
    kind: &str,
    reason: &str,
    attempts: u32,
    last_raw: &str,
) -> Result<Value> {
    ensure_tables(conn)?;
    let raw_digest = super::llm_obs::digest_of(last_raw, 500);
    conn.execute(
        "INSERT INTO webui_task_fail(task_id,kind,reason,attempts,last_raw) VALUES(?1,?2,?3,?4,?5)",
        params![task_id, kind, reason, attempts as i64, raw_digest],
    )?;
    let _ = conn.execute(
        "UPDATE webui_task SET state='failed', updated_ts=?1, result=?2 WHERE id=?3",
        params![now(), format!("FAILED: {reason} (attempts={attempts})"), task_id],
    );
    category_store::log_op(conn, "task.failed", task_id,
        &format!("kind={kind} attempts={attempts} reason={reason}"), "")?;
    Ok(json!({
        "ok": true, "task_id": task_id, "kind": kind,
        "reason": reason, "attempts": attempts,
        "note": "失败已归档；WebUI 任务队列可查看终止原因。",
    }))
}

/// 失败归档列表（供面板展示）。
pub fn failures(conn: &Connection, limit: usize) -> Result<Value> {
    ensure_tables(conn)?;
    let lim = limit.clamp(1, 200) as i64;
    let mut st = conn.prepare(
        "SELECT id,task_id,kind,reason,attempts,last_raw,created_ts FROM webui_task_fail ORDER BY id DESC LIMIT ?1",
    )?;
    let rows: Vec<Value> = st.query_map(params![lim], |r| Ok(json!({
        "id": r.get::<_, i64>(0)?,
        "task_id": r.get::<_, String>(1)?,
        "kind": r.get::<_, String>(2)?,
        "reason": r.get::<_, String>(3)?,
        "attempts": r.get::<_, i64>(4)?,
        "last_raw": r.get::<_, String>(5)?,
        "created_ts": r.get::<_, i64>(6)?,
    })))?.filter_map(|x| x.ok()).collect();
    Ok(json!({"command": "webui tasks failures", "count": rows.len(), "failures": rows}))
}

/// 终止未运行任务（pending → canceled）。
pub fn cancel(conn: &Connection, id: &str) -> Result<Value> {
    ensure_tables(conn)?;
    let n = conn.execute(
        "UPDATE webui_task SET state='canceled', updated_ts=?1 WHERE id=?2 AND state IN ('pending','blocked')",
        params![now(), id],
    )?;
    category_store::log_op(conn, "task.cancel", id, &format!("rows={n}"), "")?;
    Ok(json!({"ok": n > 0, "id": id, "canceled": n}))
}

/// 结果回写（2026-10-17 · 路径A）：由执行方（ToolPkg / MCP）执行完后调用。
/// body: `{ "state": "done"|"rejected"|"failed", "result": "<文本>", "dispatched": bool }`
/// - state 缺省按 `dispatched` 推：true→done，false→rejected。
pub fn update_result(conn: &Connection, id: &str, body: &str) -> Result<Value> {
    ensure_tables(conn)?;
    let v: Value = serde_json::from_str(body.trim()).unwrap_or(json!({}));
    let dispatched = v.get("dispatched").and_then(|x| x.as_bool());
    let state = v.get("state").and_then(|x| x.as_str()).map(|s| s.to_string())
        .unwrap_or_else(|| if dispatched == Some(true) { "done".into() } else { "rejected".into() });
    if !["done", "rejected", "failed", "running"].contains(&state.as_str()) {
        bail!("state 非法：{state}（允许 done/rejected/failed/running）");
    }
    let result = v.get("result").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let n = conn.execute(
        "UPDATE webui_task SET state=?1, result=?2, updated_ts=?3 WHERE id=?4",
        params![state, result, now(), id],
    )?;
    category_store::log_op(conn, "task.result", id, &format!("state={state} rows={n}"), "")?;
    // ── SSE 广播：每次 result 回写都推一条 ──
    // 这是「保底通道」：即使 Python 侧细粒度日志没接上，
    // 前端也能通过 state 变化 + progress 字段知道进展。
    let prog = serde_json::from_str::<Value>(&result).unwrap_or(json!({}));
    super::task_events::publish("task_state", json!({
        "id": id,
        "state": state,
        "rows": n,
        "msg": prog.get("msg").and_then(|x| x.as_str()).unwrap_or(""),
        "current": prog.get("current").and_then(|x| x.as_str()).unwrap_or(""),
        "current_name": prog.get("current_name").and_then(|x| x.as_str()).unwrap_or(""),
        "current_category": prog.get("current_category").and_then(|x| x.as_str()).unwrap_or(""),
        "phase": prog.get("phase").and_then(|x| x.as_str()).unwrap_or(""),
        "done": prog.get("done").and_then(|x| x.as_i64()).unwrap_or(0),
        "total": prog.get("total").and_then(|x| x.as_i64()).unwrap_or(0),
        "failed": prog.get("failed").and_then(|x| x.as_i64()).unwrap_or(0),
        "results": prog.get("results").cloned().unwrap_or(json!([])),
        "civ": prog.get("civ").and_then(|x| x.as_str()).unwrap_or(""),
    }));
    Ok(json!({
        "ok": n > 0, "id": id, "state": state, "rows": n,
        "note": if n > 0 { "已回写" } else { "任务不存在" },
    }))
}

/// 单条事件推送（**只广播、不写库**）。
///
/// 用途：Python 侧 exec_pending.py 逐行读到 cardgen 的日志后，
/// 调 `POST /api/tasks/{id}/event` 把细粒度日志（每个 token / 每张卡开始）
/// 实时推给 SSE，延迟约 100ms。
///
/// 为什么不写库：细粒度日志可能每秒几十条，写 SQLite 会拖慢建卡主链路。
/// 「保底」由 update_result 负责（它写 result 字段），这里只做实时通道。
/// 前端断线重连后，从 /api/tasks/{id}/result 拉回完整进度即可。
pub fn push_event(_conn: &Connection, id: &str, body: &str) -> Result<Value> {
    let v: Value = serde_json::from_str(body.trim()).unwrap_or(json!({}));
    let kind = v.get("type").and_then(|x| x.as_str()).unwrap_or("log");
    let mut data = v.get("data").cloned().unwrap_or(json!({}));
    // 强制带上任务 id，前端才能按任务过滤
    if let Some(obj) = data.as_object_mut() {
        obj.insert("id".to_string(), json!(id));
    }
    // ── 【2026-10-17 全流式】thinking / content 双通道 ──
    // 这两类事件是 LLM 原生 token 流（reasoning + content），
    // 与 log（进程 stdout）语义不同：
    //   · log     = 程序日志（"⚠ 模型 502，正在重试"）
    //   · thinking= 模型的思考过程（"用户问的是…核心答案：瑞利散射"）
    //   · content = 模型正在写的正文（逐字打字机效果）
    // 前端据此分到不同面板：日志面板 / 🧠 折叠思考区 / ✍️ 正文区。
    super::task_events::publish(kind, data);
    Ok(json!({ "ok": true, "id": id, "type": kind }))
}

/// 读取某任务的思考轨迹与正文（供面板刷新后回放）。
///
/// 数据源是 cardgen.py 写的 `task_stream` 表。为什么不用 webui_task.result：
/// `list()` 里对 result 做了 `substr(result,1,2000)`，一张卡的思考+正文
/// 动辄上万字，塞进去必被截断成残片。
///
/// `code` 为空时返回该任务最近的全部卡片（最多 50 张）。
pub fn list_stream(conn: &Connection, id: &str, code: &str) -> Result<Value> {
    // 表可能还不存在（老库/首次运行），先兜一层建表，避免 500
    let _ = conn.execute(
        "CREATE TABLE IF NOT EXISTS task_stream(\
            task_id TEXT NOT NULL, code TEXT NOT NULL, name TEXT, category TEXT,\
            model TEXT, thinking TEXT, content TEXT, think_chars INTEGER,\
            content_chars INTEGER, ts INTEGER, PRIMARY KEY(task_id, code))",
        [],
    );
    let mut out: Vec<Value> = Vec::new();
    let sql_all = "SELECT code,name,category,model,thinking,content,think_chars,\
                   content_chars,ts FROM task_stream WHERE task_id=?1 \
                   ORDER BY ts DESC LIMIT 50";
    let sql_one = "SELECT code,name,category,model,thinking,content,think_chars,\
                   content_chars,ts FROM task_stream WHERE task_id=?1 AND code=?2 LIMIT 5";
    let mut push = |row: &rusqlite::Row| -> rusqlite::Result<()> {
        out.push(json!({
            "code": row.get::<_, String>(0).unwrap_or_default(),
            "name": row.get::<_, String>(1).unwrap_or_default(),
            "category": row.get::<_, String>(2).unwrap_or_default(),
            "model": row.get::<_, String>(3).unwrap_or_default(),
            "thinking": row.get::<_, String>(4).unwrap_or_default(),
            "content": row.get::<_, String>(5).unwrap_or_default(),
            "think_chars": row.get::<_, i64>(6).unwrap_or(0),
            "content_chars": row.get::<_, i64>(7).unwrap_or(0),
            "ts": row.get::<_, i64>(8).unwrap_or(0),
        }));
        Ok(())
    };
    {
        let mut st = conn.prepare(if code.is_empty() { sql_all } else { sql_one })?;
        if code.is_empty() {
            let mut rows = st.query(params![id])?;
            while let Some(r) = rows.next()? {
                push(r)?;
            }
        } else {
            let mut rows = st.query(params![id, code])?;
            while let Some(r) = rows.next()? {
                push(r)?;
            }
        }
    }
    Ok(json!({ "ok": true, "id": id, "code": code, "count": out.len(), "items": out }))
}

// ══════════════════════════════════════════════════════════════════
// 执行器派发（2026-10-17 UI 重构 · P0 修）
//
// # 为什么需要这个
// 改造前面板的「触发执行」按钮实际是**反向操作**：
//   jpost('/api/tasks/{id}/result', {state:'running'})
// 把任务从 pending 改成 running，而 exec_pending.py 只扫 pending
// → 用户越点「触发执行」，任务越不可能被执行。永久僵死。
//
// 本模块提供真正的派发：spawn `python3 exec_pending.py <limit>`，立即返回。
// 不改任务状态（保持 pending 让执行器扫到），状态由执行器回写。
//
// # 单线程网关下的注意点
// `serve()` 是 `for stream in listener.incoming() { handle_conn(...) }`，
// 单线程阻塞。所以这里**必须** spawn 不能 output()，否则整块面板卡死。
// ══════════════════════════════════════════════════════════════════

/// 执行器的全局单飞锁（存在且有活 pid = 已有执行器在跑）。
///
/// 与 cardgen.py 的 `/sdcard/wogov_cardgen.lock` 是两个不同层级：
///   · wogov_exec.lock     —— 执行器进程级（一个 exec_pending.py 实例）
///   · wogov_cardgen.lock  —— 建卡任务级（一个 cardgen.py 子进程）
/// 执行器本来就不会并发跑（limit 串行），再多一层是防用户连点。
pub const EXEC_LOCK: &str = "/sdcard/wogov_exec.lock";

/// 执行器脚本位置（相对库根之外的固定路径 —— 它属于 Operit ToolPkg 侧）。
const EXEC_SCRIPT: &str = "/sdcard/Download/Operit/brain_system/scripts/exec_pending.py";

/// 读 `/proc/<pid>/stat` 的第 22 字段（进程启动时刻，单位 clock tick）。
///
/// ## 为什么必须用它（2026-10-17 事故根治）
/// 只靠 pid 判定"锁是谁的"有致命缺陷：**pid 会被操作系统复用**。
/// 实测事故：App 重启后旧执行器 pid 被新进程复用，新执行器看到
/// `锁内 pid = 9320，我也是 9320` → 判定"锁是我的"直接放行 → 两个执行器
/// 同时扫同一批 pending → 太玄帝国 10 张卡被顶掉，一条都没生成。
///
/// `(pid, starttime)` 组合才是 Linux 下真正的唯一进程标识：pid 复用了，
/// starttime 必然不同（除非同 tick 内复用，概率可忽略）。
///
/// ## 解析要点
/// `/proc/<pid>/stat` 格式：`pid (comm) state ppid ...`，其中 comm 可能含
/// 空格和括号（例如 `(python3 worker)`），所以必须用 `rfind(')')` 之后再
/// 切分，不能简单 split_whitespace。
/// `rest[0]` = state（第 3 字段），starttime 是第 22 字段 → `rest[19]`。
fn proc_starttime(pid: u32) -> Option<u64> {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rp = raw.rfind(')')?;
    let rest: Vec<&str> = raw[rp + 1..].split_whitespace().collect();
    // 字段 n 对应 rest[n-3]（rest[0] 已是第 3 字段 state）。
    // starttime 是第 22 字段 → rest[19]。
    rest.get(19)?.parse::<u64>().ok()
}

/// 本进程的锁标识 token：`pid:starttime`。
fn self_lock_token() -> String {
    let me = std::process::id();
    match proc_starttime(me) {
        Some(st) => format!("{me}:{st}"),
        None => format!("{me}:0"),
    }
}

/// 把锁文件内容解析成 `(pid, starttime)`。兼容旧的纯 pid 格式。
///
/// 旧格式 `9320`         → `(9320, None)`
/// 新格式 `9320:5404793` → `(9320, Some(5404793))`
fn read_lock_token(p: &Path) -> Option<(u32, Option<u64>)> {
    let raw = std::fs::read_to_string(p).ok()?.trim().to_string();
    if raw.is_empty() {
        return None;
    }
    match raw.split_once(':') {
        Some((a, b)) => {
            let pid = a.trim().parse::<u32>().ok()?;
            let st = b.trim().parse::<u64>().ok().filter(|v| *v != 0);
            Some((pid, st))
        }
        None => raw.parse::<u32>().ok().map(|pid| (pid, None)),
    }
}

/// 判活：pid 存在 **且**（若已知 starttime）启动时刻一致。
///
/// 兼容旧锁（starttime 为 None）：只能按 pid 判，这是历史遗留的弱判据，
/// 下次写锁时会被升级成新格式。
fn proc_alive_token(pid: u32, starttime: Option<u64>) -> bool {
    if !Path::new(&format!("/proc/{pid}")).exists() {
        return false;
    }
    match starttime {
        None => true,
        // 进程在但读不到 starttime → 保守判活（宁可拒绝重复启动，不冒并发风险）
        Some(st) => proc_starttime(pid).map(|cur| cur == st).unwrap_or(true),
    }
}

/// 派发执行器。`limit` = 本轮最多跑几个任务（默认 1）。
///
/// 返回：{ok, dispatched, pid, already_running, log, note}
/// 失败不 bail（前端要好处理），用 ok=false + reason 表达。
pub fn dispatch_executor(lib_root: &Path, limit: u32) -> Result<Value> {
    use std::process::{Command, Stdio};
    // 上限 10 → 100（2026-10-17）：原先点一次只派 1 条、上限 10 条，
    // 「一键把队列跑完」不成立——19 条任务得点 19 次。
    // 建卡是串行长任务，放宽上限不会并发抢号（执行器内部就是 for 循环逐条跑）。
    let lim = limit.clamp(1, 100);

    // ① 单飞锁：已有执行器在跑就直接返回（不报错，告诉前端"已在跑"）
    let lock = Path::new(EXEC_LOCK);
    if lock.exists() {
        if let Some((pid, st)) = read_lock_token(lock) {
            if proc_alive_token(pid, st) {
                return Ok(json!({
                    "ok": true,
                    "dispatched": false,
                    "already_running": true,
                    "pid": pid,
                    "limit": lim,
                    "note": format!("已有执行器在跑（pid={pid}），本次不重复启动。"),
                }));
            }
        }
        // 僵尸锁：进程已死 / starttime 已不匹配（pid 被复用）→ 清掉重拿
        let _ = std::fs::remove_file(lock);
    }

    // ② 脚本存在性检查
    let script = Path::new(EXEC_SCRIPT);
    if !script.exists() {
        return Ok(json!({
            "ok": false,
            "dispatched": false,
            "reason": format!("执行器脚本不存在：{EXEC_SCRIPT}"),
            "hint": "确认 Operit brain_system 的 scripts 目录是否就位。",
        }));
    }

    // ③ 日志（子进程输出绝不能进 HTTP 流）
    let log_dir = lib_root.join("07_勘误工坊").join("cardgen_logs");
    let _ = std::fs::create_dir_all(&log_dir);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let log_path = log_dir.join(format!("exec_{stamp}.log"));
    let out = std::fs::File::create(&log_path).ok();
    let err = std::fs::File::create(log_dir.join(format!("exec_{stamp}.err"))).ok();
    let (stdout_cfg, stderr_cfg) = match (out, err) {
        (Some(o), Some(e)) => (Stdio::from(o), Stdio::from(e)),
        _ => (Stdio::null(), Stdio::null()),
    };

    // ④ spawn（不 wait！）
    let mut cmd = Command::new("python3");
    cmd.arg(script)
        .arg(lim.to_string())
        .current_dir(lib_root)
        .stdout(stdout_cfg)
        .stderr(stderr_cfg);
    match cmd.spawn() {
        Ok(child) => {
            let pid = child.id();
            // 【2026-10-17 根治】锁内容从裸 pid 改为 `pid:starttime` 复合 token。
            //
            // 子进程（exec_pending.py）自己也会抢锁，它算出的 token 必然是
            // `pid:starttime` 且 pid 就是这里的 child.id()、starttime 读的是同一个
            // /proc/<pid>/stat —— 两边内容天然一致，不会互相判成"别人"。
            //
            // 不无条件覆写的原因：spawn 到 write 之间子进程可能已经秒退、并被
            // 另一个执行器拿到锁；此时 blind write 会踩掉别人的锁。所以只在
            // 「锁不存在」或「锁内 pid 就是本子进程」时才写。
            let token = format!("{pid}:{}", proc_starttime(pid).map(|v| v.to_string()).unwrap_or_else(|| "0".into()));
            let need_write = match read_lock_token(lock) {
                None => true,
                Some((p, _)) => p == pid,
            };
            if need_write {
                let _ = std::fs::write(lock, token.as_bytes());
            }
            Ok(json!({
                "ok": true,
                "dispatched": true,
                "already_running": false,
                "pid": pid,
                "lock_token": token,
                "limit": lim,
                "log": log_path.display().to_string(),
                "note": "执行器已启动（后台跑，不阻塞面板）。状态会自动回写到任务列表。",
            }))
        }
        Err(e) => Ok(json!({
            "ok": false,
            "dispatched": false,
            "reason": format!("spawn 执行器失败：{e}"),
            "hint": "python3 是否在 ne-agent 的 PATH？",
        })),
    }
}

/// 活跃任务摘要（供右下角胶囊 3 秒轮询）。
///
/// 刻意做得**极轻**：只跑一次 GROUP BY + 取最近 5 条未完成任务。
/// 不用 list()，因为那个会返回最多 500 条并把 result 截断到 200 字。
pub fn active_summary(conn: &Connection) -> Result<Value> {
    ensure_tables(conn)?;
    // 各状态计数
    let mut counts = serde_json::Map::new();
    {
        let mut st = conn.prepare("SELECT state, COUNT(*) FROM webui_task GROUP BY state")?;
        let rows = st.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in rows.flatten() {
            counts.insert(row.0, json!(row.1));
        }
    }
    let n = |k: &str| counts.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
    let pending = n("pending");
    let running = n("running");

    // 最近 5 条**真正在跑**的任务。
    //
    // 【2026-10-17 修】早先这里含 'blocked'，导致一个后果：
    // blocked = 提交时被语义门控挡下的任务，永远不会被执行（执行器只扫 pending），
    // 但 active_count 把它算进去 → 右下角胶囊永远显示"任务进行中"在转圈，
    // 用户以为系统在干活，实际什么都没发生。blocked 只在看板"被锁"筛选里展示。
    let mut items: Vec<Value> = Vec::new();
    {
        let mut st = conn.prepare(
            "SELECT id, kind, state, payload, created_ts, updated_ts, \
                    CASE WHEN result='' THEN '' ELSE substr(result,1,1500) END \
             FROM webui_task \
             WHERE state IN ('pending','running') \
             ORDER BY created_ts DESC LIMIT 5",
        )?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?,
                r.get::<_, String>(3)?, r.get::<_, i64>(4)?, r.get::<_, i64>(5)?,
                r.get::<_, String>(6)?,
            ))
        })?;
        for row in rows.flatten() {
            let (id, kind, state, payload, cts, uts, result) = row;
            let pv: Value = serde_json::from_str(&payload).unwrap_or(json!({}));
            // cardgen 的进度写在 result 的 JSON 里
            let prog: Value = serde_json::from_str(&result).unwrap_or(Value::Null);
            items.push(json!({
                "id": id, "kind": kind, "state": state,
                "civ": pv.get("civ").cloned().unwrap_or(json!("")),
                "volume": pv.get("volume").cloned().unwrap_or(json!("")),
                "chapter": pv.get("chapter").cloned().unwrap_or(json!(0)),
                "real": pv.get("real").and_then(|x| x.as_bool()).unwrap_or(false),
                "created_ts": cts, "updated_ts": uts,
                // cardgen 有增量进度就直接透出，前端画进度条
                "total": prog.get("total").cloned().unwrap_or(json!(0)),
                "done": prog.get("done").cloned().unwrap_or(json!(0)),
                "success": prog.get("success").cloned().unwrap_or(json!(0)),
                "failed": prog.get("failed").cloned().unwrap_or(json!(0)),
                "current": prog.get("current").cloned().unwrap_or(json!("")),
            }));
        }
    }

    // 最近一条已完成（供"刚跑完"的绿胶囊）
    let last_done: Option<(String, String, i64)> = conn
        .query_row(
            "SELECT id, state, updated_ts FROM webui_task \
             WHERE state IN ('done','failed','rejected') \
             ORDER BY updated_ts DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();

    // 执行器存活判定同样走 pid+starttime，避免把「pid 已被复用」误判成活着。
    let exec_pid = read_lock_token(Path::new(EXEC_LOCK))
        .filter(|(p, st)| proc_alive_token(*p, *st))
        .map(|(p, _)| p);

    Ok(json!({
        "pending": pending,
        "running": running,
        "active_count": pending + running,
        "totals": counts,
        "items": items,
        "last_done": last_done.map(|(id, st, ts)| json!({"id": id, "state": st, "ts": ts})),
        "executor_alive": exec_pid.is_some(),
        "executor_pid": exec_pid,
        "llm_unlocked": Path::new(super::real_llm_lock::ALLOW_FILE).exists(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        ensure_tables(&c).unwrap();
        category_store::ensure_tables(&c).unwrap();
        c
    }

    #[test]
    fn rejects_unknown_kind() {
        let c = mk();
        assert!(submit(&c, r#"{"kind":"rm -rf /"}"#).is_err());
        assert!(submit(&c, "{}").is_err());
    }

    #[test]
    fn submit_blocked_when_locked() {
        let c = mk();
        let r = submit(&c, r#"{"kind":"scan","payload":{"volume":"卷01"}}"#).unwrap();
        // 默认双重门控都锁死 → 应 blocked
        assert_eq!(r["state"], "blocked");
        assert_eq!(r["ok"], false);
        let l = list(&c, 10).unwrap();
        assert_eq!(l["count"], 1);
    }

    #[test]
    fn cancel_works() {
        let c = mk();
        let id = submit(&c, r#"{"kind":"scan"}"#).unwrap()["id"].as_str().unwrap().to_string();
        assert!(cancel(&c, &id).unwrap()["ok"].as_bool().unwrap());
    }
}