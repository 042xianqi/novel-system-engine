//! 全库重编号 —— 映射表构建 + 引用改写。
//!
//! # 为什么必须同批做
//!
//! 实测全库有 **3447 处**交叉引用（`CHAR-99-001` 这类硬引用）。
//! 只改文件名不改引用 → 全库断链。
//!
//! # 流程
//!
//! 1. `build_map` 扫描全库 → 旧编号 → 新编号（含冲突消解 + 序号重排）
//! 2. `rewrite_refs` 用映射表重写所有引用
//! 3. 校验无断链 / 无重号 / 无孤儿

use crate::ScannedCard;
use anyhow::Result;
use regex::Regex;
use rusqlite::{params, Connection};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// 一条重编号记录。
#[derive(Debug, Clone)]
pub struct MapEntry {
    pub old_code: String,
    pub new_code: String,
    pub category: String,
    pub old_path: String,
    pub new_path: String,
    pub rule: String,
}

/// 旧前缀 → 新前缀的判定（依赖目录）。
///
/// 返回 `(新前缀, 判定依据说明)`。
pub fn map_prefix(old_prefix: &str, category: &str, rel_path: &str) -> (String, String) {
    // ⚠ 幂等：v3 目标前缀直接放行。
    // 否则重复跑 build_map 会把已重编号的卡再映射一次
    // （实测：POWR-99-001 → UNKN-99-001 的级联污染，111 条）。
    const V3: &[&str] = &[
        "CHAR", "CIVI", "XREA", "FACT", "LOCA", "EVNT", "TIME", "ARTS", "POWR", "PRAC",
        "REAL", "TECH", "PILL", "ITEM", "GEAR", "MTRL", "FORE", "CAUS", "RELA", "CLTR",
        "WRLD", "WEIR", "SYSX",
    ];
    // ⚠ `TECH` 是特例：它既是 v3 前缀（科技），又是**待拆分**的旧前缀
    // （旧库把 功法/力量体系/科技 都写作 TECH）。
    // 判据：所在目录。已在 科技/ 下才是真正的 v3 TECH。
    if old_prefix == "TECH" {
        return match category {
            "功法" => ("ARTS".to_string(), "TECH→ARTS（功法目录）".to_string()),
            "力量体系" => ("POWR".to_string(), "TECH→POWR（力量体系目录）".to_string()),
            "科技" if rel_path.starts_with("科技/") => {
                ("TECH".to_string(), "TECH 落 科技/ 目录，即 v3 本义（幂等）".to_string())
            }
            "科技" => ("TECH".to_string(), "TECH 独占科技".to_string()),
            _ => ("ARTS".to_string(), "TECH 未知来源，暂归功法".to_string()),
        };
    }
    if V3.contains(&old_prefix) {
        return (old_prefix.to_string(), "已是 v3 前缀（幂等）".to_string());
    }
    let np = match old_prefix {
        // 无冲突：直接沿用或加长避歧
        "CHAR" => ("CHAR", "保持不变"),
        "EVT" => ("EVNT", "加长避歧"),
        "MTC" => ("PILL", "加长避歧"),
        "LOC" => ("LOCA", "加长避歧"),
        "TIME" => ("TIME", "保持不变"),
        "ANC" => ("WEIR", "避与叙事层 ANOM 冲突"),
        "SYS" => ("SYSX", "避与 SNAP 近形"),
        // 有冲突：按目录拆
        "TECH" => match category {
            "功法" => ("ARTS", "TECH→ARTS（功法目录）"),
            "力量体系" => ("POWR", "TECH→POWR（力量体系目录）"),
            "科技" => ("TECH", "TECH 独占科技"),
            _ => ("ARTS", "TECH 未知来源，暂归功法"),
        },
        "ART" => match category {
            "物品" => ("ITEM", "ART→ITEM（物品目录）"),
            "装备" => ("GEAR", "ART→GEAR（装备目录）"),
            "天材地宝" => ("MTRL", "ART→MTRL（天材地宝目录）"),
            _ => ("ITEM", "ART 未知来源，暂归物品"),
        },
        "ORG" => match category {
            "势力" => ("FACT", "ORG→FACT（势力目录）"),
            _ => ("CIVI", "ORG→CIVI（文明）"),
        },
        "CIV" => ("CIVI", "CIV→CIVI 统一"),
        "CAS" => match category {
            "伏笔" => ("FORE", "CAS→FORE（伏笔目录）"),
            "因果线" => ("CAUS", "CAS→CAUS（因果线目录）"),
            _ => ("CAUS", "CAS 未知来源，暂归因果线"),
        },
        "REL" => match category {
            "伏笔" => ("FORE", "REL→FORE（伏笔目录）"),
            "关系" | "势力" => ("RELA", "REL→RELA（关系）"),
            _ => ("RELA", "REL 未知来源，暂归关系"),
        },
        "CULT" => {
            if rel_path.contains("/文化/") {
                ("CLTR", "CULT→CLTR（文化目录）")
            } else {
                ("XREA", "CULT→XREA（跨文明关系）")
            }
        }
        // 异常前缀：按目录归类
        "YGEN" => ("SYSX", "YGEN→SYSX（金手指系统设定）"),
        "OTHER" => match category {
            "文明" => ("CIVI", "OTHER→CIVI（文明卡误用前缀）"),
            _ => ("UNKN", "OTHER 来源不明，待人工判定"),
        },
        _ => ("UNKN", "未知前缀，待人工判定"),
    };
    (np.0.to_string(), np.1.to_string())
}

