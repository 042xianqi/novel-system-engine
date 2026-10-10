//! DB 层 —— `db/*.py` 的 Rust 移植（schema 初始化 + 知识灌库 + 只读查询辅助）。
//!
//! 事实源：`05_管线框架/db/`（init.py / load_knowledge.py / load_structure.py / tick.py）
//!
//! **修复的两处历史 bug（如实标注）**：
//!   1. `db/load_knowledge.py:34` 硬编码 `ROOT = "/sdcard/projects/系统/资料库"`（错路径）。
//!      Rust 版**全部路径由 `root` 参数注入**，不再硬编码。
//!   2. `db/emotion_schema.sql:566` 的 `CREATE TABLE kb_rules` **缺 `IF NOT EXISTS`**，
//!      与其他表不一致 → 二次执行会报错（非幂等）。Rust 版在装载 schema 时**自动补齐**。
//!
//! 安全：`open_readonly()` 供查询用；写操作只在显式 `open_rw()` 后发生。

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// 当前章 DB 句柄。
pub struct Db {
    pub con: Connection,
    pub root: PathBuf,
}

impl Db {
    /// 只读打开 `novel.db`（质检/查询场景，绝不写）。
    pub fn open_readonly(root: &Path) -> Result<Self> {
        let p = root.join("novel.db");
        let con = Connection::open_with_flags(
            &p,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )
        .with_context(|| format!("只读打开失败：{}", p.display()))?;
        Ok(Db { con, root: root.to_path_buf() })
    }

    /// 读写打开（灌库场景）。**调用方须确认目标不是用户真库**。
    pub fn open_rw(root: &Path) -> Result<Self> {
        let p = root.join("novel.db");
        let con = Connection::open(&p)
            .with_context(|| format!("打开失败：{}", p.display()))?;
        Ok(Db { con, root: root.to_path_buf() })
    }

    /// 读 schema.sql / emotion_schema.sql 原文（不执行）。
    pub fn schema_sql(&self) -> Result<Vec<(String, String)>> {
        let dir = self.root.join("05_管线框架").join("db");
        let mut out = Vec::new();
        for name in ["schema.sql", "emotion_schema.sql"] {
            let p = dir.join(name);
            if p.exists() {
                let t = std::fs::read_to_string(&p)
                    .with_context(|| format!("读不到 {}", p.display()))?;
                out.push((name.to_string(), t));
            }
        }
        Ok(out)
    }
}

/// 预处理 schema：剥 `#` 注释行、拆出 `ALTER TABLE`、**给缺 IF NOT EXISTS 的 CREATE TABLE 补上**。
///
/// 对齐 Python `init.load_schema`，并修复 `kb_rules` 的幂等 bug。
pub fn preprocess_schema(raw: &str) -> (String, Vec<String>) {
    let mut body = Vec::new();
    let mut alters = Vec::new();
    for line in raw.lines() {
        let t = line.trim_start();
        if t.starts_with('#') {
            continue;
        }
        if t.to_uppercase().starts_with("ALTER TABLE") {
            alters.push(line.to_string());
            continue;
        }
        // 修复：CREATE TABLE 缺 IF NOT EXISTS → 补上
        body.push(patch_create_if_not_exists(line));
    }
    (body.join("\n"), alters)
}

/// 单行修复：`CREATE TABLE foo (` → `CREATE TABLE IF NOT EXISTS foo (`。
/// 已含 IF NOT EXISTS 则原样返回（幂等）。
fn patch_create_if_not_exists(line: &str) -> String {
    let up = line.to_uppercase();
    let Some(pos) = up.find("CREATE TABLE") else {
        return line.to_string();
    };
    let after = &line[pos + "CREATE TABLE".len()..];
    if after.trim_start().to_uppercase().starts_with("IF NOT EXISTS") {
        return line.to_string();
    }
    format!("{}CREATE TABLE IF NOT EXISTS{}", &line[..pos], after)
}

/// 幂等 ALTER ADD COLUMN（对齐 `_add_columns_idempotent`）。
pub fn add_columns_idempotent(con: &Connection, alters: &[String]) -> Result<usize> {
    let re = regex::Regex::new(r"(?i)^ALTER TABLE (\S+) ADD COLUMN (\S+)")?;
    let mut added = 0;
    for stmt in alters {
        let Some(c) = re.captures(stmt) else { continue };
        let table = c.get(1).unwrap().as_str();
        let col = c.get(2).unwrap().as_str();
        let mut exists = false;
        let mut st = con.prepare(&format!("PRAGMA table_info({table})"))?;
        let rows = st.query_map([], |r| r.get::<_, String>(1))?;
        for r in rows.flatten() {
            if r == col {
                exists = true;
            }
        }
        if !exists {
            con.execute(stmt, [])?;
            added += 1;
        }
    }
    Ok(added)
}

