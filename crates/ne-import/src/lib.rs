//! ne-import：资料库 → SQLite 全量导入 + 全库重编号。
//!
//! # 背景：为什么必须"先导入全文"
//!
//! 实测（2026-10-10）：`materials.summary` **只是摘要**——
//! 506 张卡可比对，源 932,651 字 vs DB 6,742 字（**覆盖 0%**）。
//! 也就是说资料库的真实内容**只存在于 .md 文件**。
//! 因此"删源文档"之前，必须先把全文导入 `materials.content`。
//!
//! # 重编号必须与导入同批
//!
//! 全库有 **3447 处**交叉引用（`CHAR-99-001` 这类硬引用）。
//! 改编号会断链，所以映射表、引用改写、文件重命名必须一起做。
//!
//! 编号规范见 `docs/specs/编号规范-v3.md`。

use anyhow::{Context, Result};
use regex::Regex;
use rusqlite::{params, Connection};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub mod renumber;

/// 单个来源文件的扫描记录（未落库）。
#[derive(Debug, Clone)]
pub struct ScannedCard {
    pub rel_path: String,
    pub category: String,
    pub old_code: Option<String>,
    pub name: Option<String>,
    pub h1: Option<String>,
    pub content: String,
    pub content_chars: usize,
    pub sha: String,
}

/// 全库扫描结果。
#[derive(Debug, Default)]
pub struct ScanResult {
    pub cards: Vec<ScannedCard>,
    pub no_code: Vec<String>,
    pub root: String,
}

fn sha256_hex(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}

/// 统计"净字数"（去掉空白），用于对账。
fn net_chars(s: &str) -> usize {
    s.chars().filter(|c| !c.is_whitespace()).count()
}

/// 抽取 H1（首个 `# ` 行）。
fn find_h1(text: &str) -> Option<String> {
    text.lines()
        .map(|l| l.trim())
        .find(|l| l.starts_with("# "))
        .map(|l| l.trim_start_matches("# ").trim().to_string())
}

/// 从 H1 或文件名抽编号。
fn extract_code(h1: Option<&str>, stem: &str) -> Option<String> {
    // ⚠ 必须**精确**匹配 `PREFIX-纪元-序号` 或 `PREFIX-序号`。
    // 早期用 `(?:-\d{1,2}){1,2}` 会贪婪截断：`CHAR-99-001` → `CHAR-99-00`（丢末位）。
    let re = Regex::new(r"^([A-Z]{2,6}-\d{2}-\d{3}|[A-Z]{2,6}-\d{3})").ok()?;
    if let Some(h) = h1 {
        if let Some(c) = re.captures(h.trim()) {
            return Some(c.get(1).unwrap().as_str().to_string());
        }
    }
    re.captures(stem).map(|c| c.get(1).unwrap().as_str().to_string())
}

/// 从 `# CODE｜NAME` 抽名称。
fn extract_name(h1: Option<&str>) -> Option<String> {
    let h = h1?;
    h.split(['｜', '|']).nth(1).map(|s| s.trim().to_string())
}

/// 扫描 `02_资料库` 全量卡片（含正文全文）。
pub fn scan_library(lib_root: &Path) -> Result<ScanResult> {
    let mut out = ScanResult {
        root: lib_root.to_string_lossy().to_string(),
        ..Default::default()
    };
    for entry in WalkDir::new(lib_root).into_iter().filter_map(|e| e.ok()) {
        let p = entry.path();
        if !p.is_file() || p.extension().map(|e| e != "md").unwrap_or(true) {
            continue;
        }
        // 跳过隐藏/元数据目录
        let rel_os = p.strip_prefix(lib_root).unwrap_or(p);
        let rel = rel_os.to_string_lossy().replace('\\', "/");
        if rel.split('/').any(|seg| seg.starts_with('_')) {
            continue;
        }
        let content = std::fs::read_to_string(p).unwrap_or_default();
        let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let h1 = find_h1(&content);
        let code = extract_code(h1.as_deref(), &stem);
        let name = extract_name(h1.as_deref()).or_else(|| {
            if code.is_none() {
                Some(stem.clone())
            } else {
                None
            }
        });
        // 类别 = 顶层目录名（或二级，如 文明/太玄帝国/…）
        let category = rel.split('/').next().unwrap_or("").to_string();
        if code.is_none() {
            out.no_code.push(rel.clone());
        }
        // ⚠ 无编号卡**同样要导入**：实测 51 张是真实内容
        // （如《宇宙秩序·大道法则》5516 字），跳过=丢数据。
        out.cards.push(ScannedCard {
            rel_path: rel,
            category,
            old_code: code,
            name,
            h1,
            content_chars: net_chars(&content),
            sha: sha256_hex(&content),
            content,
        });
    }
    Ok(out)
}

