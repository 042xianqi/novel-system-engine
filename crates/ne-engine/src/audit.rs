//! 卡片合规审计 —— 用 `ne_core::templates` 的 20 类模板校验存量库。
//!
//! 检出维度：
//! - `missing_h1_code`：H1 未带编号（如 `# 太玄皇宫`）
//! - `h1_not_first`：H1 不在首行（前置块挤占）
//! - `missing_required`：缺必填段/字段
//! - `misplaced`：卡放在非归属目录（如 ANC 卡躺在 事件/）
//! - `unknown_category`：目录无对应模板

use anyhow::Result;
use ne_core::templates::{all_templates, template_of, CardTemplate, FieldKind};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// 单张卡的审计结论。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardAudit {
    pub file: String,
    pub category: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub issues: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
}

impl CardAudit {
    pub fn ok(&self) -> bool {
        self.issues.is_empty() && self.missing.is_empty()
    }
}

/// 一类卡的汇总。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryAudit {
    pub category: String,
    pub total: usize,
    pub ok: usize,
    /// 各必填字段的命中数（用于算覆盖率）
    pub field_hits: BTreeMap<String, usize>,
}

/// 全库报告。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditReport {
    pub lib_root: String,
    pub categories: Vec<CategoryAudit>,
    pub cards: Vec<CardAudit>,
    pub total_cards: usize,
    pub ok_cards: usize,
}

/// 提取卡内出现的所有「段名/字段名」（H2/H3/加粗/表格首列）。
fn collect_keys(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for ln in text.lines() {
        let s = ln.trim();
        if s.starts_with("##") {
            let n = s.trim_start_matches('#').trim();
            if !n.is_empty() {
                out.push(n.to_string());
            }
        } else if let Some(rest) = s.strip_prefix('*') {
            // **字段**：值
            let t = rest.trim_start_matches('*');
            if let Some(end) = t.find("**") {
                let n = t[..end].trim().trim_end_matches(['：', ':']).trim();
                if !n.is_empty() {
                    out.push(n.to_string());
                }
            }
        } else if s.starts_with('|') {
            let cells: Vec<&str> = s.trim_matches('|').split('|').collect();
            if let Some(c0) = cells.first() {
                let n = c0.trim().trim_matches('*').trim().trim_end_matches(['：', ':']).trim();
                if !n.is_empty() && !n.chars().all(|c| c == '-' || c == ' ') {
                    out.push(n.to_string());
                }
            }
        }
    }
    out
}

/// H1 行（`# ...`）与是否在首行。
fn find_h1(text: &str) -> (Option<String>, bool) {
    let mut first_nonempty = true;
    for ln in text.lines() {
        let s = ln.trim();
        if s.is_empty() {
            continue;
        }
        if let Some(rest) = s.strip_prefix("# ") {
            return (Some(rest.trim().to_string()), first_nonempty);
        }
        first_nonempty = false;
    }
    (None, false)
}

/// 从 H1 或文件名里抽编号（`CHAR-99-001｜名` 或 `CHAR-99-001.md`）。
fn extract_code(h1: Option<&str>, stem: &str) -> Option<String> {
    let re = regex::Regex::new(r"^([A-Z]{2,6}(?:-\d+){1,3})").unwrap();
    if let Some(h) = h1 {
        if let Some(c) = re.captures(h.trim()) {
            return Some(c.get(1).unwrap().as_str().to_string());
        }
    }
    re.captures(stem).map(|c| c.get(1).unwrap().as_str().to_string())
}

/// 前缀 → 模板。
///
/// ⚠ 前缀有歧义，必须结合**路径**消歧（实测踩坑）：
/// - `CULT` 既是「文化」也是「跨文明关系」——
///   实测 11/12 张 CULT 卡是各文明的**文化卡**（在 `文明/<文明>/文化/`），
///   只有 `文明/CULT-99-010.md` 才是跨文明关系卡（讲 A文明×B文明）。
/// - `ORG` 既是「文明」也是「势力」——实测全在 `文明/` 下。
/// - `TECH` 既是「功法」「力量体系」也是「科技」。
fn template_by_prefix(code: &str, rel_path: &str) -> Option<CardTemplate> {
    let p = code.split('-').next()?;
    let cands: Vec<CardTemplate> = all_templates()
        .into_iter()
        .filter(|t| {
            t.code_prefix == p || t.code_prefix_aliases.iter().any(|a| a == p)
        })
        .collect();
    if cands.len() <= 1 {
        return cands.into_iter().next();
    }
    // v3 前缀已全局唯一；以下为历史多前缀前缀（若有）的路径消歧兜底。
    if p == "CULT" {
        if rel_path.contains("/文化/") {
            return cands.into_iter().find(|t| t.category == "文化");
        }
        return cands.into_iter().find(|t| t.category == "跨文明关系");
    }
    if p == "TECH" {
        if rel_path.contains("力量体系") {
            return cands.into_iter().find(|t| t.category == "力量体系");
        }
        if rel_path.contains("科技") {
            return cands.into_iter().find(|t| t.category == "科技");
        }
        return cands.into_iter().find(|t| t.category == "科技");
    }
    cands.into_iter().next()
}

