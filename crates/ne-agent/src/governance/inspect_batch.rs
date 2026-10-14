//! inspect-batch（world-gov）—— 批量检视入口（**默认只预览，不入队；绝不自动推理**）。
//!
//! # 定位
//! `inspect-card` 一次一条；本模块把**一批 key** 拆成多条批量任务，支持三种输入模式：
//! - `--from-scan <scan_probe_*.json>`：直接读 scan 落盘探针，取其中的 key 列表；
//! - `--id-list <file>`：从文本文件读一批 key（每行一个，忽略空行/`#` 注释）；
//! - `--since <ts|last>`：与 scan 增量复用同一时间条件（读 library_index.src_mtime）。
//!
//! # 红线（铁则）
//! - ❌ 不调用 LLM / 不推理 / 不发请求；只**列出**要处理哪些 key（预览）。
//! - ❌ 不自动 step；`--enqueue` 只入队（可选 `--persist` 落 task_jobs），推进仍需手打 `queue step`。
//! - ✅ `--dry-run` 只算数量，不写任何任务/探针文件。
//! - ✅ 复用既有 `inspect-card` 探针与 `queue` 语义，不新增表。

use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::Path;

use super::scan_snapshot;

/// 批量输入模式。
#[derive(Debug, Clone, PartialEq)]
pub enum BatchSource {
    /// 从 scan 落盘探针取 key
    FromScan(String),
    /// 从文本文件取 key
    IdList(String),
    /// 按增量时间条件取 key（读 index.src_mtime）
    Since(i64),
    /// 全库取 key（默认仅 L2/L3/L4；配合 `--slice` 分片）——第 17 批
    All,
}

impl BatchSource {
    pub fn kind(&self) -> &'static str {
        match self {
            BatchSource::FromScan(_) => "from-scan",
            BatchSource::IdList(_) => "id-list",
            BatchSource::Since(_) => "since",
            BatchSource::All => "all",
        }
    }
}

/// 从 scan 探针 JSON 文本解析 key 列表（**纯函数**，便于单测）。
pub fn keys_from_scan_json(raw: &str) -> Result<Vec<String>> {
    let v: Value = serde_json::from_str(raw)
        .map_err(|e| anyhow::anyhow!("scan 探针不是合法 JSON：{e}"))?;
    let items = v
        .get("probe_items")
        .and_then(|x| x.as_array())
        .ok_or_else(|| anyhow::anyhow!("scan 探针缺少 probe_items 数组"))?;
    let keys: Vec<String> = items
        .iter()
        .filter_map(|it| it.get("key").and_then(|k| k.as_str()).map(|s| s.to_string()))
        .collect();
    Ok(keys)
}

/// 从 id-list 文本解析 key（每行一个；忽略空行与 `#` 注释）。
pub fn keys_from_id_list(raw: &str) -> Vec<String> {
    raw.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.to_string())
        .collect()
}

/// 按 `since` 从 index 取 key（增量；默认遵守 L2/L3/L4 过滤）。
/// 无有效 src_mtime → 返回全部扫描域 key（降级），由调用方标注。
pub fn keys_since(
    conn: &Connection,
    since: i64,
    all_layers: bool,
) -> Result<(Vec<String>, bool)> {
    let has_mtime: bool = {
        let mut st = conn.prepare("PRAGMA table_info(library_index)")?;
        let cols: Vec<String> = st
            .query_map([], |r| r.get::<_, String>(1))?
            .filter_map(|v| v.ok())
            .collect();
        cols.iter().any(|c| c == "src_mtime")
    };
    if !has_mtime {
        return Ok((keys_all(conn, all_layers)?, true));
    }
    let mut stmt = conn.prepare(
        "SELECT key,layer,IFNULL(src_mtime,0) FROM library_index ORDER BY layer,key",
    )?;
    let rows: Vec<(String, String, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .filter_map(|x| x.ok())
        .collect();
    let any_mtime = rows.iter().any(|(_, _, m)| *m > 0);
    if !any_mtime {
        // 全 0 → 增量无意义，降级全扫
        return Ok((keys_all(conn, all_layers)?, true));
    }
    let keys: Vec<String> = rows
        .into_iter()
        .filter(|(_, layer, _)| all_layers || crate::world_gov::is_scannable_layer(layer))
        .filter(|(_, _, m)| *m >= since)
        .map(|(k, _, _)| k)
        .collect();
    Ok((keys, false))
}

/// 全库 key（遵守层过滤）。
fn keys_all(conn: &Connection, all_layers: bool) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT key,layer FROM library_index ORDER BY layer,key")?;
    let keys: Vec<String> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .filter_map(|x| x.ok())
        .filter(|(_, layer)| all_layers || crate::world_gov::is_scannable_layer(layer))
        .map(|(k, _)| k)
        .collect();
    Ok(keys)
}