/// 把扫描结果写入 `materials`（含 `content` 全文列）。
///
/// 幂等：按 `(code, category)` upsert。
pub fn import_cards(con: &Connection, scan: &ScanResult) -> Result<usize> {
    let mut n = 0;
    for c in &scan.cards {
        // 无编号卡：用「类别 + 路径短hash」生成稳定占位 code（后续由 renumber 分配正式号）
        let code = c.old_code.clone().unwrap_or_else(|| {
            let mut h: u64 = 1469598103934665603;
            for b in c.rel_path.as_bytes() {
                h ^= *b as u64;
                h = h.wrapping_mul(1099511628211);
            }
            format!("__NC__{}-{:06x}", c.category, (h & 0xffffff) as u32)
        });
        let summary: String = c
            .content
            .lines()
            .find(|l| l.trim_start().starts_with('>'))
            .map(|l| l.trim_start_matches(['>', ' ']).trim().to_string())
            .unwrap_or_default();
        // ⚠ upsert 键用 **file_path**，不是 (code, category)。
        // 原因：库里有 590 条旧记录（content 为空、code 可能与新扫描不同），
        // 它们与本次扫描指向**同一批文件**。按 code upsert 会留下重复行，
        // 且旧行 content_chars=NULL 会让对账永远报差异。
        // file_path 才是稳定的文件身份。
        let existing: Option<i64> = con
            .query_row(
                "SELECT id FROM materials WHERE file_path = ?1 LIMIT 1",
                params![c.rel_path],
                |r| r.get(0),
            )
            .ok();

        if let Some(id) = existing {
            con.execute(
                "UPDATE materials SET code=?1, name=?2, category=?3, summary=?4, h1=?5,
                     content=?6, content_chars=?7, content_sha=?8, old_code=?9,
                     updated_at=datetime('now')
                 WHERE id=?10",
                params![
                    code,
                    c.name.clone().unwrap_or_default(),
                    c.category,
                    summary,
                    c.h1.clone().unwrap_or_default(),
                    c.content,
                    c.content_chars as i64,
                    c.sha,
                    c.old_code.clone().unwrap_or_default(),
                    id,
                ],
            )
            .with_context(|| format!("更新失败: {}", c.rel_path))?;
        } else {
            con.execute(
                "INSERT INTO materials(code, name, category, sub_category, file_path, summary,
                                       h1, content, content_chars, content_sha, old_code, updated_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11, datetime('now'))",
                params![
                    code,
                    c.name.clone().unwrap_or_default(),
                    c.category,
                    Option::<String>::None,
                    c.rel_path,
                    summary,
                    c.h1.clone().unwrap_or_default(),
                    c.content,
                    c.content_chars as i64,
                    c.sha,
                    c.old_code.clone().unwrap_or_default(),
                ],
            )
            .with_context(|| format!("导入失败: {}", c.rel_path))?;
        }
        n += 1;
    }
    Ok(n)
}

/// 记入导入台账（供事后对账）。
pub fn record_ledger(con: &Connection, scan: &ScanResult) -> Result<usize> {
    let mut n = 0;
    for c in &scan.cards {
        con.execute(
            "INSERT OR REPLACE INTO import_ledger(src_root, src_path, src_chars, src_sha,
                                                  dst_table, dst_key, dst_chars)
             VALUES(?1,?2,?3,?4,'materials',?5,?6)",
            params![
                scan.root,
                c.rel_path,
                c.content_chars as i64,
                c.sha,
                c.old_code.clone().unwrap_or_default(),
                c.content_chars as i64,
            ],
        )?;
        n += 1;
    }
    Ok(n)
}

