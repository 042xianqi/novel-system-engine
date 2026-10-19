//! 人工标记体系（world-gov）—— 标签 + 自由备注 + 复核状态。
//!
//! # 定位
//! 给世界观条目「贴便利贴」：多个标签 + 一条最新备注。标签可作为
//! scan / inspect-batch / probe-export / audit-export 的筛选条件。
//!
//! # 存储（复用 novel.db，**与主索引完全隔离**）
//! - `item_tag (id, key, tag, created_ts, source)`：一个条目可多标签；`UNIQUE(key,tag)` 去重。
//! - `item_note (id, key, status, content, created_ts, modified_ts)`：一个条目只有一条最新备注。
//!
//! # 铁则
//! - ❌ 绝不写入/修改 `library_index`、不触碰源文件。
//! - ✅ `CREATE TABLE IF NOT EXISTS` 全幂等；表不存在才创建。
//! - ✅ 所有 SQL 走绑定参数；输入严格 sanitize（长度/控制字符/路径分隔）。
use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

/// 标签最大字符数（按 char 计，非字节）。
pub const MAX_TAG_CHARS: usize = 64;
/// 条目 key 最大字符数。
pub const MAX_KEY_CHARS: usize = 256;
/// 备注最大字符数。
pub const MAX_NOTE_CHARS: usize = 20000;
/// 复核状态枚举。
pub const NOTE_STATUSES: &[&str] = &["unreviewed", "ok", "suspect", "false_positive", "critical"];
/// 标签来源枚举：manual（人工）/ reserved（预留给后续 AI 输出）。
pub const TAG_SOURCES: &[&str] = &["manual", "reserved"];

const SCHEMA: &str = r#"
-- 人工标记体系（2026-10-14）—— 幂等；与主索引 library_index 完全隔离。
CREATE TABLE IF NOT EXISTS item_tag (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    key        TEXT NOT NULL,
    tag        TEXT NOT NULL,
    created_ts INTEGER NOT NULL,
    source     TEXT NOT NULL DEFAULT 'manual',
    UNIQUE(key, tag)
);
CREATE INDEX IF NOT EXISTS idx_item_tag_tag ON item_tag(tag, key);
CREATE TABLE IF NOT EXISTS item_note (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    key         TEXT NOT NULL UNIQUE,
    status      TEXT NOT NULL DEFAULT 'unreviewed',
    content     TEXT NOT NULL DEFAULT '',
    created_ts  INTEGER NOT NULL,
    modified_ts INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_item_note_status ON item_note(status);
"#;

/// 幂等建表（不存在才创建）。
pub fn ensure_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA)?;
    Ok(())
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn has_control(s: &str) -> bool {
    s.chars().any(|c| c.is_control())
}

/// 条目 key sanitize：trim；非空；≤256 char；禁控制字符/路径分隔。
pub fn sanitize_key(key: &str) -> Result<String> {
    let k = key.trim();
    if k.is_empty() {
        bail!("条目 key 不能为空");
    }
    if k.chars().count() > MAX_KEY_CHARS {
        bail!("条目 key 过长（>{} 字符）", MAX_KEY_CHARS);
    }
    if has_control(k) {
        bail!("条目 key 含控制字符（换行/制表等）");
    }
    if k.contains('/') || k.contains('\\') {
        bail!("条目 key 不得含路径分隔符");
    }
    Ok(k.to_string())
}

/// 标签 sanitize：trim；非空；≤64 char；禁控制字符/路径分隔。
pub fn sanitize_tag(tag: &str) -> Result<String> {
    let t = tag.trim();
    if t.is_empty() {
        bail!("标签不能为空");
    }
    if t.chars().count() > MAX_TAG_CHARS {
        bail!("标签过长（>{} 字符）：{}", MAX_TAG_CHARS, t);
    }
    if has_control(t) {
        bail!("标签含控制字符（换行/制表等）");
    }
    if t.contains('/') || t.contains('\\') {
        bail!("标签不得含路径分隔符");
    }
    Ok(t.to_string())
}

/// 备注内容 sanitize：禁 NUL 与非法控制字符；允许换行/回车/制表；≤20000 char。
pub fn sanitize_content(content: &str) -> Result<String> {
    if content.contains('\0') {
        bail!("备注含 NUL 字节");
    }
    if content
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\r' && c != '\t')
    {
        bail!("备注含非法控制字符");
    }
    if content.chars().count() > MAX_NOTE_CHARS {
        bail!("备注过长（>{} 字符）", MAX_NOTE_CHARS);
    }
    Ok(content.to_string())
}