/// 解析输入，得到待处理 key 列表 + 降级标记。
pub fn resolve_keys(
    conn: &Connection,
    lib_root: &Path,
    source: &BatchSource,
    all_layers: bool,
) -> Result<(Vec<String>, bool)> {
    match source {
        BatchSource::FromScan(p) => {
            let path = if Path::new(p).is_absolute() {
                std::path::PathBuf::from(p)
            } else {
                lib_root.join(p)
            };
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| anyhow::anyhow!("读取 scan 探针失败 {}：{e}", path.display()))?;
            Ok((keys_from_scan_json(&raw)?, false))
        }
        BatchSource::IdList(p) => {
            let path = if Path::new(p).is_absolute() {
                std::path::PathBuf::from(p)
            } else {
                lib_root.join(p)
            };
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| anyhow::anyhow!("读取 id-list 失败 {}：{e}", path.display()))?;
            Ok((keys_from_id_list(&raw), false))
        }
        BatchSource::Since(ts) => keys_since(conn, *ts, all_layers),
        BatchSource::All => Ok((keys_all(conn, all_layers)?, false)),
    }
}

/// 取第 `n/m` 片（1 基；`n` ∈ [1,m]）。纯函数：稳定顺序切分，空输入安全。
/// 与 `scan-library --slice` 同源语义（`world_gov::parse_slice_strict` 负责校验 N/M）。
pub fn slice_keys(keys: Vec<String>, n: usize, m: usize) -> Vec<String> {
    if m == 0 || keys.is_empty() {
        return Vec::new();
    }
    let i = n.saturating_sub(1); // 1 基 → 0 基
    let len = keys.len();
    let start = len * i / m;
    let end = len * (i + 1) / m;
    keys.into_iter().skip(start).take(end.saturating_sub(start)).collect()
}

/// 从 `--since <val>` 字符串复用 scan_snapshot 的解析（支持 `last`）。
pub fn parse_since(val: &str, lib_root: &Path) -> Result<(Option<i64>, String)> {
    scan_snapshot::parse_since(val, lib_root)
}

/// 组装批量预览报告（**纯读**，不写任何文件/任务）。
/// `tag_filter`（可选）：人工标签筛选（第 10 批）；`None`/空 = 不过滤（默认行为不变）。
/// `slice`（可选）：取第 N/M 片（第 17 批）；`None` = 不分片（默认行为不变）。
pub fn build_batch_report(
    conn: &Connection,
    lib_root: &Path,
    source: &BatchSource,
    all_layers: bool,
    dry_run: bool,
    tag_filter: Option<&crate::governance::note_tag_store::TagFilter>,
) -> Result<Value> {
    build_batch_report_sliced(conn, lib_root, source, all_layers, dry_run, tag_filter, None)
}

