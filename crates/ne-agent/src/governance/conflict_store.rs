//! 冲突记录库（world-gov，第 13 批）—— 存「AI 给出的观点」，人是终审。
//!
//! # 定位
//! 把每一次语义调用的**结构化结果**可追溯地落库：探针 key、快照锚点、法理指纹、
//! 原始 prompt / raw response、token 计数、人工复核状态。
//!
//! # 存储（复用 novel.db，与主索引隔离）
//! `conflict_records(id, probe_key, snap_id, baseline_fp, ts, verdict_json, raw_prompt,
//!   raw_response, token_in, token_out, review_status)`
//! - `review_status` 枚举：`pending` | `valid` | `false_alarm` | `ignored`（对接第 10 批人工复核）。
//!
//! # 铁则
//! - ❌ 不直接改资料库；只存「AI 观点」。
//! - ✅ 幂等建表；`CREATE TABLE IF NOT EXISTS`。
//! - ✅ 可反向给条目打 `ai-flagged` 标签（仅供人复核，不自动改内容）。
use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

/// 复核状态枚举。
pub const REVIEW_STATUSES: &[&str] = &["pending", "valid", "false_alarm", "ignored"];
/// AI 自动打的标签（供人复核用）。
pub const AI_FLAG_TAG: &str = "ai-flagged";

const SCHEMA: &str = r#"
-- 冲突记录库（2026-10-14，第 13 批）—— 幂等；与主索引隔离。
CREATE TABLE IF NOT EXISTS conflict_records (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    probe_key     TEXT NOT NULL,
    snap_id       TEXT,
    baseline_fp   TEXT NOT NULL DEFAULT '',
    ts            INTEGER NOT NULL,
    verdict_json  TEXT NOT NULL DEFAULT '{}',
    raw_prompt    TEXT NOT NULL DEFAULT '',
    raw_response  TEXT NOT NULL DEFAULT '',
    token_in      INTEGER NOT NULL DEFAULT 0,
    token_out     INTEGER NOT NULL DEFAULT 0,
    review_status TEXT NOT NULL DEFAULT 'pending'
);
CREATE INDEX IF NOT EXISTS idx_conflict_review ON conflict_records(review_status, probe_key);
"#;

/// 幂等建表。
pub fn ensure_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA)?;
    Ok(())
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// 校验复核状态。
pub fn validate_review(s: &str) -> Result<String> {
    let s = s.trim();
    if !REVIEW_STATUSES.contains(&s) {
        bail!("未知复核状态 '{}'（合法：{}）", s, REVIEW_STATUSES.join(" | "));
    }
    Ok(s.to_string())
}

/// 一条新冲突记录（写入前）。
#[derive(Debug, Clone)]
pub struct NewConflict {
    pub probe_key: String,
    pub snap_id: Option<String>,
    pub baseline_fp: String,
    pub verdict: Value,
    pub raw_prompt: String,
    pub raw_response: String,
    pub token_in: i64,
    pub token_out: i64,
}

/// 插入一条冲突记录，返回新 id。
pub fn insert(conn: &Connection, c: &NewConflict) -> Result<i64> {
    ensure_table(conn)?;
    let verdict_json = serde_json::to_string(&c.verdict)?;
    conn.execute(
        "INSERT INTO conflict_records(probe_key,snap_id,baseline_fp,ts,verdict_json,raw_prompt,raw_response,token_in,token_out,review_status)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'pending')",
        params![c.probe_key, c.snap_id, c.baseline_fp, now_secs(), verdict_json, c.raw_prompt, c.raw_response, c.token_in, c.token_out],
    )?;
    Ok(conn.last_insert_rowid())
}

fn row_to_json(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    let verdict_json: String = r.get(5)?;
    let verdict: Value = serde_json::from_str(&verdict_json).unwrap_or(json!({}));
    Ok(json!({
        "id": r.get::<_, i64>(0)?,
        "probe_key": r.get::<_, String>(1)?,
        "snap_id": r.get::<_, Option<String>>(2)?,
        "baseline_fp": r.get::<_, String>(3)?,
        "ts": r.get::<_, i64>(4)?,
        "verdict": verdict,
        "token_in": r.get::<_, i64>(6)?,
        "token_out": r.get::<_, i64>(7)?,
        "review_status": r.get::<_, String>(8)?,
    }))
}