/// 复核状态校验（trim；须在枚举内）。
pub fn validate_status(status: &str) -> Result<String> {
    let s = status.trim();
    if s.is_empty() {
        bail!("状态不能为空");
    }
    if !NOTE_STATUSES.contains(&s) {
        bail!("未知状态 '{}'（合法：{}）", s, NOTE_STATUSES.join(" | "));
    }
    Ok(s.to_string())
}

// ────────────────────────────────── 标签 CRUD

/// 加标签：返回 (新增数, 已存在跳过数)。
pub fn add_tags(conn: &Connection, key: &str, tags: &[String], source: &str) -> Result<(usize, usize)> {
    ensure_tables(conn)?;
    let k = sanitize_key(key)?;
    if !TAG_SOURCES.contains(&source) {
        bail!("未知标签来源 '{}'（合法：{}）", source, TAG_SOURCES.join(" | "));
    }
    let ts = now_secs();
    let mut inserted = 0usize;
    let mut skipped = 0usize;
    for raw in tags {
        let t = sanitize_tag(raw)?;
        let n = conn.execute(
            "INSERT OR IGNORE INTO item_tag(key,tag,created_ts,source) VALUES(?1,?2,?3,?4)",
            params![k, t, ts, source],
        )?;
        if n > 0 {
            inserted += 1;
        } else {
            skipped += 1;
        }
    }
    Ok((inserted, skipped))
}

/// 删某条目的某个标签：返回删除行数（0 = 本就没有）。
pub fn rm_tag(conn: &Connection, key: &str, tag: &str) -> Result<usize> {
    ensure_tables(conn)?;
    let k = sanitize_key(key)?;
    let t = sanitize_tag(tag)?;
    let n = conn.execute("DELETE FROM item_tag WHERE key=?1 AND tag=?2", params![k, t])?;
    Ok(n)
}

fn tag_row(key: String, tag: String, source: String, created_ts: i64) -> Value {
    json!({"key": key, "tag": tag, "source": source, "created_ts": created_ts})
}