/// 同 `build_batch_report`，但额外支持 `--slice N/M` 分片（第 17 批）。
/// 分片在**标签筛选之后**进行：先得候选全集，再取第 N/M 片，保证各片不重不漏。
#[allow(clippy::too_many_arguments)]
pub fn build_batch_report_sliced(
    conn: &Connection,
    lib_root: &Path,
    source: &BatchSource,
    all_layers: bool,
    dry_run: bool,
    tag_filter: Option<&crate::governance::note_tag_store::TagFilter>,
    slice: Option<(usize, usize)>,
) -> Result<Value> {
    let (keys, degraded) = resolve_keys(conn, lib_root, source, all_layers)?;
    // 标签筛选：默认不过滤（None/空 → 全放行，向后兼容）
    let before = keys.len();
    let keys: Vec<String> = match tag_filter {
        Some(f) if !f.is_empty() => {
            use crate::governance::note_tag_store as nts;
            match nts::keys_matching(conn, f)? {
                Some(set) => keys.into_iter().filter(|k| set.contains(k)).collect(),
                None => keys,
            }
        }
        _ => keys,
    };
    let tag_filtered = before - keys.len();
    // 分片：默认不分片（None → 全量，向后兼容）
    let selected_before_slice = keys.len();
    let keys: Vec<String> = match slice {
        Some((n, m)) => slice_keys(keys, n, m),
        None => keys,
    };
    Ok(json!({
        "command": "world-gov inspect-batch",
        "mode": "batch_preview",
        "source_kind": source.kind(),
        "counts": {
            "batch_size": keys.len(),
            "all_layers": all_layers,
            "since_degraded": degraded,
            "tag_filtered": tag_filtered,
            "slice": slice.map(|(n, m)| json!({"n": n, "m": m, "selected_before_slice": selected_before_slice})).unwrap_or(json!(null)),
        },
        "tag_filter": tag_filter.map(|f| f.describe()).unwrap_or(json!(null)),
        "keys": keys,
        "plan": {
            "per_key_action": "生成 inspect-card 探针 → 可选入队（不自动推理、不自动 step）",
            "enqueue_required": true,
            "gate_required_for_infer": true,
        },
        "dry_run": dry_run,
        "note": "批量只预览/入队，绝不自动推理；推进需手打 world-gov queue step。",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE library_index(layer TEXT,key TEXT,title TEXT,chars INTEGER,source TEXT,src_mtime INTEGER NOT NULL DEFAULT 0);",
        ).unwrap();
        for (layer, key, m) in [
            ("L4", "A-1", 1000),
            ("L4", "A-2", 2000),
            ("L3", "L3-1", 3000),
            ("AUX", "README", 5000),
        ] {
            c.execute(
                "INSERT INTO library_index(layer,key,title,chars,source,src_mtime) VALUES(?1,?2,'',1,'s',?3)",
                rusqlite::params![layer, key, m],
            ).unwrap();
        }
        c
    }

    #[test]
    fn scan_json_keys_parsed() {
        let raw = r#"{"probe_items":[{"key":"K1"},{"key":"K2"},{"nope":1}]}"#;
        assert_eq!(keys_from_scan_json(raw).unwrap(), vec!["K1", "K2"]);
        assert!(keys_from_scan_json("not json").is_err());
        assert!(keys_from_scan_json("{}").is_err(), "缺 probe_items 应报错");
    }

    #[test]
    fn id_list_ignores_blank_and_comments() {
        let raw = "CHAR-1\n\n# 注释\n  CHAR-2  \n";
        assert_eq!(keys_from_id_list(raw), vec!["CHAR-1", "CHAR-2"]);
    }

    #[test]
    fn since_filters_by_mtime_and_layer() {
        let conn = mem_conn();
        let (keys, degraded) = keys_since(&conn, 2000, false).unwrap();
        assert!(!degraded);
        // src_mtime>=2000 且 L2/L3/L4：A-2(2000), L3-1(3000)；AUX 被层过滤掉
        // ORDER BY layer,key 为字符串序（L3 在 L4 前），故只比较集合
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(sorted, vec!["A-2", "L3-1"]);
    }

    #[test]
    fn since_all_layers_includes_aux() {
        let conn = mem_conn();
        let (keys, _) = keys_since(&conn, 2000, true).unwrap();
        assert!(keys.contains(&"README".to_string()), "--all-layers 应含 AUX");
    }

    #[test]
    fn since_all_zero_mtime_degrades_to_full() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE library_index(layer TEXT,key TEXT,title TEXT,chars INTEGER,source TEXT,src_mtime INTEGER NOT NULL DEFAULT 0);",
        ).unwrap();
        c.execute("INSERT INTO library_index(layer,key,title,chars,source,src_mtime) VALUES('L4','X','',1,'s',0)", []).unwrap();
        let (keys, degraded) = keys_since(&c, 123, false).unwrap();
        assert!(degraded, "全 0 mtime → 降级全扫");
        assert_eq!(keys, vec!["X"], "降级时返回全扫描域");
    }

    #[test]
    fn since_missing_column_degrades() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE library_index(layer TEXT,key TEXT,title TEXT,chars INTEGER,source TEXT);").unwrap();
        c.execute("INSERT INTO library_index(layer,key,title,chars,source) VALUES('L4','Y','',1,'s')", []).unwrap();
        let (keys, degraded) = keys_since(&c, 1, false).unwrap();
        assert!(degraded, "缺 src_mtime 列 → 降级");
        assert_eq!(keys, vec!["Y"]);
    }

    #[test]
    fn batch_report_counts_three_source_kinds() {
        let conn = mem_conn();
        let lib = std::env::temp_dir();
        // since 模式
        let r = build_batch_report(&conn, &lib, &BatchSource::Since(2000), false, true, None).unwrap();
        assert_eq!(r["counts"]["batch_size"], 2);
        assert_eq!(r["source_kind"], "since");
        assert_eq!(r["dry_run"], true);
        // id-list 模式
        let f = lib.join(format!("wg_batch_{}.txt", std::process::id()));
        std::fs::write(&f, "K1\nK2\nK3\n").unwrap();
        let r2 = build_batch_report(
            &conn, &lib, &BatchSource::IdList(f.to_string_lossy().to_string()), false, true, None,
        ).unwrap();
        assert_eq!(r2["counts"]["batch_size"], 3);
        assert_eq!(r2["source_kind"], "id-list");
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn batch_report_from_scan_file() {
        let conn = mem_conn();
        let lib = std::env::temp_dir();
        let f = lib.join(format!("wg_scan_{}.json", std::process::id()));
        std::fs::write(&f, r#"{"probe_items":[{"key":"S1"},{"key":"S2"}]}"#).unwrap();
        let r = build_batch_report(
            &conn, &lib, &BatchSource::FromScan(f.to_string_lossy().to_string()), false, true, None,
        ).unwrap();
        assert_eq!(r["counts"]["batch_size"], 2);
        assert_eq!(r["source_kind"], "from-scan");
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn batch_tag_filter_selects_keys() {
        use crate::governance::note_tag_store as nts;
        let conn = mem_conn();
        nts::add_tags(&conn, "A-2", &["剧情关键".into()], "manual").unwrap();
        let lib = std::env::temp_dir();
        let f = lib.join(format!("wg_batch_tag_{}.txt", std::process::id()));
        std::fs::write(&f, "A-1\nA-2\nL3-1\n").unwrap();
        let filter = nts::TagFilter::build(&["剧情关键".into()], &[]).unwrap();
        let r = build_batch_report(
            &conn, &lib, &BatchSource::IdList(f.to_string_lossy().to_string()),
            false, true, Some(&filter),
        ).unwrap();
        assert_eq!(r["counts"]["batch_size"], 1, "仅 A-2 带标签");
        assert_eq!(r["counts"]["tag_filtered"], 2);
        assert_eq!(r["keys"][0], "A-2");
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn slice_keys_partitions_without_overlap() {
        let keys: Vec<String> = (0..10).map(|i| format!("K{i}")).collect();
        // 4 片：按 len*i/m 切（10 条 → 2/3/2/3）
        let s1 = slice_keys(keys.clone(), 1, 4);
        let s2 = slice_keys(keys.clone(), 2, 4);
        let s3 = slice_keys(keys.clone(), 3, 4);
        let s4 = slice_keys(keys.clone(), 4, 4);
        assert_eq!(s1.len() + s2.len() + s3.len() + s4.len(), 10, "各片不重不漏");
        let mut all: Vec<String> = vec![];
        all.extend(s1); all.extend(s2); all.extend(s3); all.extend(s4);
        assert_eq!(all, keys, "拼接后应还原全集且顺序稳定");
    }

    #[test]
    fn slice_keys_edge_cases() {
        assert!(slice_keys(vec![], 1, 4).is_empty(), "空输入安全");
        assert!(slice_keys(vec!["A".into()], 1, 0).is_empty(), "m=0 安全");
        // 3 条切 2 片 → 1/2（按 len*i/m：start=0,end=1 / start=1,end=3）
        assert_eq!(slice_keys(vec!["A".into(), "B".into(), "C".into()], 1, 2), vec!["A"]);
        assert_eq!(slice_keys(vec!["A".into(), "B".into(), "C".into()], 2, 2), vec!["B", "C"]);
    }

    #[test]
    fn batch_report_slice_subsets_keys() {
        let conn = mem_conn();
        let lib = std::env::temp_dir();
        // All 源（L4 A-1,A-2 + L3 L3-1；AUX 被层过滤），切 2 片取第 1 片
        let r = build_batch_report_sliced(&conn, &lib, &BatchSource::All, false, true, None, Some((1, 2))).unwrap();
        assert_eq!(r["source_kind"], "all");
        assert_eq!(r["counts"]["slice"]["n"], 1);
        assert_eq!(r["counts"]["slice"]["m"], 2);
        let full = build_batch_report_sliced(&conn, &lib, &BatchSource::All, false, true, None, None).unwrap();
        assert!(r["counts"]["batch_size"].as_u64().unwrap() < full["counts"]["batch_size"].as_u64().unwrap());
    }
}