/// 建表（幂等）：执行两份 schema。
pub fn ensure_schema(root: &Path) -> Result<usize> {
    let db = Db::open_rw(root)?;
    let mut n = 0;
    for (_name, raw) in db.schema_sql()? {
        let (body, alters) = preprocess_schema(&raw);
        db.con.execute_batch(&body)?;
        n += add_columns_idempotent(&db.con, &alters)?;
    }
    Ok(n)
}

/// 表行数快照（体检用）。
pub fn table_counts(con: &Connection) -> Result<Value> {
    let mut out = serde_json::Map::new();
    let mut st = con.prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")?;
    let names: Vec<String> = st
        .query_map([], |r| r.get::<_, String>(0))?
        .flatten()
        .collect();
    drop(st);
    for t in names {
        let c: i64 = con
            .query_row(&format!("SELECT COUNT(*) FROM \"{t}\""), [], |r| r.get(0))
            .unwrap_or(-1);
        out.insert(t, json!(c));
    }
    Ok(Value::Object(out))
}

// ───────────────────────────── 知识灌库（对齐 load_knowledge.py）

/// 12 位 sha 前缀（对齐 `_sha12`）。
fn sha12(path: &Path) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let data = std::fs::read(path).unwrap_or_default();
    let mut h = DefaultHasher::new();
    data.hash(&mut h);
    format!("{:012x}", h.finish() & 0xffff_ffff_ffff)
}

fn jread(root: &Path, rel: &str) -> Option<Value> {
    let p = root.join("05_管线框架").join(rel);
    let t = std::fs::read_to_string(&p).ok()?;
    serde_json::from_str(&t).ok()
}

/// 灌 kb_knowledge（data/kb_knowledge.json）→ 行数。
pub fn load_kb_knowledge(con: &Connection, root: &Path) -> Result<usize> {
    const VERDICT_ENUM: [&str; 5] = ["canon", "drift", "deprecated", "flavor", "pending"];
    let Some(data) = jread(root, "data/kb_knowledge.json") else {
        return Ok(0);
    };
    let entries = data.get("entries").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let src_path = root.join("05_管线框架").join("data/kb_knowledge.json");
    let sha = sha12(&src_path);
    let mut n = 0;
    let mut st = con.prepare(
        "INSERT OR IGNORE INTO kb_knowledge
         (domain, card_code, canon, basis, verdict, layer, tag, source, version, source_card, source_sha)
         VALUES (?,?,?,?,?,?,?,?,?,?,?)",
    )?;
    for e in entries {
        let gs = |k: &str| e.get(k).and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        let domain = gs("domain");
        let canon = gs("canon");
        let verdict = gs("verdict");
        if domain.is_empty() || canon.is_empty() {
            continue;
        }
        if !VERDICT_ENUM.contains(&verdict.as_str()) {
            continue;
        }
        let card_code = gs("card_code"); // 域级定论存空串（保 UNIQUE 生效）
        let layer = gs("layer");
        let tag = gs("tag");
        let basis = jopt({
            let b = gs("basis");
            if b.is_empty() { None } else { Some(json!(b)) }
        });
        let source = jopt(Some({
            let s = gs("source");
            if s.is_empty() { json!("勘误知识") } else { json!(s) }
        }));
        let version = jopt({
            let v = gs("version");
            if v.is_empty() { None } else { Some(json!(v)) }
        });
        st.execute(rusqlite::params![
            domain, card_code, canon, basis, verdict, layer, tag, source, version,
            "data/kb_knowledge.json", sha
        ])?;
        n += 1;
    }
    Ok(n)
}

/// 灌 kb_quotes（data/kb_quotes.json）。
pub fn load_kquotes(con: &Connection, root: &Path) -> Result<usize> {
    let Some(data) = jread(root, "data/kb_quotes.json") else { return Ok(0) };
    let entries = entries_of(&data);
    let sha = sha12(&root.join("05_管线框架").join("data/kb_quotes.json"));
    let mut n = 0;
    let mut st = con.prepare(
        "INSERT OR IGNORE INTO kb_quotes
         (quote, source, meaning, scene_tag, emotion_tag, insert_pos, themes, civs, basis, source_card, source_sha)
         VALUES (?,?,?,?,?,?,?,?,?,?,?)",
    )?;
    for e in entries {
        let quote = e.get("quote").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        if quote.is_empty() {
            continue;
        }
        let opt = |k: &str| jopt(e.get(k).cloned());
        let jarr = |k: &str| jopt(e.get(k).map(|v| json!(v.to_string())));
        st.execute(rusqlite::params![
            quote, opt("source"), opt("meaning"), opt("scene_tag"), opt("emotion_tag"),
            opt("insert_pos"), jarr("themes"), jarr("civs"),
            "外部典库", "data/kb_quotes.json", sha
        ])?;
        n += 1;
    }
    Ok(n)
}