/// 列标签（可按键/按标签过滤）。
pub fn list_tags(conn: &Connection, key: Option<&str>, tag: Option<&str>) -> Result<Vec<Value>> {
    ensure_tables(conn)?;
    let key = match key {
        Some(k) if !k.trim().is_empty() => Some(sanitize_key(k)?),
        _ => None,
    };
    let tag = match tag {
        Some(t) if !t.trim().is_empty() => Some(sanitize_tag(t)?),
        _ => None,
    };
    let mut sql = String::from("SELECT key,tag,source,created_ts FROM item_tag WHERE 1=1");
    if key.is_some() {
        sql.push_str(" AND key=?1");
    }
    if tag.is_some() {
        sql.push_str(if key.is_some() { " AND tag=?2" } else { " AND tag=?1" });
    }
    sql.push_str(" ORDER BY key,tag");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows: Vec<Value> = Vec::new();
    match (&key, &tag) {
        (Some(k), Some(t)) => {
            let it = stmt.query_map(params![k, t], |r| {
                Ok(tag_row(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?;
            for x in it.filter_map(|v| v.ok()) {
                rows.push(x);
            }
        }
        (Some(k), None) => {
            let it = stmt.query_map(params![k], |r| {
                Ok(tag_row(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?;
            for x in it.filter_map(|v| v.ok()) {
                rows.push(x);
            }
        }
        (None, Some(t)) => {
            let it = stmt.query_map(params![t], |r| {
                Ok(tag_row(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?;
            for x in it.filter_map(|v| v.ok()) {
                rows.push(x);
            }
        }
        (None, None) => {
            let it = stmt.query_map([], |r| {
                Ok(tag_row(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?;
            for x in it.filter_map(|v| v.ok()) {
                rows.push(x);
            }
        }
    }
    Ok(rows)
}

// ────────────────────────────────── 标签筛选器

/// 统一标签筛选条件：`with` 全含（AND），`without` 全不含（NOT）。
#[derive(Debug, Clone, Default)]
pub struct TagFilter {
    pub with: Vec<String>,
    pub without: Vec<String>,
}

impl TagFilter {
    /// 构造并 sanitize（trim）。空串忽略。
    pub fn build(with: &[String], without: &[String]) -> Result<Self> {
        let mut w = Vec::new();
        for t in with {
            if t.trim().is_empty() {
                continue;
            }
            w.push(sanitize_tag(t)?);
        }
        let mut wo = Vec::new();
        for t in without {
            if t.trim().is_empty() {
                continue;
            }
            wo.push(sanitize_tag(t)?);
        }
        Ok(TagFilter { with: w, without: wo })
    }
    pub fn is_empty(&self) -> bool {
        self.with.is_empty() && self.without.is_empty()
    }
    pub fn describe(&self) -> Value {
        json!({"with_tag": self.with, "without_tag": self.without})
    }
}

/// 计算满足标签条件的 key 集合；筛选器为空时返回 `None`（表示「不过滤，全放行」）。
pub fn keys_matching(conn: &Connection, filter: &TagFilter) -> Result<Option<HashSet<String>>> {
    if filter.is_empty() {
        return Ok(None);
    }
    ensure_tables(conn)?;
    let mut map: HashMap<String, HashSet<String>> = HashMap::new();
    let mut stmt = conn.prepare("SELECT key,tag FROM item_tag")?;
    let it = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for x in it.filter_map(|v| v.ok()) {
        map.entry(x.0).or_default().insert(x.1);
    }
    let out: HashSet<String> = map
        .into_iter()
        .filter(|(_, tags)| {
            filter.with.iter().all(|w| tags.contains(w))
                && filter.without.iter().all(|w| !tags.contains(w))
        })
        .map(|(k, _)| k)
        .collect();
    Ok(Some(out))
}

/// 单 key 是否通过标签筛选（无筛选器恒 true）。
pub fn key_allowed(conn: &Connection, key: &str, filter: &TagFilter) -> Result<bool> {
    match keys_matching(conn, filter)? {
        None => Ok(true),
        Some(set) => Ok(set.contains(key)),
    }
}

// ────────────────────────────────── 备注 CRUD

fn note_row(row: &rusqlite::Row) -> rusqlite::Result<Value> {
    Ok(json!({
        "key": row.get::<_, String>(0)?,
        "status": row.get::<_, String>(1)?,
        "content": row.get::<_, String>(2)?,
        "created_ts": row.get::<_, i64>(3)?,
        "modified_ts": row.get::<_, i64>(4)?,
    }))
}

/// 设置（新建或部分更新）某条目备注；至少提供 status/content 之一。
pub fn set_note(
    conn: &Connection,
    key: &str,
    status: Option<&str>,
    content: Option<&str>,
) -> Result<Value> {
    ensure_tables(conn)?;
    let k = sanitize_key(key)?;
    if status.is_none() && content.is_none() {
        bail!("note set 至少提供 --status 或 --content");
    }
    let now = now_secs();
    let existing: Option<i64> = conn
        .query_row("SELECT id FROM item_note WHERE key=?1", params![k], |r| r.get(0))
        .optional()?;
    match existing {
        None => {
            let st = match status {
                Some(s) => validate_status(s)?,
                None => "unreviewed".to_string(),
            };
            let ct = match content {
                Some(c) => sanitize_content(c)?,
                None => String::new(),
            };
            conn.execute(
                "INSERT INTO item_note(key,status,content,created_ts,modified_ts) VALUES(?1,?2,?3,?4,?4)",
                params![k, st, ct, now],
            )?;
        }
        Some(_) => {
            if let Some(s) = status {
                let st = validate_status(s)?;
                conn.execute(
                    "UPDATE item_note SET status=?1, modified_ts=?2 WHERE key=?3",
                    params![st, now, k],
                )?;
            }
            if let Some(c) = content {
                let ct = sanitize_content(c)?;
                conn.execute(
                    "UPDATE item_note SET content=?1, modified_ts=?2 WHERE key=?3",
                    params![ct, now, k],
                )?;
            }
        }
    }
    Ok(get_note(conn, &k)?.unwrap_or(json!(null)))
}

/// 取某条目备注（无则 None）。
pub fn get_note(conn: &Connection, key: &str) -> Result<Option<Value>> {
    ensure_tables(conn)?;
    let k = sanitize_key(key)?;
    let v = conn
        .query_row(
            "SELECT key,status,content,created_ts,modified_ts FROM item_note WHERE key=?1",
            params![k],
            note_row,
        )
        .optional()?;
    Ok(v)
}

/// 列备注（可按状态过滤）。
pub fn list_notes(conn: &Connection, status: Option<&str>) -> Result<Vec<Value>> {
    ensure_tables(conn)?;
    let st = match status {
        Some(s) if !s.trim().is_empty() => Some(validate_status(s)?),
        _ => None,
    };
    let mut rows: Vec<Value> = Vec::new();
    if let Some(s) = st {
        let mut stmt = conn.prepare(
            "SELECT key,status,content,created_ts,modified_ts FROM item_note WHERE status=?1 ORDER BY key",
        )?;
        let it = stmt.query_map(params![s], note_row)?;
        for x in it.filter_map(|v| v.ok()) {
            rows.push(x);
        }
    } else {
        let mut stmt = conn.prepare(
            "SELECT key,status,content,created_ts,modified_ts FROM item_note ORDER BY key",
        )?;
        let it = stmt.query_map([], note_row)?;
        for x in it.filter_map(|v| v.ok()) {
            rows.push(x);
        }
    }
    Ok(rows)
}

/// 取出全部注解（key → {tags, note}），供报告层一次性构建索引。
pub fn all_annotations(conn: &Connection) -> Result<HashMap<String, Value>> {
    ensure_tables(conn)?;
    let mut out: HashMap<String, Value> = HashMap::new();
    let mut stmt = conn.prepare("SELECT key,tag FROM item_tag ORDER BY key,tag")?;
    let it = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for x in it.filter_map(|v| v.ok()) {
        let e = out.entry(x.0).or_insert_with(|| json!({"tags": [], "note": Value::Null}));
        if let Some(arr) = e["tags"].as_array_mut() {
            arr.push(json!(x.1));
        }
    }
    let mut stmt = conn.prepare(
        "SELECT key,status,content,created_ts,modified_ts FROM item_note",
    )?;
    let it = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, note_row(r)?)))?;
    for x in it.filter_map(|v| v.ok()) {
        let e = out.entry(x.0).or_insert_with(|| json!({"tags": [], "note": Value::Null}));
        e["note"] = x.1;
    }
    Ok(out)
}

/// 取一批 key 的标签 + 备注注解（供报告/prompt 附带）。
pub fn annotations(conn: &Connection, keys: &[String]) -> Result<HashMap<String, Value>> {
    ensure_tables(conn)?;
    let mut out: HashMap<String, Value> = HashMap::new();
    for k in keys {
        out.insert(k.clone(), json!({"tags": [], "note": Value::Null}));
    }
    let mut stmt = conn.prepare("SELECT key,tag FROM item_tag ORDER BY key,tag")?;
    let it = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for x in it.filter_map(|v| v.ok()) {
        if let Some(e) = out.get_mut(&x.0) {
            if let Some(arr) = e["tags"].as_array_mut() {
                arr.push(json!(x.1));
            }
        }
    }
    let mut stmt = conn.prepare(
        "SELECT key,status,content,created_ts,modified_ts FROM item_note",
    )?;
    let it = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, note_row(r)?)))?;
    for x in it.filter_map(|v| v.ok()) {
        if let Some(e) = out.get_mut(&x.0) {
            e["note"] = x.1;
        }
    }
    Ok(out)
}

// ────────────────────────────────── 导出 / 导入

/// 导出全部标签库（JSON）。
pub fn export_tags(conn: &Connection) -> Result<Value> {
    let rows = list_tags(conn, None, None)?;
    Ok(json!({
        "version": 1,
        "kind": "tags",
        "exported_ts": now_secs(),
        "count": rows.len(),
        "tags": rows,
    }))
}

/// 导出全部备注库（JSON）。
pub fn export_notes(conn: &Connection) -> Result<Value> {
    let rows = list_notes(conn, None)?;
    Ok(json!({
        "version": 1,
        "kind": "notes",
        "exported_ts": now_secs(),
        "count": rows.len(),
        "notes": rows,
    }))
}

/// 导入标签库。`force`：覆盖（先删除被导入 key 的现有标签，再写入）。
/// `dry_run`：只统计不落盘。
pub fn import_tags(conn: &Connection, raw: &str, dry_run: bool, force: bool) -> Result<Value> {
    ensure_tables(conn)?;
    let v: Value = serde_json::from_str(raw)
        .map_err(|e| anyhow::anyhow!("导入文件不是合法 JSON：{e}"))?;
    let arr = v
        .get("tags")
        .and_then(|x| x.as_array())
        .ok_or_else(|| anyhow::anyhow!("导入文件缺少 tags 数组（kind=tags）"))?;
    let ts = now_secs();
    let mut inserted = 0usize;
    let mut skipped = 0usize;
    let mut rejected: Vec<String> = Vec::new();
    if force {
        let mut force_keys: HashSet<String> = HashSet::new();
        for it in arr {
            if let Some(k) = it.get("key").and_then(|x| x.as_str()) {
                if let Ok(kk) = sanitize_key(k) {
                    force_keys.insert(kk);
                }
            }
        }
        if !dry_run {
            for k in &force_keys {
                conn.execute("DELETE FROM item_tag WHERE key=?1", params![k])?;
            }
        }
    }
    for it in arr {
        let k = match it.get("key").and_then(|x| x.as_str()) {
            Some(k) => k,
            None => {
                rejected.push("缺 key".to_string());
                continue;
            }
        };
        let t = match it.get("tag").and_then(|x| x.as_str()) {
            Some(t) => t,
            None => {
                rejected.push(format!("{k}: 缺 tag"));
                continue;
            }
        };
        let kk = match sanitize_key(k) {
            Ok(x) => x,
            Err(e) => {
                rejected.push(format!("{k}: {e}"));
                continue;
            }
        };
        let tt = match sanitize_tag(t) {
            Ok(x) => x,
            Err(e) => {
                rejected.push(format!("{k}/{t}: {e}"));
                continue;
            }
        };
        let src = it.get("source").and_then(|x| x.as_str()).unwrap_or("manual");
        let src = if TAG_SOURCES.contains(&src) { src } else { "manual" };
        if dry_run {
            inserted += 1;
            continue;
        }
        let n = conn.execute(
            "INSERT OR IGNORE INTO item_tag(key,tag,created_ts,source) VALUES(?1,?2,?3,?4)",
            params![kk, tt, ts, src],
        )?;
        if n > 0 {
            inserted += 1;
        } else {
            skipped += 1;
        }
    }
    Ok(json!({
        "command": "world-gov tag import",
        "dry_run": dry_run,
        "force": force,
        "total_in_file": arr.len(),
        "inserted": inserted,
        "skipped_existing": skipped,
        "rejected": rejected,
        "note": "导入仅动 item_tag；绝不写入 library_index 或源文件。",
    }))
}

/// 导入备注库。默认不覆盖已存在备注（跳过）；`force` 覆盖。`dry_run` 只统计。
pub fn import_notes(conn: &Connection, raw: &str, dry_run: bool, force: bool) -> Result<Value> {
    ensure_tables(conn)?;
    let v: Value = serde_json::from_str(raw)
        .map_err(|e| anyhow::anyhow!("导入文件不是合法 JSON：{e}"))?;
    let arr = v
        .get("notes")
        .and_then(|x| x.as_array())
        .ok_or_else(|| anyhow::anyhow!("导入文件缺少 notes 数组（kind=notes）"))?;
    let ts = now_secs();
    let mut inserted = 0usize;
    let mut updated = 0usize;
    let mut skipped = 0usize;
    let mut rejected: Vec<String> = Vec::new();
    for it in arr {
        let k = match it.get("key").and_then(|x| x.as_str()) {
            Some(k) => k,
            None => {
                rejected.push("缺 key".to_string());
                continue;
            }
        };
        let kk = match sanitize_key(k) {
            Ok(x) => x,
            Err(e) => {
                rejected.push(format!("{k}: {e}"));
                continue;
            }
        };
        let st = match it.get("status").and_then(|x| x.as_str()) {
            Some(s) => match validate_status(s) {
                Ok(x) => x,
                Err(e) => {
                    rejected.push(format!("{k}: {e}"));
                    continue;
                }
            },
            None => "unreviewed".to_string(),
        };
        let ct = match it.get("content").and_then(|x| x.as_str()) {
            Some(c) => match sanitize_content(c) {
                Ok(x) => x,
                Err(e) => {
                    rejected.push(format!("{k}: {e}"));
                    continue;
                }
            },
            None => String::new(),
        };
        let exists: Option<i64> = conn
            .query_row("SELECT id FROM item_note WHERE key=?1", params![kk], |r| r.get(0))
            .optional()?;
        if exists.is_some() && !force {
            skipped += 1;
            continue;
        }
        if dry_run {
            if exists.is_some() {
                updated += 1;
            } else {
                inserted += 1;
            }
            continue;
        }
        if exists.is_some() {
            conn.execute(
                "UPDATE item_note SET status=?1, content=?2, modified_ts=?3 WHERE key=?4",
                params![st, ct, ts, kk],
            )?;
            updated += 1;
        } else {
            conn.execute(
                "INSERT INTO item_note(key,status,content,created_ts,modified_ts) VALUES(?1,?2,?3,?4,?4)",
                params![kk, st, ct, ts],
            )?;
            inserted += 1;
        }
    }
    Ok(json!({
        "command": "world-gov note import",
        "dry_run": dry_run,
        "force": force,
        "total_in_file": arr.len(),
        "inserted": inserted,
        "updated": updated,
        "skipped_existing": skipped,
        "rejected": rejected,
        "note": "导入仅动 item_note；绝不写入 library_index 或源文件。",
    }))
}

// ────────────────────────────────── 自检辅助

/// 结构完整性 + 脏数据检查（供 self-check 调用；**只读**）。
pub fn table_check(conn: &Connection) -> Result<Value> {
    let mut missing: Vec<String> = Vec::new();
    for t in ["item_tag", "item_note"] {
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                params![t],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if n == 0 {
            missing.push(t.to_string());
        }
    }
    if !missing.is_empty() {
        return Ok(json!({
            "ok": false, "missing_tables": missing,
            "hint": "缺表；跑任意 world-gov 命令（open_gov 幂等建表）或 note/tag 命令即可创建",
        }));
    }
    let cols_tag: Vec<String> = conn
        .prepare("PRAGMA table_info(item_tag)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .filter_map(|v| v.ok())
        .collect();
    let cols_note: Vec<String> = conn
        .prepare("PRAGMA table_info(item_note)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .filter_map(|v| v.ok())
        .collect();
    let need_tag = ["key", "tag", "created_ts", "source"];
    let need_note = ["key", "status", "content", "created_ts", "modified_ts"];
    let miss_tag: Vec<&str> = need_tag.iter().copied().filter(|c| !cols_tag.iter().any(|x| x == c)).collect();
    let miss_note: Vec<&str> = need_note.iter().copied().filter(|c| !cols_note.iter().any(|x| x == c)).collect();
    if !miss_tag.is_empty() || !miss_note.is_empty() {
        return Ok(json!({
            "ok": false,
            "missing_cols": {"item_tag": miss_tag, "item_note": miss_note},
        }));
    }
    let tags = list_tags(conn, None, None)?;
    let total_tag = tags.len();
    let mut dirty_tags: Vec<Value> = Vec::new();
    for t in &tags {
        let tg = t["tag"].as_str().unwrap_or("");
        let src = t["source"].as_str().unwrap_or("");
        if tg.trim().is_empty() || tg.chars().count() > MAX_TAG_CHARS || has_control(tg) {
            dirty_tags.push(json!({"key": t["key"], "tag": tg, "why": "dirty_tag"}));
        } else if !TAG_SOURCES.contains(&src) {
            dirty_tags.push(json!({"key": t["key"], "tag": tg, "why": "bad_source"}));
        }
    }
    let notes = list_notes(conn, None)?;
    let total_note = notes.len();
    let mut dirty_notes: Vec<Value> = Vec::new();
    for n in &notes {
        let s = n["status"].as_str().unwrap_or("");
        if !NOTE_STATUSES.contains(&s) {
            dirty_notes.push(json!({"key": n["key"], "status": s, "why": "bad_status"}));
        }
    }
    let dirty = dirty_tags.len() + dirty_notes.len();
    Ok(json!({
        "ok": dirty == 0,
        "tables_present": true,
        "tags": total_tag,
        "notes": total_note,
        "dirty_tags": dirty_tags,
        "dirty_notes": dirty_notes,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        ensure_tables(&c).unwrap();
        c
    }

    #[test]
    fn tables_are_isolated_and_idempotent() {
        let c = Connection::open_in_memory().unwrap();
        ensure_tables(&c).unwrap();
        ensure_tables(&c).unwrap(); // 幂等
        let names: Vec<String> = c
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .filter_map(|v| v.ok())
            .collect();
        assert!(names.contains(&"item_tag".to_string()));
        assert!(names.contains(&"item_note".to_string()));
        assert!(!names.contains(&"library_index".to_string()), "绝不创建/触碰主索引");
    }

    #[test]
    fn add_dedupe_and_rm() {
        let c = mem();
        let (i1, s1) = add_tags(&c, "CHAR-10-001", &["时间线可疑".into(), "关键人物".into()], "manual").unwrap();
        assert_eq!((i1, s1), (2, 0));
        let (i2, s2) = add_tags(&c, "CHAR-10-001", &["时间线可疑".into()], "manual").unwrap();
        assert_eq!((i2, s2), (0, 1), "重复标签跳过");
        assert_eq!(list_tags(&c, Some("CHAR-10-001"), None).unwrap().len(), 2);
        assert_eq!(rm_tag(&c, "CHAR-10-001", "时间线可疑").unwrap(), 1);
        assert_eq!(rm_tag(&c, "CHAR-10-001", "时间线可疑").unwrap(), 0);
        assert_eq!(list_tags(&c, Some("CHAR-10-001"), None).unwrap().len(), 1);
    }

    #[test]
    fn sanitize_rejects_dirty() {
        assert!(sanitize_tag("").is_err());
        assert!(sanitize_tag("   ").is_err());
        assert!(sanitize_tag(&"x".repeat(65)).is_err());
        assert!(sanitize_tag("坏/标签").is_err());
        assert!(sanitize_tag("带\n换行").is_err());
        assert!(sanitize_tag("正常标签").is_ok());
        assert!(sanitize_key("").is_err());
        assert!(sanitize_key("a/b").is_err());
        assert!(sanitize_key("CHAR-10-001").is_ok());
        assert!(sanitize_key("卷01·卷主体故事").is_ok(), "中文/· 合法");
        assert!(sanitize_content("多行\n备注\t带制表").is_ok());
        assert!(sanitize_content("含\0NUL").is_err());
        assert!(validate_status("suspect").is_ok());
        assert!(validate_status("nope").is_err());
    }

    #[test]
    fn note_set_get_partial_update() {
        let c = mem();
        let n = set_note(&c, "CHAR-1", Some("suspect"), Some("刻意反转，忽略年龄校验")).unwrap();
        assert_eq!(n["status"], "suspect");
        assert_eq!(n["content"], "刻意反转，忽略年龄校验");
        let n2 = set_note(&c, "CHAR-1", Some("ok"), None).unwrap();
        assert_eq!(n2["status"], "ok");
        assert_eq!(n2["content"], "刻意反转，忽略年龄校验");
        assert!(set_note(&c, "CHAR-1", None, None).is_err(), "空更新报错");
        assert_eq!(n2["created_ts"], n["created_ts"], "created_ts 不变");
    }

    #[test]
    fn note_list_by_status() {
        let c = mem();
        set_note(&c, "A", Some("suspect"), Some("x")).unwrap();
        set_note(&c, "B", Some("ok"), Some("y")).unwrap();
        set_note(&c, "C", Some("suspect"), Some("z")).unwrap();
        assert_eq!(list_notes(&c, Some("suspect")).unwrap().len(), 2);
        assert_eq!(list_notes(&c, None).unwrap().len(), 3);
        assert!(list_notes(&c, Some("bad")).is_err());
    }

    #[test]
    fn keys_matching_with_without() {
        let c = mem();
        add_tags(&c, "K1", &["待复核".into(), "剧情关键".into()], "manual").unwrap();
        add_tags(&c, "K2", &["待复核".into()], "manual").unwrap();
        add_tags(&c, "K3", &["已确认".into()], "manual").unwrap();
        let f = TagFilter::build(&["待复核".into()], &[]).unwrap();
        let mut got: Vec<String> = keys_matching(&c, &f).unwrap().unwrap().into_iter().collect();
        got.sort();
        assert_eq!(got, vec!["K1", "K2"]);
        let f = TagFilter::build(&["待复核".into()], &["剧情关键".into()]).unwrap();
        let got: Vec<String> = keys_matching(&c, &f).unwrap().unwrap().into_iter().collect();
        assert_eq!(got, vec!["K2"]);
        let f = TagFilter::default();
        assert!(keys_matching(&c, &f).unwrap().is_none());
        assert!(f.is_empty());
        let f = TagFilter::build(&["待复核".into(), "剧情关键".into()], &[]).unwrap();
        let got: Vec<String> = keys_matching(&c, &f).unwrap().unwrap().into_iter().collect();
        assert_eq!(got, vec!["K1"]);
    }

    #[test]
    fn export_import_roundtrip_and_dry_run() {
        let c = mem();
        add_tags(&c, "K1", &["x".into(), "y".into()], "manual").unwrap();
        let dump = export_tags(&c).unwrap();
        let raw = serde_json::to_string(&dump).unwrap();
        let c2 = mem();
        let r = import_tags(&c2, &raw, false, false).unwrap();
        assert_eq!(r["inserted"], 2);
        assert_eq!(list_tags(&c2, None, None).unwrap().len(), 2);
        let c3 = mem();
        let r = import_tags(&c3, &raw, true, false).unwrap();
        assert_eq!(r["dry_run"], true);
        assert_eq!(list_tags(&c3, None, None).unwrap().len(), 0, "dry-run 不得写");
    }

    #[test]
    fn import_rejects_dirty_and_force_overwrites_tags() {
        let c = mem();
        let raw = r#"{"kind":"tags","tags":[
            {"key":"K1","tag":"好标签"},
            {"key":"K1","tag":"坏/标签"},
            {"key":"","tag":"空key"},
            {"key":"K2","tag":"好标签"}
        ]}"#;
        let r = import_tags(&c, raw, false, false).unwrap();
        assert_eq!(r["inserted"], 2, "合法 2 条");
        assert_eq!(r["rejected"].as_array().unwrap().len(), 2, "脏 2 条被拒");
        add_tags(&c, "K1", &["旧标签".into()], "manual").unwrap();
        let raw2 = r#"{"kind":"tags","tags":[{"key":"K1","tag":"新标签"}]}"#;
        import_tags(&c, raw2, false, true).unwrap();
        let k1: Vec<String> = list_tags(&c, Some("K1"), None)
            .unwrap()
            .iter()
            .map(|v| v["tag"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(k1, vec!["新标签"], "force 覆盖后只剩新标签");
    }

    #[test]
    fn import_notes_skip_vs_force() {
        let c = mem();
        set_note(&c, "K1", Some("ok"), Some("原内容")).unwrap();
        let raw = r#"{"kind":"notes","notes":[{"key":"K1","status":"suspect","content":"新内容"}]}"#;
        let r = import_notes(&c, raw, false, false).unwrap();
        assert_eq!(r["skipped_existing"], 1, "默认不覆盖");
        assert_eq!(get_note(&c, "K1").unwrap().unwrap()["status"], "ok");
        let r = import_notes(&c, raw, false, true).unwrap();
        assert_eq!(r["updated"], 1, "--force 覆盖");
        assert_eq!(get_note(&c, "K1").unwrap().unwrap()["status"], "suspect");
        let bad = r#"{"kind":"notes","notes":[{"key":"K2","status":"nope","content":"x"}]}"#;
        let r = import_notes(&c, bad, false, false).unwrap();
        assert_eq!(r["rejected"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn annotations_include_tags_and_note() {
        let c = mem();
        add_tags(&c, "K1", &["a".into()], "manual").unwrap();
        set_note(&c, "K1", Some("critical"), Some("注意")).unwrap();
        let m = annotations(&c, &["K1".into(), "K2".into()]).unwrap();
        assert_eq!(m["K1"]["tags"], json!(["a"]));
        assert_eq!(m["K1"]["note"]["status"], "critical");
        assert_eq!(m["K2"]["tags"], json!([]));
        assert!(m["K2"]["note"].is_null());
    }

    #[test]
    fn table_check_detects_dirty_rows() {
        let c = mem();
        add_tags(&c, "K1", &["ok".into()], "manual").unwrap();
        set_note(&c, "K1", Some("ok"), Some("x")).unwrap();
        let r = table_check(&c).unwrap();
        assert_eq!(r["ok"], true);
        assert_eq!(r["tags"], 1);
        c.execute("UPDATE item_note SET status='garbage' WHERE key='K1'", []).unwrap();
        let r = table_check(&c).unwrap();
        assert_eq!(r["ok"], false);
        assert_eq!(r["dirty_notes"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn table_check_missing_tables() {
        let c = Connection::open_in_memory().unwrap();
        let r = table_check(&c).unwrap();
        assert_eq!(r["ok"], false);
        assert_eq!(r["missing_tables"].as_array().unwrap().len(), 2);
    }
}