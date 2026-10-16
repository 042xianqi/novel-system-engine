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

/// 允许的任务类型（白名单，防任意命令注入）。
const KINDS: [&str; 8] = [
    "scan", "pipeline", "semantic_pipeline", "outline_rebuild",
    "inspect_batch", "cardgen", "self_check", "custom",
];

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
    let unlocked = l1 && l2;
    let id = format!(
        "wt-{}-{:06}",
        now(),
        (now() % 1000000) as u32
    );
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
        &format!("kind={kind} gate_l1={l1} gate_l2={l2}"), "",
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
        "note": "已登记为待执行指令；真正执行由 MCP / CLI 读取该指令后推进（WebUI 不执行）。",
    }))
}

/// 任务列表。
pub fn list(conn: &Connection, limit: usize) -> Result<Value> {
    ensure_tables(conn)?;
    let lim = limit.clamp(1, 500) as i64;
    let mut st = conn.prepare(
        "SELECT id,kind,state,gate_l1,gate_l2,created_ts,updated_ts,\
                CASE WHEN result='' THEN '' ELSE substr(result,1,200) END \
         FROM webui_task ORDER BY created_ts DESC LIMIT ?1",
    )?;
    let rows: Vec<Value> = st
        .query_map(params![lim], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "kind": r.get::<_, String>(1)?,
                "state": r.get::<_, String>(2)?,
                "gate_l1": r.get::<_, i64>(3)? != 0,
                "gate_l2": r.get::<_, i64>(4)? != 0,
                "created_ts": r.get::<_, i64>(5)?,
                "updated_ts": r.get::<_, i64>(6)?,
                "result_preview": r.get::<_, String>(7)?,
            }))
        })?
        .filter_map(|x| x.ok())
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