//! 终态总览（world-gov，第 16 批）—— **只读**聚合全批次产物，一次性体检。
//!
//! # 定位
//! 第 1–15 批的产物散在各隔离表与目录里；本模块把它们**汇总成一份终态快照**，
//! 让人一眼看清：能力是否就绪、主索引有无被污染、各隔离表有多少数据、
//! 关系网/沙盘是否已建、三重锁状态、以及「扫描→增量→标签→快照→关系网→沙盘」闭环链。
//!
//! # 铁则（本模块最严）
//! - ✅ **纯只读**：只 SELECT / 列目录，不写任何表、不改任何文件、不建目录。
//! - ✅ 零外呼、零真实 LLM。
//! - ✅ 显式**零污染证明**：校验 `library_index` 仍在且辅助表均非主表。
use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::Path;

use super::{conflict_store, diff_snapshot, llm_gate, note_tag_store, probe_cache, rel_graph, sandbox, self_check};

/// 本轮终态纪元（与 self_check epoch 对齐）。
pub const TERMINAL_EPOCH: i64 = 17;

/// 受保护的主表（**绝不允许**被辅助表逻辑替换/覆盖）。
const PROTECTED_TABLES: &[&str] = &["library_index", "materials", "task_jobs", "risk_index"];

/// 辅助隔离表（world-gov 自有；与主索引物理隔离）。
const ISOLATED_TABLES: &[&str] = &[
    "item_tag",
    "item_note",
    "lib_snapshot",
    "conflict_records",
    "rel_edge",
    "rel_graph_meta",
    "sandbox_run",
    "sandbox_step",
];

fn table_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
        rusqlite::params![name],
        |r| r.get::<_, i64>(0),
    )
    .unwrap_or(0)
        > 0
}

fn count(conn: &Connection, table: &str) -> i64 {
    if !table_exists(conn, table) {
        return -1; // -1 = 表不存在（区别于 0 行）
    }
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get::<_, i64>(0))
        .unwrap_or(-1)
}

/// 所有库内表名。
fn all_tables(conn: &Connection) -> Vec<String> {
    conn.prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .ok()
        .and_then(|mut s| {
            s.query_map([], |r| r.get::<_, String>(0))
                .ok()
                .map(|it| it.filter_map(|x| x.ok()).collect::<Vec<_>>())
        })
        .unwrap_or_default()
}

/// 零污染证明：主表在、辅助表存在、且无同名冲突。
fn no_pollution_proof(conn: &Connection) -> Value {
    let tables = all_tables(conn);
    let missing_protected: Vec<&str> = PROTECTED_TABLES
        .iter()
        .filter(|t| !tables.iter().any(|x| x == *t))
        .copied()
        .collect();
    let isolated_present: Vec<&str> = ISOLATED_TABLES
        .iter()
        .filter(|t| tables.iter().any(|x| x == *t))
        .copied()
        .collect();
    // 辅助表名不得与主表名重叠
    let collision: Vec<&str> = ISOLATED_TABLES
        .iter()
        .filter(|t| PROTECTED_TABLES.contains(t))
        .copied()
        .collect();
    let ok = collision.is_empty() && missing_protected.is_empty();
    json!({
        "ok": ok,
        "library_index_present": tables.iter().any(|t| t == "library_index"),
        "missing_protected": missing_protected,
        "isolated_present": isolated_present,
        "name_collision": collision,
        "note": "只读校验：主表存在且辅助表名与主表不重叠 → 主索引零污染。",
    })
}

/// 基线信息。
fn baseline_info(conn: &Connection) -> Value {
    match crate::world_gov::baseline_entries(conn) {
        Ok(entries) if !entries.is_empty() => {
            let fp = crate::world_gov::baseline_fingerprint(&entries);
            json!({"entries": entries.len(), "fingerprint": fp})
        }
        Ok(_) => json!({"entries": 0, "hint": "基线为空；跑 world-gov bootstrap"}),
        Err(e) => json!({"error": e.to_string()}),
    }
}

/// 闭环环节链状态（每环给出是否就绪与关键计数）。
fn chain_status(conn: &Connection, lib_root: &Path) -> Value {
    let cards = count(conn, "library_index");
    let indexes_ok = cards > 0;
    let tag_ok = table_exists(conn, "item_tag");
    let snap_ok = table_exists(conn, "lib_snapshot");
    let graph_ok = table_exists(conn, "rel_graph_meta") && count(conn, "rel_graph_meta") > 0;
    let sandbox_ok = table_exists(conn, "sandbox_run");
    let cache = probe_cache::status(lib_root, 30).unwrap_or(json!({}));
    json!([
        {"step": "scan/incremental", "ready": indexes_ok, "cards": cards},
        {"step": "tag/note", "ready": tag_ok, "item_tag": count(conn, "item_tag"), "item_note": count(conn, "item_note")},
        {"step": "snapshot", "ready": snap_ok, "lib_snapshot": count(conn, "lib_snapshot")},
        {"step": "graph", "ready": graph_ok, "rel_edge": count(conn, "rel_edge")},
        {"step": "sandbox", "ready": sandbox_ok, "sandbox_run": count(conn, "sandbox_run")},
        {"step": "probe_cache", "ready": true, "entries": cache.get("entries").cloned().unwrap_or(json!(0))},
        {"step": "ai(optional)", "ready": llm_gate::compile_feature_enabled(), "locked_by_default": !llm_gate::real_llm_unlocked(false)},
    ])
}