/// 审计单张卡。
pub fn audit_card(path: &Path, lib_root: &Path, cat_dir: &str) -> Result<CardAudit> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let rel = path
        .strip_prefix(lib_root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.to_string_lossy().to_string());

    let (h1, h1_first) = find_h1(&text);
    let code = extract_code(h1.as_deref(), &stem);

    let mut issues = Vec::new();
    let mut missing = Vec::new();

    // 归属目录（用 code 前缀或目录名）
    let eff_cat = code
        .as_deref()
        .and_then(|c| template_by_prefix(c, &rel))
        .map(|t| t.category)
        .unwrap_or_else(|| ne_core::templates::normalize_category(cat_dir));

    let Some(tmpl) = template_of(&eff_cat) else {
        issues.push(format!("unknown_category={cat_dir}"));
        return Ok(CardAudit { file: rel, category: cat_dir.to_string(), code, issues, missing });
    };

    // 跨类混放：code 前缀推断的类 ≠ 所在目录
    let dir_norm = ne_core::templates::normalize_category(cat_dir);
    if tmpl.category != dir_norm {
        issues.push(format!("misplaced: {} 卡在 {} 目录", tmpl.category, cat_dir));
    }

    if code.is_none() {
        issues.push("missing_h1_code".into());
    }
    if h1.is_none() {
        issues.push("no_h1".into());
    } else if !h1_first {
        issues.push("h1_not_first".into());
    }

    // 必填字段
    let keys = collect_keys(&text);
    for f in tmpl.fields.iter().filter(|f| f.required) {
        if f.kind == FieldKind::Capsule {
            // capsule：H1 后首个 `>` 行
            if !text.lines().any(|l| l.trim_start().starts_with('>')) {
                missing.push(f.name.clone());
            }
            continue;
        }
        if !keys.iter().any(|k| f.matches(k)) {
            missing.push(f.name.clone());
        }
    }

    Ok(CardAudit { file: rel, category: tmpl.category, code, issues, missing })
}

/// 审计整个资料库。
pub fn audit_library(lib_root: &Path) -> Result<AuditReport> {
    let mut cards = Vec::new();
    let mut cats: BTreeMap<String, CategoryAudit> = BTreeMap::new();

    let rd = std::fs::read_dir(lib_root)?;
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('_') {
            continue;
        }
        let mut files = Vec::new();
        walk_md(&p, &mut files);
        if files.is_empty() {
            continue;
        }
        for f in &files {
            let a = audit_card(f, lib_root, &name)?;
            let ent = cats.entry(a.category.clone()).or_insert_with(|| CategoryAudit {
                category: a.category.clone(),
                total: 0,
                ok: 0,
                field_hits: BTreeMap::new(),
            });
            ent.total += 1;
            if a.ok() {
                ent.ok += 1;
            }
            if let Some(t) = template_of(&a.category) {
                let txt = std::fs::read_to_string(f).unwrap_or_default();
                let keys = collect_keys(&txt);
                let has_cap = txt.lines().any(|l| l.trim_start().starts_with('>'));
                for fld in &t.fields {
                    let hit = if fld.kind == FieldKind::Capsule {
                        has_cap
                    } else {
                        keys.iter().any(|k| fld.matches(k))
                    };
                    if hit {
                        *ent.field_hits.entry(fld.name.clone()).or_insert(0) += 1;
                    }
                }
            }
            cards.push(a);
        }
    }

    let total = cards.len();
    let ok = cards.iter().filter(|c| c.ok()).count();
    Ok(AuditReport {
        lib_root: lib_root.to_string_lossy().to_string(),
        categories: cats.into_values().collect(),
        cards,
        total_cards: total,
        ok_cards: ok,
    })
}

