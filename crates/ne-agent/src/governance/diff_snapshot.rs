//! 差异快照（world-gov，第 11 批）—— 逻辑快照：只存**索引 + 元信息哈希**。
//!
//! # 定位
//! **不复制小说正文**。拍一次快照 ≈ 把当时所有条目的
//! `key + layer + src_mtime + content_hash` 序列化进一张表；速度快、体积小。
//! 可对比任意两个快照 → 条目 增/删/改。
//!
//! # 存储（复用 novel.db，与主索引隔离）
//! `lib_snapshot(snap_id TEXT(40) PK, snap_label, created_ts, baseline_fp, src_meta BLOB)`
//! - `snap_id` = sha1(label + created_ts + 条目元信息指纹) 派生，40 hex。
//! - `src_meta` = JSON 序列化的条目元信息数组（**只含哈希，不含正文**）。
//!
//! # 铁则
//! - ❌ 不复制正文；不写/改 `library_index`；不触碰源文件。
//! - ✅ `CREATE TABLE IF NOT EXISTS` 幂等。
//! - ✅ 快照只读：不能在快照里改真实库；删除快照只删本表行。
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

use super::digest;

/// 单表 schema（幂等）。
const SCHEMA: &str = r#"
-- 差异快照（2026-10-14，第 11 批）—— 逻辑快照；只存索引+元信息哈希，不存正文。
CREATE TABLE IF NOT EXISTS lib_snapshot (
    snap_id     TEXT PRIMARY KEY,      -- sha1 派生 id（40 hex）
    snap_label  TEXT NOT NULL DEFAULT '',
    created_ts  INTEGER NOT NULL,
    baseline_fp TEXT NOT NULL DEFAULT '',
    src_meta    BLOB NOT NULL          -- JSON：数组，每项 {key,layer,src_mtime,content_hash}
);
CREATE INDEX IF NOT EXISTS idx_lib_snapshot_ts ON lib_snapshot(created_ts);
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

/// 单个条目的元信息（**只含哈希，不含正文**）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MetaEntry {
    pub key: String,
    pub layer: String,
    pub src_mtime: i64,
    pub content_hash: String,
}