/// 主入口：汇总终态快照（纯只读）。
pub fn overview(conn: &Connection, lib_root: &Path) -> Result<Value> {
    let mut caps = self_check::capabilities();
    if let Some(o) = caps.as_object_mut() {
        o.insert("toolchain_epoch".into(), json!(TERMINAL_EPOCH));
    }

    let graph_meta = rel_graph::summary(conn, 0)
        .ok()
        .and_then(|v| v.get("meta").cloned())
        .unwrap_or(json!({"built": false}));
    let graph_edges = count(conn, "rel_edge");
    let graph_dangling = graph_meta.get("dangling").cloned().unwrap_or(json!(0));

    let conflict = conflict_store::summary(conn).unwrap_or(json!({}));
    let sandbox_runs = count(conn, "sandbox_run");
    let sandbox_check = sandbox::table_check(conn).unwrap_or(json!({"ok": false}));
    let tag_check = note_tag_store::table_check(conn).unwrap_or(json!({"ok": false}));
    let snap_check = diff_snapshot::table_check(conn, 30).unwrap_or(json!({"ok": false}));

    let proof = no_pollution_proof(conn);

    Ok(json!({
        "command": "world-gov status",
        "toolchain_epoch": TERMINAL_EPOCH,
        "lib_root": lib_root.display().to_string(),
        "capabilities": caps,
        "triple_lock": llm_gate::lock_status(false),
        "baseline": baseline_info(conn),
        "library": {"cards": count(conn, "library_index")},
        "isolated_tables": {
            "item_tag": count(conn, "item_tag"),
            "item_note": count(conn, "item_note"),
            "lib_snapshot": count(conn, "lib_snapshot"),
            "conflict_records": count(conn, "conflict_records"),
            "rel_edge": graph_edges,
            "rel_graph_meta": count(conn, "rel_graph_meta"),
            "sandbox_run": sandbox_runs,
            "sandbox_step": count(conn, "sandbox_step"),
        },
        "graph": {"meta": graph_meta, "edges": graph_edges, "dangling": graph_dangling},
        "conflict": conflict,
        "sandbox": {"runs": sandbox_runs, "check_ok": sandbox_check["ok"] == true},
        "health": {
            "note_tag_store_ok": tag_check["ok"] == true,
            "lib_snapshot_ok": snap_check["ok"] == true,
            "sandbox_ok": sandbox_check["ok"] == true,
        },
        "no_pollution": proof,
        "chain": chain_status(conn, lib_root),
        "note": "纯只读终态快照；不改任何表/文件，零外呼。AI 环节默认锁死（三重锁）。",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn mem_seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE library_index(key TEXT, layer TEXT, source TEXT, types TEXT DEFAULT '[]');
             CREATE TABLE task_jobs(id TEXT);
             CREATE TABLE risk_index(id INTEGER);
             CREATE TABLE materials(id INTEGER);",
        )
        .unwrap();
        note_tag_store::ensure_tables(&c).unwrap();
        diff_snapshot::ensure_table(&c).unwrap();
        conflict_store::ensure_table(&c).unwrap();
        rel_graph::ensure_table(&c).unwrap();
        sandbox::ensure_table(&c).unwrap();
        c.execute("INSERT INTO library_index(key,layer,source) VALUES('CHAR-99-001','L4','x.md')", []).unwrap();
        c
    }

    #[test]
    fn overview_is_readonly_and_reports_epoch() {
        let c = mem_seeded();
        let before = all_tables(&c);
        let v = overview(&c, Path::new("/tmp")).unwrap();
        let after = all_tables(&c);
        assert_eq!(before, after, "只读：表集合不变");
        assert_eq!(v["toolchain_epoch"], TERMINAL_EPOCH);
        assert_eq!(v["library"]["cards"], 1);
    }

    #[test]
    fn no_pollution_proof_ok_when_clean() {
        let c = mem_seeded();
        let v = overview(&c, Path::new("/tmp")).unwrap();
        assert_eq!(v["no_pollution"]["ok"], true);
        assert_eq!(v["no_pollution"]["library_index_present"], true);
        assert!(v["no_pollution"]["missing_protected"].as_array().unwrap().is_empty());
    }

    #[test]
    fn no_pollution_proof_flags_missing_main_table() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE library_index(key TEXT);").unwrap();
        // 缺 materials/task_jobs/risk_index → 证明失败
        let p = no_pollution_proof(&c);
        assert_eq!(p["ok"], false);
        assert!(!p["missing_protected"].as_array().unwrap().is_empty());
    }

    #[test]
    fn isolated_tables_counts_present() {
        let c = mem_seeded();
        let v = overview(&c, Path::new("/tmp")).unwrap();
        // 已建表 → 计数 >=0（非 -1）
        assert!(v["isolated_tables"]["rel_edge"].as_i64().unwrap() >= 0);
        assert!(v["isolated_tables"]["sandbox_run"].as_i64().unwrap() >= 0);
    }

    #[test]
    fn chain_marks_steps() {
        let c = mem_seeded();
        let v = overview(&c, Path::new("/tmp")).unwrap();
        let chain = v["chain"].as_array().unwrap();
        // 有卡 → scan/incremental ready
        assert!(chain.iter().any(|x| x["step"] == "scan/incremental" && x["ready"] == true));
        // ai 环节默认锁死
        assert!(chain.iter().any(|x| x["step"] == "ai(optional)" && x["locked_by_default"] == true));
    }

    #[test]
    fn missing_tables_count_is_minus_one() {
        let c = Connection::open_in_memory().unwrap();
        assert_eq!(count(&c, "nope"), -1);
    }
}