fn walk_md(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let n = e.file_name().to_string_lossy().to_string();
        if p.is_dir() {
            if n.starts_with('_') {
                continue;
            }
            walk_md(&p, out);
        } else if n.ends_with(".md") {
            out.push(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_keys_from_all_formats() {
        let t = "# CHAR-1｜甲\n> 胶囊：xx\n## 详细信息\n### 基础信息\n**姓名**：甲\n| 谱系 | 修炼 |\n";
        let k = collect_keys(t);
        assert!(k.contains(&"详细信息".to_string()));
        assert!(k.contains(&"基础信息".to_string()));
        assert!(k.contains(&"姓名".to_string()));
        assert!(k.contains(&"谱系".to_string()));
    }

    #[test]
    fn h1_first_detection() {
        assert!(find_h1("# A\n正文").1);
        assert!(!find_h1("=== 资料内容 ===\n\n# A\n").1, "前置块挤占应被检出");
    }

    #[test]
    fn code_from_h1_or_stem() {
        assert_eq!(extract_code(Some("CHAR-99-001｜陈寂"), "x").as_deref(), Some("CHAR-99-001"));
        assert_eq!(extract_code(Some("无编号"), "EVT-100").as_deref(), Some("EVT-100"));
        assert_eq!(extract_code(None, "太玄皇宫").as_deref(), None);
    }

    #[test]
    fn detects_missing_required() {
        let dir = std::env::temp_dir().join(format!("audit_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("事件")).unwrap();

        // 【2026-10-18 修】本测试此前长期失败，根因是**模板已升级、测试没跟上**：
        //
        //   · 统一模板规范 v1.0（2026-10-17）把结构改为四区块：
        //       ① 基础要素 / ② 核心关联实体 / ③ 扩展描述 / ④ 溯源与版本
        //   · 「胶囊」变为**强制**字段（H1 下第一行引用块）
        //   · 「核心关联实体」强制，且吸收 关联/关联标注/关联实体 为别名
        //   · 原散文式字段（含「因果链」「时间」「事件名」）一律**降级为可选**
        //
        // 所以现在只缺「胶囊」和「核心关联实体」两个，旧断言里的
        // 「因果链」「关联标注」「涉及文明」都不再是必填项。
        std::fs::write(dir.join("事件/EVT-1.md"), "# EVT-1｜甲\n## 事件名\nx\n## 时间\nx\n").unwrap();
        let a = audit_card(&dir.join("事件/EVT-1.md"), &dir, "事件").unwrap();

        // 两个强制字段缺失，必须报出
        assert!(a.missing.contains(&"胶囊".to_string()),
                "应报缺「胶囊」（H1 下第一行引用块），实际 {:?}", a.missing);
        assert!(a.missing.contains(&"核心关联实体".to_string()),
                "应报缺「核心关联实体」（四区块第二块），实际 {:?}", a.missing);

        // 已提供的散文式字段不得被误报（它们现在是可选）
        for k in ["事件名", "时间"] {
            assert!(!a.missing.contains(&k.to_string()), "{} 已提供却被报缺：{:?}", k, a.missing);
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn accepts_alias_as_hit() {
        let dir = std::env::temp_dir().join(format!("audit_alias_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("事件")).unwrap();
        // 用别名「关联」代替规范名「关联标注」—— 应算命中
        std::fs::write(
            dir.join("事件/EVT-2.md"),
            "# EVT-2｜甲\n> 胶囊：x\n## 事件名\nx\n## 编号\nEVT-2\n## 时间\nx\n## 涉及文明\nx\n## 因果链\nx\n## 关联\ny\n",
        ).unwrap();
        let a = audit_card(&dir.join("事件/EVT-2.md"), &dir, "事件").unwrap();
        assert!(a.missing.is_empty(), "别名应被接受：{:?}", a.missing);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detects_misplaced() {
        let dir = std::env::temp_dir().join(format!("audit_mis_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("事件")).unwrap();
        std::fs::write(dir.join("事件/ANC-99-001.md"), "# ANC-99-001｜诡异甲\n> 胶囊：x\n").unwrap();
        let a = audit_card(&dir.join("事件/ANC-99-001.md"), &dir, "事件").unwrap();
        assert!(a.issues.iter().any(|i| i.contains("misplaced")), "{:?}", a.issues);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_h1_code_detected() {
        let dir = std::env::temp_dir().join(format!("audit_noh1_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("地图")).unwrap();
        std::fs::write(dir.join("太玄皇宫.md"), "# 太玄皇宫\n> 胶囊：x\n").unwrap();
        let a = audit_card(&dir.join("太玄皇宫.md"), &dir, "地图").unwrap();
        assert!(a.issues.contains(&"missing_h1_code".to_string()), "{:?}", a.issues);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