/// 列出冲突记录（可按 review_status / probe_key 过滤；不含 raw 大字段）。
pub fn list(conn: &Connection, status: Option<&str>, probe_key: Option<&str>) -> Result<Vec<Value>> {
    ensure_table(conn)?;
    let st = match status {
        Some(s) if !s.trim().is_empty() => Some(validate_review(s)?),
        _ => None,
    };
    let pk = probe_key.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let mut sql = String::from(
        "SELECT id,probe_key,snap_id,baseline_fp,ts,verdict_json,token_in,token_out,review_status FROM conflict_records WHERE 1=1",
    );
    let mut idx = 1;
    let mut binds: Vec<String> = Vec::new();
    if let Some(s) = &st {
        sql.push_str(&format!(" AND review_status=?{idx}"));
        idx += 1;
        binds.push(s.clone());
    }
    if let Some(k) = &pk {
        sql.push_str(&format!(" AND probe_key=?{idx}"));
        binds.push(k.clone());
    }
    sql.push_str(" ORDER BY id DESC");
    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<Value> = match binds.len() {
        0 => stmt.query_map([], row_to_json)?.filter_map(|x| x.ok()).collect(),
        1 => stmt.query_map(params![binds[0]], row_to_json)?.filter_map(|x| x.ok()).collect(),
        _ => stmt.query_map(params![binds[0], binds[1]], row_to_json)?.filter_map(|x| x.ok()).collect(),
    };
    Ok(rows)
}

/// 取单条（含 raw_prompt / raw_response）。返回 (json, prompt, response)。
pub fn show(conn: &Connection, id: i64) -> Result<Option<(Value, String, String)>> {
    ensure_table(conn)?;
    let r: Option<(i64, String, Option<String>, String, i64, String, String, String, i64, i64, String)> = conn
        .query_row(
            "SELECT id,probe_key,snap_id,baseline_fp,ts,verdict_json,raw_prompt,raw_response,token_in,token_out,review_status
             FROM conflict_records WHERE id=?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?)),
        )
        .optional()?;
    match r {
        Some((id, k, snap, fp, ts, vj, rp, rr, ti, to, rs)) => {
            let verdict: Value = serde_json::from_str(&vj).unwrap_or(json!({}));
            let j = json!({
                "id": id, "probe_key": k, "snap_id": snap, "baseline_fp": fp, "ts": ts,
                "verdict": verdict, "token_in": ti, "token_out": to, "review_status": rs,
                "raw_prompt_chars": rp.chars().count(), "raw_response_chars": rr.chars().count(),
            });
            Ok(Some((j, rp, rr)))
        }
        None => Ok(None),
    }
}

/// 人工复核：更新 review_status。可附带给 probe_key 打 `ai-flagged`（打标签由调用方决定）。
pub fn set_review(conn: &Connection, id: i64, status: &str) -> Result<bool> {
    ensure_table(conn)?;
    let st = validate_review(status)?;
    let n = conn.execute("UPDATE conflict_records SET review_status=?1 WHERE id=?2", params![st, id])?;
    Ok(n > 0)
}

/// 反向给条目打 `ai-flagged`（复用第 10 批标签；仅供人复核）。
pub fn flag_probe_key_as_ai(conn: &Connection, probe_key: &str) -> Result<usize> {
    let (ins, _) = crate::governance::note_tag_store::add_tags(
        conn,
        probe_key,
        &[AI_FLAG_TAG.to_string()],
        "manual",
    )?;
    Ok(ins)
}

/// 统计（供 audit-export / 面板）。
pub fn summary(conn: &Connection) -> Result<Value> {
    ensure_table(conn)?;
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM conflict_records", [], |r| r.get(0)).unwrap_or(0);
    let mut by: serde_json::Map<String, Value> = serde_json::Map::new();
    for s in REVIEW_STATUSES {
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM conflict_records WHERE review_status=?1", params![s], |r| r.get(0))
            .unwrap_or(0);
        by.insert((*s).to_string(), json!(n));
    }
    Ok(json!({"total": total, "by_review_status": by}))
}