/// 灌 kb_anecdotes（data/kb_anecdotes.json）。
pub fn load_kanecdotes(con: &Connection, root: &Path) -> Result<usize> {
    let Some(data) = jread(root, "data/kb_anecdotes.json") else { return Ok(0) };
    let entries = entries_of(&data);
    let sha = sha12(&root.join("05_管线框架").join("data/kb_anecdotes.json"));
    let mut n = 0;
    let mut st = con.prepare(
        "INSERT OR IGNORE INTO kb_anecdotes
         (anecdote, source, moral, scene_tag, emotion_tag, insert_pos, domain, basis, source_card, source_sha)
         VALUES (?,?,?,?,?,?,?,?,?,?)",
    )?;
    for e in entries {
        let a = e.get("anecdote").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        if a.is_empty() {
            continue;
        }
        let opt = |k: &str| jopt(e.get(k).cloned());
        st.execute(rusqlite::params![
            a, opt("source"), opt("moral"), opt("scene_tag"), opt("emotion_tag"),
            opt("insert_pos"), opt("domain"), "外部典库", "data/kb_anecdotes.json", sha
        ])?;
        n += 1;
    }
    Ok(n)
}

/// 灌 kb_imagery（data/kb_imagery.json）。
pub fn load_kimagery(con: &Connection, root: &Path) -> Result<usize> {
    let Some(data) = jread(root, "data/kb_imagery.json") else { return Ok(0) };
    let entries = entries_of(&data);
    let sha = sha12(&root.join("05_管线框架").join("data/kb_imagery.json"));
    let mut n = 0;
    let mut st = con.prepare(
        "INSERT OR IGNORE INTO kb_imagery
         (imagery, literal, metaphor, emotion_tag, use_scene, basis, source_card, source_sha)
         VALUES (?,?,?,?,?,?,?,?)",
    )?;
    for e in entries {
        let img = e.get("imagery").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        if img.is_empty() {
            continue;
        }
        let opt = |k: &str| jopt(e.get(k).cloned());
        st.execute(rusqlite::params![
            img, opt("literal"), opt("metaphor"), opt("emotion_tag"), opt("use_scene"),
            "外部典库", "data/kb_imagery.json", sha
        ])?;
        n += 1;
    }
    Ok(n)
}

/// `serde_json::Value` → SQLite 可绑定的值（rusqlite 的 ToSql 不覆盖 Value）。
/// null → NULL；字符串 → TEXT；数字 → INTEGER/REAL；其余 → JSON 文本。
fn jopt(v: Option<Value>) -> Option<Box<dyn rusqlite::ToSql>> {
    let v = v?;
    match v {
        Value::Null => None,
        Value::String(s) => Some(Box::new(s)),
        Value::Bool(b) => Some(Box::new(if b { 1i64 } else { 0i64 })),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(Box::new(i))
            } else {
                Some(Box::new(n.as_f64().unwrap_or(0.0)))
            }
        }
        other => Some(Box::new(other.to_string())),
    }
}

fn entries_of(data: &Value) -> Vec<Value> {
    if let Some(arr) = data.get("entries").and_then(|v| v.as_array()) {
        arr.clone()
    } else if let Some(arr) = data.as_array() {
        arr.clone()
    } else {
        vec![]
    }
}

/// 一次性灌全部知识表，返回各表行数。
pub fn load_all_knowledge(con: &Connection, root: &Path) -> Result<Value> {
    Ok(json!({
        "kb_knowledge": load_kb_knowledge(con, root)?,
        "kb_quotes": load_kquotes(con, root)?,
        "kb_anecdotes": load_kanecdotes(con, root)?,
        "kb_imagery": load_kimagery(con, root)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_adds_if_not_exists() {
        assert_eq!(
            patch_create_if_not_exists("CREATE TABLE kb_rules ("),
            "CREATE TABLE IF NOT EXISTS kb_rules ("
        );
        // 已有 → 不变
        assert_eq!(
            patch_create_if_not_exists("CREATE TABLE IF NOT EXISTS x ("),
            "CREATE TABLE IF NOT EXISTS x ("
        );
        // 缩进保留
        assert_eq!(
            patch_create_if_not_exists("  CREATE TABLE y ("),
            "  CREATE TABLE IF NOT EXISTS y ("
        );
    }

    #[test]
    fn preprocess_strips_hash_and_splits_alters() {
        let raw = "# 注释\nCREATE TABLE kb_rules (a int);\nALTER TABLE materials ADD COLUMN tags TEXT;\n";
        let (body, alters) = preprocess_schema(raw);
        assert!(!body.contains('#'));
        assert!(body.contains("IF NOT EXISTS kb_rules"));
        assert_eq!(alters.len(), 1);
    }

    #[test]
    fn schema_is_idempotent_after_patch() {
        // 测试夹具（随 crate 一起入库）
        let p = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/emotion_schema.sql"
        ));
        assert!(p.exists(), "夹具缺失：{}", p.display());
        let raw = std::fs::read_to_string(p).unwrap();
        let (body, _) = preprocess_schema(&raw);
        // 建两次不应报错
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch(&body).expect("首次建表");
        con.execute_batch(&body).expect("二次建表应幂等（kb_rules 已补 IF NOT EXISTS）");
        let n: i64 = con
            .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='kb_rules'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }
}