/// 解析 `PREFIX-纪元-序号` 或 `PREFIX-序号`。
fn parse_code(code: &str) -> Option<(String, Option<String>, String)> {
    let re = Regex::new(r"^([A-Z]{2,6})-(\d{2})-(\d{3})$|^([A-Z]{2,6})-(\d{3})$").ok()?;
    let c = re.captures(code)?;
    if let (Some(p), Some(e), Some(n)) = (c.get(1), c.get(2), c.get(3)) {
        Some((p.as_str().into(), Some(e.as_str().into()), n.as_str().into()))
    } else {
        Some((
            c.get(4)?.as_str().into(),
            None,
            c.get(5)?.as_str().into(),
        ))
    }
}

/// 扫描结果 → 重编号映射表。
///
/// 规则：同 `(新前缀, 纪元)` 内统一重排为**连续且全局无冲突**的序号。
///
/// ⚠ 关键：不同旧前缀可能映射到**同一新前缀**
/// （`CIV` 与 `ORG` → 都进 `CIVI`；`REL` 与 `CAS` → 都进 `FORE`）。
/// 若按旧前缀各自重排，会撞号（实测 8 组）。故必须用**全局占用表**：
///
/// 1. 第一遍：把「序号无需变更」的卡预占其原序号
/// 2. 第二遍：其余卡从 1 起找**第一个未被预占**的序号
/// 3. 若原序号恰好可用，优先沿用（减少无谓变更）
pub fn build_map(cards: &[ScannedCard]) -> Vec<MapEntry> {
    // (新前缀, 纪元) → Vec<(原序号, card)>
    let mut groups: BTreeMap<(String, String), Vec<(String, &ScannedCard)>> = BTreeMap::new();
    let mut unknown: Vec<&ScannedCard> = Vec::new();

    for c in cards {
        let Some(old) = &c.old_code else {
            unknown.push(c);
            continue;
        };
        let Some((pfx, era, seq)) = parse_code(old) else {
            unknown.push(c);
            continue;
        };
        let (np, _) = map_prefix(&pfx, &c.category, &c.rel_path);
        let era_key = era.unwrap_or_else(|| "99".into());
        groups.entry((np, era_key)).or_default().push((seq, c));
    }

    let mut out = Vec::new();
    for ((np, era), mut items) in groups {
        items.sort_by(|a, b| a.0.cmp(&b.0));

        // ① 预占：序号在组内**唯一**的卡先占位
        let mut cnt: HashMap<String, usize> = HashMap::new();
        for (seq, _) in &items {
            *cnt.entry(seq.clone()).or_insert(0) += 1;
        }
        let mut used: std::collections::HashSet<u32> = std::collections::HashSet::new();
        for (seq, _) in &items {
            if cnt.get(seq).copied().unwrap_or(0) == 1 {
                if let Ok(n) = seq.parse::<u32>() {
                    used.insert(n);
                }
            }
        }

        // ② 分配
        let mut next_free = 1u32;
        for (seq, card) in &items {
            let uniq = cnt.get(seq).copied().unwrap_or(0) == 1;
            let seq_n = seq.parse::<u32>().unwrap_or(0);
            let final_n = if uniq && seq_n != 0 {
                seq_n // 无冲突：沿用
            } else {
                // 撞号卡：找下一个未占用
                while used.contains(&next_free) {
                    next_free += 1;
                }
                let n = next_free;
                used.insert(n);
                next_free += 1;
                n
            };
            used.insert(final_n);

            let new_code = format!("{np}-{era}-{final_n:03}");
            let old_code = card.old_code.clone().unwrap_or_default();
            let new_path = build_new_path(card, &new_code);
            let old_pfx = parse_code(&old_code).map(|x| x.0).unwrap_or_default();
            let (_, rule) = map_prefix(&old_pfx, &card.category, &card.rel_path);
            out.push(MapEntry {
                old_code,
                new_code,
                category: card.category.clone(),
                old_path: card.rel_path.clone(),
                new_path,
                rule,
            });
        }
    }

    // 无编号卡：按类别分配（纪元 99，序号从 900 起，避开正式号段）
    let mut n = 901;
    let mut used_unknown: HashMap<String, u32> = HashMap::new();
    for c in unknown {
        let np = crate::category_prefix(&c.category, &c.rel_path);
        let cur = used_unknown.entry(np.to_string()).or_insert(n);
        let code = format!("{np}-99-{:03}", *cur);
        *cur += 1;
        // 无编号卡：文件名改为 `<新编号>｜<原名>.md`，保留可读名
        let stem = c
            .rel_path
            .rsplit_once('/')
            .map(|(_, f)| f)
            .unwrap_or(&c.rel_path)
            .trim_end_matches(".md")
            .to_string();
        let new_file = format!("{code}｜{stem}.md");
        let new_path = match c.rel_path.rsplit_once('/') {
            Some((d, _)) => format!("{d}/{new_file}"),
            None => new_file,
        };
        out.push(MapEntry {
            old_code: String::new(),
            new_code: code,
            category: c.category.clone(),
            old_path: c.rel_path.clone(),
            new_path,
            rule: "原无编号，按类别补分配（900 号段）".into(),
        });
    }
    out
}

