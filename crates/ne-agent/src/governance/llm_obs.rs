//! LLM 可观测性与熔断（world-gov，第 17 批）。
//!
//! # 记录
//! 每次调用落 `llm_call_log`：backend / model / token in-out / 耗时 / 缓存命中 / 结果。
//! 全局累计走聚合查询；`world-gov llm stats` 读它。
//!
//! # 熔断
//! 连续失败 N 次（默认 5）→ `llm_runtime.tripped_until_ts` 置为未来时间，期间**拒绝出网**
//! （退化为静态模式并告警），到期自动恢复。**不做无限重试**。
//!
//! # 隔离
//! 两张表均为 world-gov 隔离辅助表；与主索引、资料库完全无关。
use anyhow::Result;
use rusqlite::{params, Connection};
use serde_json::{json, Value};

const SCHEMA: &str = r#"
-- LLM 可观测性（2026-10-14，第 17 批）—— 幂等；与主索引隔离。
CREATE TABLE IF NOT EXISTS llm_call_log (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    ts         INTEGER NOT NULL,
    backend    TEXT NOT NULL DEFAULT '',
    model      TEXT NOT NULL DEFAULT '',
    token_in   INTEGER NOT NULL DEFAULT 0,
    token_out  INTEGER NOT NULL DEFAULT 0,
    elapsed_ms INTEGER NOT NULL DEFAULT 0,
    cache_hit  INTEGER NOT NULL DEFAULT 0,
    ok         INTEGER NOT NULL DEFAULT 1,
    note       TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_llm_log_ts ON llm_call_log(ts);
CREATE TABLE IF NOT EXISTS llm_runtime (
    id               INTEGER PRIMARY KEY CHECK (id=1),
    concurrent_fail  INTEGER NOT NULL DEFAULT 0,
    tripped_until_ts INTEGER NOT NULL DEFAULT 0,
    total_calls      INTEGER NOT NULL DEFAULT 0,
    total_fail       INTEGER NOT NULL DEFAULT 0
);
"#;

pub fn ensure_table(conn: &Connection) -> Result<()> {
    // 任务书 #5：幂等补列（旧库升级友好）
    let _ = conn.execute_batch(
        "ALTER TABLE llm_call_log ADD COLUMN source TEXT DEFAULT '';
         ALTER TABLE llm_call_log ADD COLUMN actor TEXT DEFAULT '';
         ALTER TABLE llm_call_log ADD COLUMN lock_state TEXT DEFAULT '';
         ALTER TABLE llm_call_log ADD COLUMN req_digest TEXT DEFAULT '';
         ALTER TABLE llm_call_log ADD COLUMN resp_digest TEXT DEFAULT '';",
    );
    conn.execute_batch(SCHEMA)?;
    Ok(())
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// 一次调用的记录项。
#[derive(Debug, Clone)]
pub struct CallRecord {
    pub backend: String,
    pub model: String,
    pub token_in: i64,
    pub token_out: i64,
    pub elapsed_ms: i64,
    pub cache_hit: bool,
    pub ok: bool,
    pub note: String,
    // ── 任务书 #5：审计补全字段（additive，默认空/0，兼容既有构造点）
    /// 调用来源：webui / mcp / cli / internal
    pub source: String,
    /// 触发者：human（手动） / ai（自动流水线）
    pub actor: String,
    /// 锁状态摘要（如 "locked"/"open"）
    pub lock_state: String,
    /// 入参摘要（截断 200 字）
    pub req_digest: String,
    /// 返回摘要（截断 200 字）
    pub resp_digest: String,
}

impl Default for CallRecord {
    fn default() -> Self {
        CallRecord {
            backend: String::new(), model: String::new(), token_in: 0, token_out: 0,
            elapsed_ms: 0, cache_hit: false, ok: false, note: String::new(),
            source: String::new(), actor: String::new(), lock_state: String::new(),
            req_digest: String::new(), resp_digest: String::new(),
        }
    }
}

/// 摘要工具：压空白 + 截断 N 字（审计不给全文，只给摘要）。
pub fn digest_of(s: &str, n: usize) -> String {
    let t: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() <= n { t } else { t.chars().take(n).collect::<String>() + "…" }
}

/// 记一条调用；同时更新累计与熔断计数。返回 (累计调用数, 累计失败数, 是否刚触发熔断)。
pub fn record(conn: &Connection, r: &CallRecord, breaker_threshold: u32, cooldown_sec: u64) -> Result<(i64, i64, bool)> {
    ensure_table(conn)?;
    let ts = now_secs();
    conn.execute(
        "INSERT INTO llm_call_log(ts,backend,model,token_in,token_out,elapsed_ms,cache_hit,ok,note,         source,actor,lock_state,req_digest,resp_digest)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
        params![ts, r.backend, r.model, r.token_in, r.token_out, r.elapsed_ms,
                r.cache_hit as i64, r.ok as i64, r.note,
                r.source, r.actor, r.lock_state, r.req_digest, r.resp_digest],
    )?;
    // 确保 runtime 行存在
    conn.execute("INSERT OR IGNORE INTO llm_runtime(id) VALUES(1)", [])?;
    // 成功 → 失败计数清零；失败 → +1
    if r.ok {
        conn.execute("UPDATE llm_runtime SET concurrent_fail=0, total_calls=total_calls+1 WHERE id=1", [])?;
    } else {
        conn.execute("UPDATE llm_runtime SET concurrent_fail=concurrent_fail+1, total_calls=total_calls+1, total_fail=total_fail+1 WHERE id=1", [])?;
    }
    let fails: i64 = conn.query_row("SELECT concurrent_fail FROM llm_runtime WHERE id=1", [], |x| x.get(0)).unwrap_or(0);
    let mut tripped = false;
    if breaker_threshold > 0 && fails >= breaker_threshold as i64 {
        let until = ts + cooldown_sec as i64;
        conn.execute("UPDATE llm_runtime SET tripped_until_ts=?1 WHERE id=1", params![until])?;
        tripped = true;
    }
    let (tc, tf): (i64, i64) = conn
        .query_row("SELECT total_calls,total_fail FROM llm_runtime WHERE id=1", [], |x| Ok((x.get(0)?, x.get(1)?)))
        .unwrap_or((0, 0));
    Ok((tc, tf, tripped))
}

/// 熔断是否生效（当前时间 < tripped_until）。
pub fn breaker_tripped(conn: &Connection) -> Result<bool> {
    ensure_table(conn)?;
    let until: i64 = conn
        .query_row("SELECT IFNULL(tripped_until_ts,0) FROM llm_runtime WHERE id=1", [], |x| x.get(0))
        .unwrap_or(0);
    Ok(until > now_secs())
}

/// 手动重置熔断（`llm stats --reset-breaker`）。
pub fn reset_breaker(conn: &Connection) -> Result<()> {
    ensure_table(conn)?;
    conn.execute("INSERT OR IGNORE INTO llm_runtime(id) VALUES(1)", [])?;
    conn.execute("UPDATE llm_runtime SET concurrent_fail=0, tripped_until_ts=0 WHERE id=1", [])?;
    Ok(())
}

/// 统计总览。
pub fn stats(conn: &Connection) -> Result<Value> {
    ensure_table(conn)?;
    let (tc, tf): (i64, i64) = conn
        .query_row("SELECT IFNULL(total_calls,0),IFNULL(total_fail,0) FROM llm_runtime WHERE id=1", [], |x| Ok((x.get(0)?, x.get(1)?)))
        .unwrap_or((0, 0));
    let log_calls: i64 = conn.query_row("SELECT COUNT(*) FROM llm_call_log", [], |x| x.get(0)).unwrap_or(0);
    let (tin, tout): (i64, i64) = conn
        .query_row("SELECT IFNULL(SUM(token_in),0),IFNULL(SUM(token_out),0) FROM llm_call_log WHERE ok=1", [], |x| Ok((x.get(0)?, x.get(1)?)))
        .unwrap_or((0, 0));
    let hits: i64 = conn.query_row("SELECT COUNT(*) FROM llm_call_log WHERE cache_hit=1", [], |x| x.get(0)).unwrap_or(0);
    let avg_ms: f64 = conn
        .query_row("SELECT IFNULL(AVG(elapsed_ms),0) FROM llm_call_log WHERE cache_hit=0 AND ok=1", [], |x| x.get::<_, f64>(0))
        .unwrap_or(0.0);
    let mut by_backend: Vec<Value> = Vec::new();
    if let Ok(mut st) = conn.prepare(
        "SELECT backend,COUNT(*),IFNULL(SUM(token_in),0),IFNULL(SUM(token_out),0) FROM llm_call_log GROUP BY backend ORDER BY 2 DESC",
    ) {
        if let Ok(it) = st.query_map([], |x| Ok((x.get::<_, String>(0)?, x.get::<_, i64>(1)?, x.get::<_, i64>(2)?, x.get::<_, i64>(3)?))) {
            for x in it.filter_map(|v| v.ok()) {
                by_backend.push(json!({"backend": x.0, "calls": x.1, "token_in": x.2, "token_out": x.3}));
            }
        }
    }
    let tripped = breaker_tripped(conn)?;
    Ok(json!({
        "command": "world-gov llm stats",
        "total_calls": tc,
        "total_fail": tf,
        "log_rows": log_calls,
        "token_in_total": tin,
        "token_out_total": tout,
        "cache_hits": hits,
        "cache_hit_rate": if log_calls > 0 { hits as f64 / log_calls as f64 } else { 0.0 },
        "avg_elapsed_ms": avg_ms,
        "by_backend": by_backend,
        "breaker_tripped": tripped,
        "note": "仅统计隔离表 llm_call_log；不回显 prompt/response 正文。",
    }))
}

/// 供 self-check：结构检查（只读）。
pub fn table_check(conn: &Connection) -> Result<Value> {
    let exists: i64 = conn
        .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='llm_call_log'", [], |x| x.get(0))
        .unwrap_or(0);
    if exists == 0 {
        return Ok(json!({"ok": false, "missing_table": "llm_call_log"}));
    }
    let rows: i64 = conn.query_row("SELECT COUNT(*) FROM llm_call_log", [], |x| x.get(0)).unwrap_or(0);
    Ok(json!({"ok": true, "log_rows": rows, "breaker_tripped": breaker_tripped(conn)?}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        ensure_table(&c).unwrap();
        c
    }
    fn rec(ok: bool) -> CallRecord {
        CallRecord { backend: "mock".into(), model: "m".into(), token_in: 10, token_out: 5, elapsed_ms: 3, cache_hit: false, ok, note: String::new(), ..Default::default() }
    }

    #[test]
    fn table_isolated_and_idempotent() {
        let c = Connection::open_in_memory().unwrap();
        ensure_table(&c).unwrap();
        ensure_table(&c).unwrap();
        let names: Vec<String> = c.prepare("SELECT name FROM sqlite_master WHERE type='table'").unwrap()
            .query_map([], |r| r.get::<_, String>(0)).unwrap().filter_map(|x| x.ok()).collect();
        assert!(names.contains(&"llm_call_log".to_string()));
        assert!(!names.contains(&"library_index".to_string()));
    }

    #[test]
    fn record_and_stats() {
        let c = conn();
        record(&c, &rec(true), 5, 60).unwrap();
        record(&c, &rec(true), 5, 60).unwrap();
        let s = stats(&c).unwrap();
        assert_eq!(s["total_calls"], 2);
        assert_eq!(s["token_in_total"], 20);
        assert_eq!(s["token_out_total"], 10);
        assert_eq!(s["total_fail"], 0);
    }

    #[test]
    fn breaker_trips_after_threshold_and_resets_on_success() {
        let c = conn();
        for _ in 0..2 { record(&c, &rec(false), 3, 60).unwrap(); }
        assert!(!breaker_tripped(&c).unwrap(), "未达阈值不熔断");
        let (_tc, _tf, tripped) = record(&c, &rec(false), 3, 60).unwrap();
        assert!(tripped, "达阈值应触发");
        assert!(breaker_tripped(&c).unwrap());
        reset_breaker(&c).unwrap();
        assert!(!breaker_tripped(&c).unwrap());
    }

    #[test]
    fn success_clears_fail_counter() {
        let c = conn();
        record(&c, &rec(false), 5, 60).unwrap();
        record(&c, &rec(false), 5, 60).unwrap();
        record(&c, &rec(true), 5, 60).unwrap();
        assert!(!breaker_tripped(&c).unwrap());
        let f: i64 = c.query_row("SELECT concurrent_fail FROM llm_runtime WHERE id=1", [], |x| x.get(0)).unwrap();
        assert_eq!(f, 0);
    }

    #[test]
    fn cache_hit_counted() {
        let c = conn();
        let mut r = rec(true);
        r.cache_hit = true;
        record(&c, &r, 5, 60).unwrap();
        let s = stats(&c).unwrap();
        assert_eq!(s["cache_hits"], 1);
    }

    #[test]
    fn table_check_missing_then_present() {
        let c = Connection::open_in_memory().unwrap();
        assert_eq!(table_check(&c).unwrap()["ok"], false);
        assert_eq!(table_check(&conn()).unwrap()["ok"], true);
    }
}