/// 从 library_index + 源文件构建当前世界的元信息数组（按 key 排序）。
/// 只读；源文件缺失 → content_hash = "ABSENT"。
pub fn collect_meta(conn: &Connection, lib_root: &Path) -> Result<Vec<MetaEntry>> {
    let has_mtime: bool = {
        let mut st = conn.prepare("PRAGMA table_info(library_index)")?;
        let cols: Vec<String> = st
            .query_map([], |r| r.get::<_, String>(1))?
            .filter_map(|v| v.ok())
            .collect();
        cols.iter().any(|c| c == "src_mtime")
    };
    let sql = if has_mtime {
        "SELECT key,layer,IFNULL(source,''),IFNULL(src_mtime,0) FROM library_index ORDER BY key"
    } else {
        "SELECT key,layer,IFNULL(source,''),0 FROM library_index ORDER BY key"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows: Vec<(String, String, String, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .filter_map(|v| v.ok())
        .collect();
    let mut out: Vec<MetaEntry> = Vec::with_capacity(rows.len());
    for (key, layer, source, src_mtime) in rows {
        let content_hash = if source.trim().is_empty() {
            "NO_SOURCE".to_string()
        } else {
            let p = if Path::new(&source).is_absolute() {
                std::path::PathBuf::from(&source)
            } else {
                lib_root.join(&source)
            };
            digest::file_content_hash(&p)
        };
        out.push(MetaEntry { key, layer, src_mtime, content_hash });
    }
    Ok(out)
}

/// 元信息数组的稳定指纹（对 key+layer+mtime+hash 取 sha1）。
pub fn meta_fingerprint(meta: &[MetaEntry]) -> String {
    let mut s = String::new();
    for m in meta {
        s.push_str(&m.key);
        s.push('\u{1}');
        s.push_str(&m.layer);
        s.push('\u{1}');
        s.push_str(&m.src_mtime.to_string());
        s.push('\u{1}');
        s.push_str(&m.content_hash);
        s.push('\u{2}');
    }
    digest::sha1_hex(s.as_bytes())
}

/// 派生 snap_id（**纯函数**）：sha1(label + created_ts + meta_fp)。
pub fn derive_snap_id(label: &str, created_ts: i64, meta_fp: &str) -> String {
    digest::sha1_hex(format!("{label}\u{1}{created_ts}\u{1}{meta_fp}").as_bytes())
}

/// 创建快照。返回 (snap_id, entry_count)。`label` 空则给默认名。
pub fn create(conn: &Connection, lib_root: &Path, label: &str) -> Result<(String, usize)> {
    ensure_table(conn)?;
    let meta = collect_meta(conn, lib_root)?;
    let created = now_secs();
    let label = if label.trim().is_empty() {
        format!("snap-{created}")
    } else {
        label.trim().to_string()
    };
    let meta_fp = meta_fingerprint(&meta);
    let baseline_fp = crate::world_gov::read_baseline_fingerprint(lib_root).unwrap_or_default();
    let snap_id = derive_snap_id(&label, created, &meta_fp);
    let blob = serde_json::to_vec(&meta)?;
    conn.execute(
        "INSERT OR REPLACE INTO lib_snapshot(snap_id,snap_label,created_ts,baseline_fp,src_meta)
         VALUES(?1,?2,?3,?4,?5)",
        params![snap_id, label, created, baseline_fp, blob],
    )?;
    Ok((snap_id, meta.len()))
}

/// 快照元数据（不含 src_meta 大字段）。
pub fn list(conn: &Connection) -> Result<Vec<Value>> {
    ensure_table(conn)?;
    let mut stmt = conn.prepare(
        "SELECT snap_id,snap_label,created_ts,baseline_fp,length(src_meta) FROM lib_snapshot ORDER BY created_ts DESC",
    )?;
    let rows: Vec<Value> = stmt
        .query_map([], |r| {
            Ok(json!({
                "snap_id": r.get::<_, String>(0)?,
                "label": r.get::<_, String>(1)?,
                "created_ts": r.get::<_, i64>(2)?,
                "baseline_fp": r.get::<_, String>(3)?,
                "meta_bytes": r.get::<_, i64>(4)?,
            }))
        })?
        .filter_map(|x| x.ok())
        .collect();
    Ok(rows)
}

/// 读某个快照的元信息数组。
pub fn read_meta(conn: &Connection, snap_id: &str) -> Result<Option<Vec<MetaEntry>>> {
    ensure_table(conn)?;
    let blob: Option<Vec<u8>> = conn
        .query_row(
            "SELECT src_meta FROM lib_snapshot WHERE snap_id=?1",
            params![snap_id],
            |r| r.get(0),
        )
        .optional()?;
    match blob {
        Some(b) => Ok(Some(serde_json::from_slice(&b)?)),
        None => Ok(None),
    }
}

/// 取单个快照摘要（含 label/ts/条目数）。
pub fn show_meta(conn: &Connection, snap_id: &str) -> Result<Option<Value>> {
    ensure_table(conn)?;
    let r: Option<(String, String, i64, String, Vec<u8>)> = conn
        .query_row(
            "SELECT snap_id,snap_label,created_ts,baseline_fp,src_meta FROM lib_snapshot WHERE snap_id=?1",
            params![snap_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    match r {
        Some((id, label, ts, fp, blob)) => {
            let meta: Vec<MetaEntry> = serde_json::from_slice(&blob).unwrap_or_default();
            Ok(Some(json!({
                "snap_id": id, "label": label, "created_ts": ts,
                "baseline_fp": fp, "entry_count": meta.len(),
            })))
        }
        None => Ok(None),
    }
}

/// 删除快照：返回是否删除。**只删本表行**，不动任何业务数据。
pub fn delete(conn: &Connection, snap_id: &str) -> Result<bool> {
    ensure_table(conn)?;
    let n = conn.execute("DELETE FROM lib_snapshot WHERE snap_id=?1", params![snap_id])?;
    Ok(n > 0)
}

/// 两快照差异结果。
#[derive(Debug, Clone)]
pub struct DiffResult {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub modified: Vec<(String, String, String)>, // (key, hash_a, hash_b)
}

/// 对比两个元信息数组（**纯函数**）。
pub fn diff_meta(a: &[MetaEntry], b: &[MetaEntry]) -> DiffResult {
    let ma: BTreeMap<&str, &MetaEntry> = a.iter().map(|m| (m.key.as_str(), m)).collect();
    let mb: BTreeMap<&str, &MetaEntry> = b.iter().map(|m| (m.key.as_str(), m)).collect();
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut modified = Vec::new();
    for (k, mb_e) in &mb {
        match ma.get(k) {
            None => added.push((*k).to_string()),
            Some(ma_e) => {
                if ma_e.content_hash != mb_e.content_hash {
                    modified.push(((*k).to_string(), ma_e.content_hash.clone(), mb_e.content_hash.clone()));
                }
            }
        }
    }
    for k in ma.keys() {
        if !mb.contains_key(k) {
            removed.push((*k).to_string());
        }
    }
    added.sort();
    removed.sort();
    modified.sort_by(|x, y| x.0.cmp(&y.0));
    DiffResult { added, removed, modified }
}

/// 对比两个已存快照 → 结构化结果。
pub fn diff_snapshots(conn: &Connection, snap_a: &str, snap_b: &str) -> Result<Value> {
    let a = read_meta(conn, snap_a)?
        .ok_or_else(|| anyhow::anyhow!("快照不存在：{snap_a}"))?;
    let b = read_meta(conn, snap_b)?
        .ok_or_else(|| anyhow::anyhow!("快照不存在：{snap_b}"))?;
    let d = diff_meta(&a, &b);
    Ok(json!({
        "command": "world-gov diff show",
        "snap_a": snap_a, "snap_b": snap_b,
        "counts": {
            "a_total": a.len(), "b_total": b.len(),
            "added": d.added.len(), "removed": d.removed.len(), "modified": d.modified.len(),
        },
        "added": d.added,
        "removed": d.removed,
        "modified": d.modified.iter().map(|(k, ha, hb)| json!({
            "key": k, "hash_a": ha, "hash_b": hb,
        })).collect::<Vec<_>>(),
        "note": "逻辑差异（基于内容哈希）；快照只读，不修改真实库。",
    }))
}

/// 自某快照以来发生改动（added+modified）的 key 列表。
pub fn changed_since(conn: &Connection, snap: &str, current: &[MetaEntry]) -> Result<Vec<String>> {
    let a = read_meta(conn, snap)?.ok_or_else(|| anyhow::anyhow!("快照不存在：{snap}"))?;
    let d = diff_meta(&a, current);
    let mut out: Vec<String> = d.added;
    out.extend(d.modified.into_iter().map(|(k, _, _)| k));
    out.sort();
    out.dedup();
    Ok(out)
}

/// 供 self-check：结构完整性 + 条目数/废弃提示（**只读**）。
pub fn table_check(conn: &Connection, stale_days: i64) -> Result<Value> {
    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='lib_snapshot'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if exists == 0 {
        return Ok(json!({"ok": false, "missing_table": "lib_snapshot",
            "hint": "跑任意 world-gov 命令（open_gov 幂等建表）即可创建"}));
    }
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM lib_snapshot", [], |r| r.get(0)).unwrap_or(0);
    let cutoff = now_secs() - stale_days * 86400;
    let stale: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM lib_snapshot WHERE created_ts < ?1",
            params![cutoff],
            |r| r.get(0),
        )
        .unwrap_or(0);
    Ok(json!({
        "ok": true,
        "snapshots": total,
        "stale_older_than_days": stale_days,
        "stale_count": stale,
        "hint": if stale > 20 { "废弃快照较多；可用 diff snap delete 手动清理（不自动删）" } else { "" },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_with_index() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE library_index(layer TEXT,key TEXT,title TEXT,chars INTEGER,source TEXT,src_mtime INTEGER NOT NULL DEFAULT 0);",
        ).unwrap();
        ensure_table(&c).unwrap();
        c
    }

    fn seed(lib: &Path) {
        std::fs::create_dir_all(lib).unwrap();
        std::fs::write(lib.join("a.md"), b"aaa").unwrap();
        std::fs::write(lib.join("b.md"), b"bbb").unwrap();
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
        assert!(names.contains(&"lib_snapshot".to_string()));
        assert!(!names.contains(&"library_index".to_string()));
    }

    #[test]
    fn meta_fingerprint_and_snap_id_are_stable() {
        let m = vec![
            MetaEntry { key: "A".into(), layer: "L4".into(), src_mtime: 1, content_hash: "h1".into() },
            MetaEntry { key: "B".into(), layer: "L3".into(), src_mtime: 2, content_hash: "h2".into() },
        ];
        let fp1 = meta_fingerprint(&m);
        assert_eq!(fp1, meta_fingerprint(&m), "同数据同指纹");
        let m2 = vec![MetaEntry { key: "A".into(), layer: "L4".into(), src_mtime: 1, content_hash: "CHANGED".into() }];
        assert_ne!(meta_fingerprint(&m2), fp1, "哈希变化 → 指纹变化");
        let id1 = derive_snap_id("v1", 100, &fp1);
        assert_eq!(id1.len(), 40);
        assert_ne!(id1, derive_snap_id("v1", 101, &fp1), "时间戳不同 → id 不同");
        assert_ne!(id1, derive_snap_id("v2", 100, &fp1), "label 不同 → id 不同");
    }

    #[test]
    fn diff_detects_added_removed_modified() {
        let a = vec![
            MetaEntry { key: "A".into(), layer: "L4".into(), src_mtime: 1, content_hash: "hA".into() },
            MetaEntry { key: "B".into(), layer: "L4".into(), src_mtime: 1, content_hash: "hB".into() },
            MetaEntry { key: "C".into(), layer: "L4".into(), src_mtime: 1, content_hash: "hC".into() },
        ];
        let b = vec![
            MetaEntry { key: "A".into(), layer: "L4".into(), src_mtime: 2, content_hash: "hA".into() },
            MetaEntry { key: "B".into(), layer: "L4".into(), src_mtime: 2, content_hash: "hB_CHANGED".into() },
            MetaEntry { key: "D".into(), layer: "L4".into(), src_mtime: 2, content_hash: "hD".into() },
        ];
        let d = diff_meta(&a, &b);
        assert_eq!(d.added, vec!["D"]);
        assert_eq!(d.removed, vec!["C"]);
        assert_eq!(d.modified.len(), 1);
        assert_eq!(d.modified[0].0, "B");
        assert_eq!(d.modified[0].1, "hB");
        assert_eq!(d.modified[0].2, "hB_CHANGED");
    }

    #[test]
    fn create_list_diff_delete_roundtrip() {
        let lib = std::env::temp_dir().join(format!("wg_snap_rt_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&lib);
        seed(&lib);
        let conn = mem_with_index();
        conn.execute("INSERT INTO library_index(layer,key,source,src_mtime) VALUES('L4','A','a.md',10)", []).unwrap();
        conn.execute("INSERT INTO library_index(layer,key,source,src_mtime) VALUES('L4','B','b.md',10)", []).unwrap();
        let (id1, n1) = create(&conn, &lib, "v1-初始").unwrap();
        assert_eq!(n1, 2);
        assert_eq!(id1.len(), 40);
        // 改 b.md → 拍第二快照
        std::fs::write(lib.join("b.md"), b"BBB").unwrap();
        let (id2, _) = create(&conn, &lib, "v2-改B").unwrap();
        assert_ne!(id1, id2);
        let l = list(&conn).unwrap();
        assert_eq!(l.len(), 2);
        // diff：modified 含 B
        let d = diff_snapshots(&conn, &id1, &id2).unwrap();
        assert_eq!(d["counts"]["modified"], 1);
        assert_eq!(d["modified"][0]["key"], "B");
        assert_eq!(d["counts"]["added"], 0);
        assert_eq!(d["counts"]["removed"], 0);
        // changed_since
        let cur = collect_meta(&conn, &lib).unwrap();
        let ch = changed_since(&conn, &id1, &cur).unwrap();
        assert_eq!(ch, vec!["B"]);
        // delete
        assert!(delete(&conn, &id1).unwrap());
        assert!(!delete(&conn, &id1).unwrap());
        assert_eq!(list(&conn).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&lib);
    }

    #[test]
    fn absent_source_hashes_to_sentinel_not_panic() {
        let lib = std::env::temp_dir().join(format!("wg_snap_absent_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&lib);
        std::fs::create_dir_all(&lib).unwrap();
        let conn = mem_with_index();
        conn.execute("INSERT INTO library_index(layer,key,source,src_mtime) VALUES('L4','X','missing.md',0)", []).unwrap();
        let meta = collect_meta(&conn, &lib).unwrap();
        assert_eq!(meta.len(), 1);
        assert_eq!(meta[0].content_hash, "ABSENT");
        let _ = std::fs::remove_dir_all(&lib);
    }

    #[test]
    fn diff_missing_snapshot_errors_cleanly() {
        let conn = mem_with_index();
        assert!(diff_snapshots(&conn, "nope", "nope2").is_err());
    }

    #[test]
    fn table_check_reports_and_counts() {
        let conn = mem_with_index();
        let r = table_check(&conn, 30).unwrap();
        assert_eq!(r["ok"], true);
        assert_eq!(r["snapshots"], 0);
    }

    #[test]
    fn table_check_missing_table() {
        let c = Connection::open_in_memory().unwrap();
        let r = table_check(&c, 30).unwrap();
        assert_eq!(r["ok"], false);
        assert_eq!(r["missing_table"], "lib_snapshot");
    }
}