/// 由旧文件名推导新文件名（保留 `｜名称` / `·状态` 后缀）。
fn build_new_path(card: &ScannedCard, new_code: &str) -> String {
    let (dir, f) = match card.rel_path.rsplit_once('/') {
        Some(x) => x,
        None => ("", card.rel_path.as_str()),
    };
    let suffix = if let Some((_, rest)) = f.split_once('｜') {
        format!("｜{rest}")
    } else if let Some((_, rest)) = f.split_once('·') {
        format!("·{rest}")
    } else {
        ".md".to_string()
    };
    let newf = format!("{new_code}{suffix}");
    if dir.is_empty() {
        newf
    } else {
        format!("{dir}/{newf}")
    }
}

/// 写映射表入库。
pub fn save_map(con: &Connection, entries: &[MapEntry]) -> Result<usize> {
    let mut n = 0;
    for e in entries {
        con.execute(
            // ⚠ 主键语义：old_path 唯一（old_code 对无编号卡为空，不能做键）
            "INSERT OR REPLACE INTO renumber_map(old_code, new_code, category, old_path, new_path, rule)
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![e.old_code, e.new_code, e.category, e.old_path, e.new_path, e.rule],
        )?;
        n += 1;
    }
    Ok(n)
}

/// 用映射表重写正文里的引用，返回 (新正文, 替换次数)。
///
/// 只替换**已知编号**；未知编号原样保留（避免误改）。
pub fn rewrite_refs(text: &str, map: &HashMap<String, String>) -> (String, usize) {
    let re = Regex::new(r"\b[A-Z]{2,6}-\d{2}-\d{3}\b").unwrap();
    let mut count = 0;
    let out = re.replace_all(text, |c: &regex::Captures| {
        let old = &c[0];
        match map.get(old) {
            Some(new) if new != old => {
                count += 1;
                new.clone()
            }
            _ => old.to_string(),
        }
    });
    (out.into_owned(), count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(code: &str, cat: &str, rel: &str) -> ScannedCard {
        ScannedCard {
            rel_path: rel.into(),
            category: cat.into(),
            old_code: Some(code.into()),
            name: None,
            h1: None,
            content: String::new(),
            content_chars: 0,
            sha: String::new(),
        }
    }

    #[test]
    fn parse_3seg_and_2seg() {
        assert_eq!(
            parse_code("CHAR-99-001"),
            Some(("CHAR".into(), Some("99".into()), "001".into()))
        );
        assert_eq!(parse_code("MTC-011"), Some(("MTC".into(), None, "011".into())));
        assert_eq!(parse_code("not-a-code"), None);
    }

    #[test]
    fn tech_splits_by_category() {
        assert_eq!(map_prefix("TECH", "功法", "功法/x.md").0, "ARTS");
        assert_eq!(map_prefix("TECH", "力量体系", "力量体系/x.md").0, "POWR");
        assert_eq!(map_prefix("TECH", "科技", "科技/x.md").0, "TECH");
    }

    #[test]
    fn build_map_is_stable_and_unique() {
        let cards = vec![
            card("TECH-99-003", "功法", "功法/TECH-99-003.md"),
            card("TECH-99-001", "力量体系", "力量体系/TECH-99-001.md"),
            card("TECH-99-005", "科技", "科技/TECH-99-005.md"),
        ];
        let m1 = build_map(&cards);
        let m2 = build_map(&cards);
        let c1: Vec<_> = m1.iter().map(|e| e.new_code.clone()).collect();
        let c2: Vec<_> = m2.iter().map(|e| e.new_code.clone()).collect();
        assert_eq!(c1, c2, "映射必须幂等");
        let uniq: std::collections::HashSet<_> = c1.iter().collect();
        assert_eq!(uniq.len(), c1.len(), "新编号必须唯一: {c1:?}");
    }

    /// 幂等回归：已重编号的卡再跑一次不得变化。
    #[test]
    fn build_map_idempotent_on_v3_prefixes() {
        let cards = vec![
            card("POWR-99-001", "力量体系", "力量体系/POWR-99-001.md"),
            card("CIVI-99-001", "文明", "文明/CIVI-99-001.md"),
            card("EVNT-99-001", "事件", "事件/EVNT-99-001.md"),
        ];
        let m = build_map(&cards);
        for e in &m {
            assert_eq!(e.old_code, e.new_code, "v3 编号不得再变：{e:?}");
        }
        // 三前缀都不得退化为 UNKN
        for e in &m {
            assert!(!e.new_code.starts_with("UNKN"), "不得退化为 UNKN：{e:?}");
        }
    }

    #[test]
    fn duplicate_old_code_gets_shifted() {
        // 同前缀同序号的两张卡（实测存在撞号）
        let cards = vec![
            card("CHAR-99-001", "角色", "角色/a.md"),
            card("CHAR-99-001", "角色", "角色/b.md"),
        ];
        let m = build_map(&cards);
        let codes: std::collections::HashSet<_> = m.iter().map(|e| e.new_code.clone()).collect();
        assert_eq!(codes.len(), 2, "撞号必须被拆开：{codes:?}");
    }

    #[test]
    fn rewrite_refs_replaces_known_only() {
        let mut map = HashMap::new();
        map.insert("CHAR-99-001".to_string(), "CHAR-99-001".to_string());
        map.insert("TECH-99-003".to_string(), "ARTS-99-002".to_string());
        let text = "见 TECH-99-003 与 CHAR-99-001，另有 UNKNOWN-99-999。";
        let (out, n) = rewrite_refs(text, &map);
        assert_eq!(n, 1, "只替换真正变化的");
        assert!(out.contains("ARTS-99-002"));
        assert!(out.contains("UNKNOWN-99-999"), "未知编号应原样保留");
    }

    #[test]
    fn rewrite_is_idempotent_on_stable_codes() {
        let map: HashMap<String, String> = [("CHAR-99-001".to_string(), "CHAR-99-001".to_string())]
            .into_iter()
            .collect();
        let (out, n) = rewrite_refs("见 CHAR-99-001。", &map);
        assert_eq!(n, 0);
        assert_eq!(out, "见 CHAR-99-001。");
    }
}

// ────────────────────────────────────────────────────────────
// 应用映射：改库内编号 + 改写引用
// ────────────────────────────────────────────────────────────

/// 应用结果统计。
#[derive(Debug, Default)]
pub struct ApplyStats {
    pub codes_updated: usize,
    pub refs_in_materials: usize,
    pub refs_in_docs: usize,
    pub cards_touched: usize,
    pub docs_touched: usize,
    /// 改写后仍存在的"未知引用"（指向不存在的编号）
    pub dangling: Vec<String>,
    /// 新旧相同的条目数（无需变更）
    pub unchanged: usize,
    pub conflicts: usize,
}

/// 从库中读出映射（old → new），仅取真正发生变化的。
pub fn load_map(con: &Connection) -> Result<HashMap<String, String>> {
    let mut st = con.prepare(
        "SELECT old_code, new_code FROM renumber_map
         WHERE old_code IS NOT NULL AND old_code <> '' AND old_code <> new_code",
    )?;
    let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut m = HashMap::new();
    for r in rows.flatten() {
        m.insert(r.0, r.1);
    }
    Ok(m)
}

/// 应用映射到 `materials`：
/// - 更新 `code` / `new_code`
/// - 改写 `content` 中的引用
pub fn apply_to_materials(con: &Connection, map: &HashMap<String, String>, dry: bool) -> Result<ApplyStats> {
    let mut st = con.prepare("SELECT id, code, content FROM materials WHERE content IS NOT NULL")?;
    let rows: Vec<(i64, String, String)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .flatten()
        .collect();
    let mut s = ApplyStats::default();
    for (id, old_code, content) in rows {
        let (new_content, n) = rewrite_refs(&content, map);
        let new_code = map.get(&old_code).cloned();
        let code_changed = new_code.is_some();
        let content_changed = n > 0;
        if !code_changed && !content_changed {
            continue;
        }
        s.cards_touched += 1;
        s.refs_in_materials += n;
        if code_changed {
            s.codes_updated += 1;
        }
        if !dry {
            match &new_code {
                Some(nc) => con.execute(
                    "UPDATE materials SET content=?1, new_code=?2, renumbered_at=datetime('now') WHERE id=?3",
                    params![new_content, nc, id],
                )?,
                None => con.execute("UPDATE materials SET content=?1 WHERE id=?2", params![new_content, id])?,
            };
        }
    }
    s.unchanged = map.len();
    Ok(s)
}

/// 应用映射到 `catalog_index`（文档全文改写）。
///
/// ⚠ `catalog_index` 只存了摘要，正文在台账里没落全文——
/// 因此**文档侧只统计引用影响，实际改写发生在源文件**（见 `rewrite_files`）。
pub fn count_refs_in_docs(con: &Connection, map: &HashMap<String, String>) -> Result<usize> {
    let mut st = con.prepare("SELECT summary FROM catalog_index WHERE summary IS NOT NULL")?;
    let rows: Vec<String> = st.query_map([], |r| r.get(0))?.flatten().collect();
    let mut n = 0;
    for t in rows {
        let (_, c) = rewrite_refs(&t, map);
        n += c;
    }
    Ok(n)
}

/// 直接改写**磁盘文件**中的引用（原地）。返回 (文件数, 替换数)。
///
/// ⚠ 只改 `02_资料库` 下的 `.md`；先写 `.bak` 以便回滚。
pub fn rewrite_files(lib_root: &Path, map: &HashMap<String, String>, dry: bool) -> Result<(usize, usize)> {
    let mut files = 0;
    let mut refs = 0;
    for e in walkdir::WalkDir::new(lib_root).into_iter().filter_map(|x| x.ok()) {
        let p = e.path();
        if !p.is_file() || p.extension().map(|x| x != "md").unwrap_or(true) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(p) else { continue };
        let (new_text, n) = rewrite_refs(&text, map);
        if n == 0 {
            continue;
        }
        files += 1;
        refs += n;
        if !dry {
            let bak = p.with_extension("md.bak");
            let _ = std::fs::write(&bak, &text);
            std::fs::write(p, new_text)?;
        }
    }
    Ok((files, refs))
}

/// 校验：改写后是否仍有指向**不存在编号**的引用。
pub fn find_dangling(con: &Connection) -> Result<Vec<String>> {
    // 已知编号集合 = materials 里所有生效 code + new_code
    let mut known = std::collections::HashSet::new();
    {
        let mut st = con.prepare(
            "SELECT code FROM materials WHERE content_chars IS NOT NULL AND code IS NOT NULL
             UNION SELECT new_code FROM materials WHERE new_code IS NOT NULL",
        )?;
        for r in st.query_map([], |r| r.get::<_, String>(0))?.flatten() {
            known.insert(r);
        }
    }
    let re = regex::Regex::new(r"\b[A-Z]{2,6}-\d{2}-\d{3}\b").unwrap();
    let mut out = Vec::new();
    let mut st = con.prepare("SELECT code, content FROM materials WHERE content IS NOT NULL")?;
    let rows: Vec<(String, String)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .flatten()
        .collect();
    for (code, content) in rows {
        for m in re.find_iter(&content) {
            let c = m.as_str();
            if !known.contains(c) {
                out.push(format!("{} 引用 {}（不存在）", code, c));
            }
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

#[cfg(test)]
mod apply_tests {
    use super::*;

    #[test]
    fn rewrite_files_dry_run_counts() {
        let dir = std::env::temp_dir().join(format!("rw_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("角色")).unwrap();
        std::fs::write(dir.join("角色/a.md"), "见 TECH-99-003 与 CHAR-99-001。").unwrap();
        let mut map = HashMap::new();
        map.insert("TECH-99-003".to_string(), "ARTS-99-001".to_string());
        let (f, n) = rewrite_files(&dir, &map, true).unwrap();
        assert_eq!((f, n), (1, 1));
        // dry-run 不应改文件
        let t = std::fs::read_to_string(dir.join("角色/a.md")).unwrap();
        assert!(t.contains("TECH-99-003"), "dry-run 不得改动");
        // 实写
        let (f2, n2) = rewrite_files(&dir, &map, false).unwrap();
        assert_eq!((f2, n2), (1, 1));
        let t2 = std::fs::read_to_string(dir.join("角色/a.md")).unwrap();
        assert!(t2.contains("ARTS-99-001"));
        assert!(dir.join("角色/a.md.bak").exists(), "应留备份");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// 按映射表**重命名磁盘文件**。
///
/// ⚠ 必须两步走（先全部改临时名），否则 `A↔B` 互换时会互相覆盖。
/// 返回 (改名数, 冲突数)。
pub fn rename_files(lib_root: &Path, entries: &[MapEntry], dry: bool) -> Result<(usize, usize)> {
    let mut renamed = 0;
    let mut conflict = 0;
    let mut plan: Vec<(PathBuf, PathBuf)> = Vec::new();
    for e in entries {
        if e.old_path == e.new_path || e.old_path.is_empty() {
            continue;
        }
        let oldp = lib_root.join(&e.old_path);
        let newp = lib_root.join(&e.new_path);
        if !oldp.exists() {
            continue;
        }
        if newp.exists() && newp != oldp {
            conflict += 1;
            continue;
        }
        plan.push((oldp, newp));
    }
    if dry {
        return Ok((plan.len(), conflict));
    }
    // 第一步：改成唯一临时名
    let mut stage: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (i, (oldp, newp)) in plan.iter().enumerate() {
        let tmp = oldp.with_file_name(format!(".renumber_tmp_{i}"));
        std::fs::rename(oldp, &tmp)?;
        stage.push((tmp, newp.clone()));
    }
    // 第二步：临时名 → 目标名
    for (tmp, newp) in stage {
        if let Some(p) = newp.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        std::fs::rename(&tmp, &newp)?;
        renamed += 1;
    }
    Ok((renamed, conflict))
}

// ────────────────────────────────────────────────────────────
// H1 编号修复
// ────────────────────────────────────────────────────────────

/// 把文件 H1 里的**旧编号**替换为新编号。
///
/// ⚠ 实测踩坑：`rewrite_files` 只替换正文引用，**漏了 H1 标题行**。
/// 结果文件名改成 `SYSX-99-001.md`，但首行还是 `# YGEN-001｜万象推演面板`——
/// 下游 `extract_code` 从 H1 取值，与文件名不符，导入时 UNIQUE 冲突。
///
/// 规则：若 H1 以**某个已知旧编号**开头，则换成对应的新编号，保留 `｜名称`。
pub fn fix_h1_codes(lib_root: &Path, entries: &[MapEntry], dry: bool) -> Result<(usize, usize)> {
    // 建 old→new 映射（限非空）
    let mut m: HashMap<String, String> = HashMap::new();
    // 无编号卡：按 new_path 定位，给 H1 补编号前缀
    let mut add_prefix: HashMap<String, String> = HashMap::new();
    for e in entries {
        if e.old_code.is_empty() && !e.new_code.is_empty() {
            add_prefix.insert(e.new_path.clone(), e.new_code.clone());
        } else if !e.old_code.is_empty() && e.old_code != e.new_code {
            m.insert(e.old_code.clone(), e.new_code.clone());
        }
    }
    let mut files = 0;
    let mut fixed = 0;
    for e in walkdir::WalkDir::new(lib_root).into_iter().filter_map(|x| x.ok()) {
        let p = e.path();
        if !p.is_file() || p.extension().map(|x| x != "md").unwrap_or(true) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(p) else { continue };
        // 该文件（按相对路径）是否属于"需补编号前缀"的无编号卡
        let rel = p
            .strip_prefix(lib_root)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/");
        let prefix_for = add_prefix.get(&rel).cloned();

        let mut lines: Vec<String> = Vec::new();
        let mut changed = false;
        let mut h1_done = false;
        for (i, l) in text.lines().enumerate() {
            if i < 40 && !h1_done && l.trim_start().starts_with("# ") {
                // 先看是否属于无编号卡 → 补前缀
                if let Some(nc) = &prefix_for {
                    let title = l.trim_start_matches(['#', ' ']).trim();
                    lines.push(format!("# {nc}｜{title}"));
                    changed = true;
                    fixed += 1;
                    h1_done = true;
                    continue;
                }
                let t = l.trim_start_matches(['#', ' ']);
                // H1 形如 `OLDCODE｜名` 或 `OLDCODE 名` 或 `OLDCODE`
                if let Some((first, rest)) = split_head_code(t) {
                    if let Some(nc) = m.get(&first) {
                        // rest 形如 `｜名称` / `名称` / 空
                        let newline = if rest.is_empty() {
                            format!("# {nc}")
                        } else if rest.starts_with('｜') || rest.starts_with('|') {
                            // 保留原分隔符（统一成全角｜）
                            let name = rest.trim_start_matches(['｜', '|']).trim();
                            format!("# {nc}｜{name}")
                        } else {
                            format!("# {nc}｜{rest}")
                        };
                        lines.push(newline);
                        changed = true;
                        fixed += 1;
                        continue;
                    }
                }
            }
            lines.push(l.to_string());
        }
        if changed {
            files += 1;
            if !dry {
                let mut out = lines.join("\n");
                if text.ends_with('\n') {
                    out.push('\n');
                }
                std::fs::write(p, out)?;
            }
        }
    }
    Ok((files, fixed))
}

/// 拆出 H1 开头的编号与剩余部分。返回 `(code, rest)`。
fn split_head_code(h1: &str) -> Option<(String, String)> {
    let h = h1.trim();
    // 匹配 PREFIX-纪元-序号 或 PREFIX-序号
    let re = regex::Regex::new(r"^([A-Z]{2,6}-\d{2}-\d{3}|[A-Z]{2,6}-\d{3})(.*)$").ok()?;
    let c = re.captures(h)?;
    Some((c.get(1)?.as_str().to_string(), c.get(2)?.as_str().trim().to_string()))
}

#[cfg(test)]
mod h1_tests {
    use super::*;

    #[test]
    fn split_head_variants() {
        assert_eq!(
            split_head_code("YGEN-001｜万象推演面板"),
            Some(("YGEN-001".into(), "｜万象推演面板".into()))
        );
        assert_eq!(split_head_code("CHAR-99-001"), Some(("CHAR-99-001".into(), "".into())));
        assert_eq!(split_head_code("无编号标题"), None);
    }

    #[test]
    fn fix_h1_rewrites_code() {
        let dir = std::env::temp_dir().join(format!("h1_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SYSX-99-001.md"), "# YGEN-001｜万象推演面板\n\n正文。\n").unwrap();
        let entries = vec![MapEntry {
            old_code: "YGEN-001".into(),
            new_code: "SYSX-99-001".into(),
            category: "力量体系".into(),
            old_path: "力量体系/YGEN-001.md".into(),
            new_path: "力量体系/SYSX-99-001.md".into(),
            rule: String::new(),
        }];
        let (f, n) = fix_h1_codes(&dir, &entries, false).unwrap();
        assert_eq!((f, n), (1, 1));
        let t = std::fs::read_to_string(dir.join("SYSX-99-001.md")).unwrap();
        assert!(t.starts_with("# SYSX-99-001｜万象推演面板"), "{t}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// 补齐 H1 编号：凡「文件名有编号、H1 无编号」的卡，给 H1 加 `编号｜` 前缀。
///
/// 与 `fix_h1_codes` 的区别：后者处理"旧编号→新编号"的替换，
/// 本函数处理"**完全没有编号**"的 H1（原无编号卡改名后遗留）。
pub fn ensure_h1_code(lib_root: &Path, dry: bool) -> Result<(usize, usize, Vec<String>)> {
    let re_code = regex::Regex::new(r"^([A-Z]{2,6}-\d{2}-\d{3}|[A-Z]{2,6}-\d{3})").unwrap();
    let mut files = 0;
    let mut fixed = 0;
    let mut skipped = Vec::new();
    for e in walkdir::WalkDir::new(lib_root).into_iter().filter_map(|x| x.ok()) {
        let p = e.path();
        if !p.is_file() || p.extension().map(|x| x != "md").unwrap_or(true) {
            continue;
        }
        let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let Some(m) = re_code.captures(&stem) else { continue };
        let code = m.get(1).unwrap().as_str().to_string();

        let Ok(text) = std::fs::read_to_string(p) else { continue };
        // 找 H1
        let mut h1_idx: Option<usize> = None;
        for (i, l) in text.lines().enumerate() {
            if i > 40 {
                break;
            }
            if l.trim_start().starts_with("# ") {
                h1_idx = Some(i);
                break;
            }
        }
        let Some(idx) = h1_idx else {
            skipped.push(format!("{code} 无 H1"));
            continue;
        };
        let lines: Vec<&str> = text.lines().collect();
        let h1 = lines[idx];
        let title = h1.trim_start_matches(['#', ' ']).trim();
        if re_code.is_match(title) {
            continue; // H1 已有编号
        }
        files += 1;
        fixed += 1;
        if !dry {
            let mut out_lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
            out_lines[idx] = format!("# {code}｜{title}");
            let mut out = out_lines.join("\n");
            if text.ends_with('\n') {
                out.push('\n');
            }
            std::fs::write(p, out)?;
        }
    }
    Ok((files, fixed, skipped))
}

#[cfg(test)]
mod ensure_tests {
    use super::*;

    #[test]
    fn ensure_h1_adds_prefix() {
        let dir = std::env::temp_dir().join(format!("ens_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("CIVI-99-901｜宇宙总纲.md"),
            "# 宇宙总纲\n\n正文。\n",
        )
        .unwrap();
        // 已有编号的 H1 不该动
        std::fs::write(dir.join("CHAR-99-001.md"), "# CHAR-99-001｜陈寂\n正文\n").unwrap();
        let (f, n, _) = ensure_h1_code(&dir, false).unwrap();
        assert_eq!((f, n), (1, 1));
        let t = std::fs::read_to_string(dir.join("CIVI-99-901｜宇宙总纲.md")).unwrap();
        assert!(t.starts_with("# CIVI-99-901｜宇宙总纲"), "{t}");
        let t2 = std::fs::read_to_string(dir.join("CHAR-99-001.md")).unwrap();
        assert_eq!(t2.matches("CHAR-99-001").count(), 1, "已编号的不得重复加");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