/// 对账：**磁盘源文件字数** vs **DB 实际存储字数**，返回不一致清单。
///
/// 返回 `(src_path, 源字数, DB字数)`。
///
/// ⚠ 早期实现错查了 `import_ledger.content_chars`（该表无此列）——
/// 对账必须落到 `materials.content_chars` 才有意义：
/// 台账只记「导入时源有多少字」，真正要验的是「DB 里现在有多少字」。
pub fn verify_import(con: &Connection, lib_root: &Path) -> Result<Vec<(String, i64, i64)>> {
    let mut st = con.prepare(
        "SELECT file_path, COALESCE(content_chars, -1) FROM materials WHERE file_path IS NOT NULL",
    )?;
    let rows = st.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    let mut bad = Vec::new();
    for r in rows.flatten() {
        let (rel, db_chars) = r;
        let p = lib_root.join(&rel);
        let src_chars = match std::fs::read_to_string(&p) {
            Ok(t) => net_chars(&t) as i64,
            Err(_) => continue, // 源不在（已删/已移）：跳过
        };
        if src_chars != db_chars {
            bad.push((rel, src_chars, db_chars));
        }
    }
    Ok(bad)
}

/// 类别 → 新前缀（v3 规范），需结合目录判定。
pub fn category_prefix(category: &str, rel_path: &str) -> &'static str {
    match category {
        "角色" => "CHAR",
        "文明" => {
            // 实测：文化卡在 `文明/<文明名>/文化/`；跨文明关系卡直接躺在 `文明/` 根
            // （如 `文明/CULT-99-010.md` 讲「太玄帝国×星耀联邦」）。
            if rel_path.contains("/文化/") {
                "CLTR"
            } else if rel_path.contains("跨文明") || rel_path.contains("×") {
                "XREA"
            } else if rel_path.split('/').count() == 2 && rel_path.starts_with("文明/CULT") {
                "XREA"
            } else {
                "CIVI"
            }
        }
        "势力" => "FACT",
        "地图" | "地点" => "LOCA",
        "事件" => "EVNT",
        "时间" => "TIME",
        "功法" => "ARTS",
        "力量体系" => "POWR",
        "修炼体系" => "PRAC",
        "境界" => "REAL",
        "科技" => "TECH",
        "丹药" => "PILL",
        "物品" => "ITEM",
        "装备" => "GEAR",
        "天材地宝" => "MTRL",
        "伏笔" => "FORE",
        "因果线" => "CAUS",
        "关系" => "RELA",
        "文化" => "CLTR",
        "世界观" => "WRLD",
        "诡异" => "WEIR",
        _ => "UNKN",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn net_chars_ignores_whitespace() {
        assert_eq!(net_chars("a b\nc\td"), 4);
        assert_eq!(net_chars("中 文\n"), 2);
    }

    #[test]
    fn extract_code_from_h1() {
        assert_eq!(
            extract_code(Some("CHAR-99-001｜陈寂"), "x").as_deref(),
            Some("CHAR-99-001")
        );
    }

    #[test]
    fn extract_code_from_stem_fallback() {
        assert_eq!(extract_code(None, "EVT-99-005").as_deref(), Some("EVT-99-005"));
        assert_eq!(extract_code(None, "太玄皇宫").as_deref(), None);
    }

    #[test]
    fn extract_name_from_h1() {
        assert_eq!(extract_name(Some("CHAR-99-001｜陈寂")).as_deref(), Some("陈寂"));
        assert_eq!(extract_name(Some("无分隔")), None);
    }

    #[test]
    fn prefix_splits_ambiguous_tech() {
        // TECH 一分为三：按目录判定
        assert_eq!(category_prefix("功法", "功法/x.md"), "ARTS");
        assert_eq!(category_prefix("力量体系", "力量体系/x.md"), "POWR");
        assert_eq!(category_prefix("科技", "科技/x.md"), "TECH");
    }

    #[test]
    fn prefix_splits_ambiguous_cas_and_art() {
        assert_eq!(category_prefix("伏笔", "伏笔/x.md"), "FORE");
        assert_eq!(category_prefix("因果线", "因果线/x.md"), "CAUS");
        assert_eq!(category_prefix("物品", "物品/x.md"), "ITEM");
        assert_eq!(category_prefix("装备", "装备/x.md"), "GEAR");
    }

    #[test]
    fn prefix_cult_splits_by_path() {
        assert_eq!(category_prefix("文明", "文明/太玄帝国/文化/CULT-1.md"), "CLTR");
        assert_eq!(category_prefix("文明", "文明/CULT-99-010.md"), "XREA");
        assert_eq!(category_prefix("文明", "文明/太玄帝国/OTHER.md"), "CIVI");
    }

    #[test]
    fn sha_is_stable() {
        assert_eq!(sha256_hex("abc"), sha256_hex("abc"));
        assert_ne!(sha256_hex("abc"), sha256_hex("abd"));
    }

    #[test]
    fn scan_and_import_roundtrip() {
        let dir = std::env::temp_dir().join(format!("neimp_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("角色")).unwrap();
        std::fs::create_dir_all(dir.join("事件")).unwrap();
        let body = "# CHAR-99-001｜陈寂\n\n> 胶囊：主角\n\n## 详细信息\n正文若干字。\n";
        std::fs::write(dir.join("角色/CHAR-99-001.md"), body).unwrap();
        std::fs::write(dir.join("事件/EVT-99-005.md"), "# EVT-99-005｜事件甲\n> 胶囊：x\n").unwrap();

        let scan = scan_library(&dir).unwrap();
        assert_eq!(scan.cards.len(), 2, "{:?}", scan.no_code);

        // 建最小 materials 表验证导入
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch(
            "CREATE TABLE materials(id INTEGER PRIMARY KEY AUTOINCREMENT, code TEXT NOT NULL,
               name TEXT, category TEXT NOT NULL, sub_category TEXT, file_path TEXT, summary TEXT,
               refs TEXT, ref_by TEXT, personality TEXT, h1 TEXT, tags TEXT,
               created_at TEXT, updated_at TEXT, content TEXT, content_chars INTEGER,
               content_sha TEXT, old_code TEXT, new_code TEXT, frontmatter TEXT, renumbered_at TEXT,
               UNIQUE(code, category));
             CREATE TABLE import_ledger(id INTEGER PRIMARY KEY AUTOINCREMENT, src_root TEXT,
               src_path TEXT NOT NULL, src_chars INTEGER, src_sha TEXT, dst_table TEXT,
               dst_key TEXT, dst_chars INTEGER, verified INTEGER DEFAULT 0,
               imported_at TEXT DEFAULT CURRENT_TIMESTAMP, UNIQUE(src_path, dst_table));",
        ).unwrap();
        let n = import_cards(&con, &scan).unwrap();
        assert_eq!(n, 2);
        record_ledger(&con, &scan).unwrap();
        let bad = verify_import(&con, &dir).unwrap();
        assert!(bad.is_empty(), "对账不应有差异：{bad:?}");
        // 故意改坏 DB 字数 → 对账必须检出
        con.execute("UPDATE materials SET content_chars=1 WHERE code='CHAR-99-001'", []).unwrap();
        let bad2 = verify_import(&con, &dir).unwrap();
        assert_eq!(bad2.len(), 1, "改坏后必须检出：{bad2:?}");

        // 全文真的进去了（不是摘要）
        let got: String = con
            .query_row("SELECT content FROM materials WHERE code='CHAR-99-001'", [], |r| r.get(0))
            .unwrap();
        assert!(got.contains("详细信息"), "必须是全文：{got}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ────────────────────────────────────────────────────────────
// 通用源导入（叙事规划 / 管线 / 勘误 / 配置 / 正文）
// ────────────────────────────────────────────────────────────

/// 非卡片源的一个条目。
#[derive(Debug, Clone)]
pub struct ScannedDoc {
    pub src_kind: String,   // narrative | pipeline | errata | config | text | backup
    pub rel_path: String,   // 相对资料库根
    pub content: String,
    pub net_chars: usize,
    pub sha: String,
}

/// 依目录判定来源类别。
pub fn kind_of(rel: &str) -> &'static str {
    if rel.starts_with("03_叙事规划") {
        "narrative"
    } else if rel.starts_with("05_管线框架") {
        "pipeline"
    } else if rel.starts_with("07_勘误工坊") {
        "errata"
    } else if rel.starts_with("00_系统配置") {
        "config"
    } else if rel.starts_with("04_正文") {
        "text"
    } else if rel.starts_with("09_冷备份") {
        "backup"
    } else if rel.starts_with("ai-layer") || rel.starts_with("scripts") {
        "config"
    } else {
        "other"
    }
}

/// 扫描除 `02_资料库` 以外的所有文本源（保留全文）。
///
/// 跳过：`__pycache__`、二进制、以及 `09_冷备份`（备份本身不再备份）。
pub fn scan_docs(root: &Path) -> Result<Vec<ScannedDoc>> {
    const EXT: &[&str] = &["md", "json", "yaml", "yml", "sql", "py", "txt", "env", "toml"];
    let mut out = Vec::new();
    for e in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        let rel = p
            .strip_prefix(root)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/");
        if rel.starts_with("02_资料库") || rel.starts_with("09_冷备份") {
            continue;
        }
        if rel.contains("__pycache__") || rel.contains("/.git/") {
            continue;
        }
        if rel.ends_with(".db") || rel.contains("novel.db") {
            continue;
        }
        let ok = p
            .extension()
            .map(|x| EXT.contains(&x.to_string_lossy().to_lowercase().as_str()))
            .unwrap_or(false);
        if !ok {
            continue;
        }
        let content = match std::fs::read_to_string(p) {
            Ok(t) => t,
            Err(_) => continue, // 非 UTF-8 或读失败：跳过
        };
        out.push(ScannedDoc {
            src_kind: kind_of(&rel).to_string(),
            rel_path: rel,
            net_chars: net_chars(&content),
            sha: sha256_hex(&content),
            content,
        });
    }
    Ok(out)
}

/// 导入非卡片源 → `catalog_index`（含全文摘要）+ `import_ledger`。
pub fn import_docs(con: &Connection, docs: &[ScannedDoc], root: &str) -> Result<usize> {
    let mut n = 0;
    for d in docs {
        let title = d
            .content
            .lines()
            .map(|l| l.trim())
            .find(|l| l.starts_with("# "))
            .map(|l| l.trim_start_matches("# ").trim().to_string());
        con.execute(
            "INSERT INTO catalog_index(node_path, node_type, category, code, title,
                                       summary, char_count, sha, src_kind)
             VALUES(?1,'file',?2,NULL,?3,?4,?5,?6,?7)
             ON CONFLICT(node_path) DO UPDATE SET
               title=excluded.title, summary=excluded.summary,
               char_count=excluded.char_count, sha=excluded.sha,
               src_kind=excluded.src_kind, imported_at=datetime('now')",
            params![
                d.rel_path,
                d.src_kind,
                title,
                d.content.chars().take(200).collect::<String>(),
                d.net_chars as i64,
                d.sha,
                d.src_kind,
            ],
        )?;
        con.execute(
            "INSERT OR REPLACE INTO import_ledger(src_root, src_path, src_chars, src_sha,
                                                  dst_table, dst_key, dst_chars)
             VALUES(?1,?2,?3,?4,'catalog_index',?5,?6)",
            params![root, d.rel_path, d.net_chars as i64, d.sha, d.rel_path, d.net_chars as i64],
        )?;
        n += 1;
    }
    Ok(n)
}

/// 对账非卡片源：磁盘字数 vs 台账字数。
pub fn verify_docs(con: &Connection, root: &Path) -> Result<Vec<(String, i64, i64)>> {
    let mut st = con.prepare(
        "SELECT src_path, src_chars FROM import_ledger
         WHERE dst_table='catalog_index'",
    )?;
    let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    let mut bad = Vec::new();
    for r in rows.flatten() {
        let (rel, ledger_chars) = r;
        let p = root.join(&rel);
        match std::fs::read_to_string(&p) {
            Ok(t) => {
                let now = net_chars(&t) as i64;
                if now != ledger_chars {
                    bad.push((rel, now, ledger_chars));
                }
            }
            Err(_) => continue,
        }
    }
    Ok(bad)
}