/// 供 self-check：结构 + 脏状态检查（只读）。
pub fn table_check(conn: &Connection) -> Result<Value> {
    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='conflict_records'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if exists == 0 {
        return Ok(json!({"ok": false, "missing_table": "conflict_records",
            "hint": "跑任意 world-gov 命令（open_gov 幂等建表）即可创建"}));
    }
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM conflict_records", [], |r| r.get(0)).unwrap_or(0);
    let mut dirty: Vec<Value> = Vec::new();
    let mut stmt = conn.prepare("SELECT id,review_status,probe_key FROM conflict_records")?;
    let it = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
    for x in it.filter_map(|v| v.ok()) {
        if !REVIEW_STATUSES.contains(&x.1.as_str()) {
            dirty.push(json!({"id": x.0, "review_status": x.1, "probe_key": x.2, "why": "bad_review_status"}));
        }
    }
    Ok(json!({"ok": dirty.is_empty(), "records": total, "dirty": dirty}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        ensure_table(&c).unwrap();
        c
    }

    fn sample() -> NewConflict {
        NewConflict {
            probe_key: "CHAR-10-001".into(),
            snap_id: Some("abc123".into()),
            baseline_fp: "fp01".into(),
            verdict: json!({"verdict": "HIGH-RISK", "law_ref": "L0原话"}),
            raw_prompt: "P".into(),
            raw_response: "R".into(),
            token_in: 100,
            token_out: 20,
        }
    }

    #[test]
    fn table_isolated_and_idempotent() {
        let c = Connection::open_in_memory().unwrap();
        ensure_table(&c).unwrap();
        ensure_table(&c).unwrap();
        let names: Vec<String> = c
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .filter_map(|v| v.ok())
            .collect();
        assert!(names.contains(&"conflict_records".to_string()));
        assert!(!names.contains(&"library_index".to_string()));
    }

    #[test]
    fn insert_list_show_review() {
        let c = mem();
        let id = insert(&c, &sample()).unwrap();
        assert!(id > 0);
        let rows = list(&c, None, None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["probe_key"], "CHAR-10-001");
        assert_eq!(rows[0]["review_status"], "pending");
        assert_eq!(rows[0]["verdict"]["verdict"], "HIGH-RISK");
        // show 含 raw
        let (j, p, r) = show(&c, id).unwrap().unwrap();
        assert_eq!(j["probe_key"], "CHAR-10-001");
        assert_eq!(p, "P");
        assert_eq!(r, "R");
        // review
        assert!(set_review(&c, id, "valid").unwrap());
        let rows = list(&c, Some("valid"), None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(list(&c, Some("pending"), None).unwrap().len(), 0);
        assert!(set_review(&c, id, "nope").is_err());
        assert!(!set_review(&c, 9999, "valid").unwrap());
    }

    #[test]
    fn list_filters_by_probe_key() {
        let c = mem();
        insert(&c, &sample()).unwrap();
        let mut s2 = sample();
        s2.probe_key = "LOC-99-001".into();
        insert(&c, &s2).unwrap();
        assert_eq!(list(&c, None, Some("CHAR-10-001")).unwrap().len(), 1);
        assert_eq!(list(&c, None, None).unwrap().len(), 2);
    }

    #[test]
    fn flag_probe_key_as_ai_tags() {
        let c = mem();
        crate::governance::note_tag_store::ensure_tables(&c).unwrap();
        let n = flag_probe_key_as_ai(&c, "CHAR-10-001").unwrap();
        assert_eq!(n, 1);
        let tags = crate::governance::note_tag_store::list_tags(&c, Some("CHAR-10-001"), None).unwrap();
        assert_eq!(tags[0]["tag"], AI_FLAG_TAG);
        // 幂等：再打不新增
        assert_eq!(flag_probe_key_as_ai(&c, "CHAR-10-001").unwrap(), 0);
    }

    #[test]
    fn summary_counts_by_status() {
        let c = mem();
        insert(&c, &sample()).unwrap();
        let id2 = insert(&c, &sample()).unwrap();
        set_review(&c, id2, "false_alarm").unwrap();
        let s = summary(&c).unwrap();
        assert_eq!(s["total"], 2);
        assert_eq!(s["by_review_status"]["pending"], 1);
        assert_eq!(s["by_review_status"]["false_alarm"], 1);
    }

    #[test]
    fn table_check_detects_dirty() {
        let c = mem();
        insert(&c, &sample()).unwrap();
        assert_eq!(table_check(&c).unwrap()["ok"], true);
        c.execute("UPDATE conflict_records SET review_status='garbage'", []).unwrap();
        let r = table_check(&c).unwrap();
        assert_eq!(r["ok"], false);
        assert_eq!(r["dirty"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn table_check_missing() {
        let c = Connection::open_in_memory().unwrap();
        let r = table_check(&c).unwrap();
        assert_eq!(r["ok"], false);
        assert_eq!(r["missing_table"], "conflict_records");
    }
}