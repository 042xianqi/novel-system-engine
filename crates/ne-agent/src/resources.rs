//! 按需资源加载 —— **规则块**（load_rule）与**技能**（load_skill）。
//!
//! 契约对照太墟：
//!   * `ToolExecutor.kt` 的 `HarnessTool.LOAD_RULE` / `HarnessTool.LOAD_SKILL` 分支
//!   * `PromptRouter.loadRule(name)` / `availableRuleNames()`
//!   * `SkillResourceReader.stripFrontmatter` / `readSubResource`（路径越界防护）
//!
//! # 为什么"按需"而不是"全塞进 system prompt"
//!
//! 与 prompt 分层同一理由（见 [crate::prompt] 的 prefix cache 契约）：
//! system prompt 里只放**目录元数据**（规则名清单、技能名+一句话），
//! 完整正文由模型判断相关后再 `load_rule`/`load_skill` 主动拉取。
//! 否则几十个技能正文常驻，直接击穿 KV cache 并挤压真实上下文。
//!
//! # 目录布局（双层：编译内置 + vault override）
//!
//! ```text
//!   <vault>/rules/<name>.md              ← 规则块（load_rule 的 name）
//!   <vault>/skills/<技能名>/SKILL.md     ← 技能正文（load_skill 的 name）
//!   <vault>/skills/<技能名>/references/… ← 子资源（load_skill 的 path）
//! ```
//!
//! 内置规则见 [`BUILTIN_RULES`]；vault 同名文件优先（与 [`crate::prompt`] 同策略）。

use anyhow::{bail, Result};
use std::path::{Component, Path, PathBuf};

/// 编译内置的规则块（出厂默认）。
///
/// 命名对齐太墟 `availableRuleNames()` 的那一套：workflow / code-navigation /
/// security / memory / environment-proot / tools。这里放**写小说工程**用得到的子集，
/// 且内容是 ne-agent 自己的（不是太墟规则的照搬）。
pub const BUILTIN_RULES: &[(&str, &str)] = &[
    ("workflow", include_str!("../rules/workflow.md")),
    ("security", include_str!("../rules/security.md")),
    ("tools", include_str!("../rules/tools.md")),
];

/// 规则块清单（内置名）。供错误提示与 system 目录段使用。
pub fn builtin_rule_names() -> Vec<&'static str> {
    BUILTIN_RULES.iter().map(|(n, _)| *n).collect()
}

/// 加载规则块：**vault override 优先，内置回退**。
///
/// 返回 `None` 表示未知规则块（文案可直接回写模型，附可用清单）。
pub fn load_rule(vault_root: &Path, name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let override_path = vault_root.join(crate::vault::RULES_DIR).join(format!("{name}.md"));
    if let Ok(text) = std::fs::read_to_string(&override_path) {
        if !text.trim().is_empty() {
            return Some(text);
        }
    }
    for (n, body) in BUILTIN_RULES {
        if *n == name {
            return Some((*body).to_string());
        }
    }
    None
}

/// 可用规则块名单（内置 + vault 目录实存），去重排序后拼成一行。
pub fn available_rule_names(vault_root: &Path) -> String {
    let mut names: Vec<String> = builtin_rule_names().into_iter().map(String::from).collect();
    let dir = vault_root.join(crate::vault::RULES_DIR);
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) == Some("md") {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    names.push(stem.to_string());
                }
            }
        }
    }
    names.sort();
    names.dedup();
    names.join(" / ")
}

/// 一个技能的**元数据**（常驻 system prompt 目录段；不含正文）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillMeta {
    /// 技能名（= 目录名；`load_skill` 的 name）
    pub name: String,
    /// SKILL.md frontmatter 的 `description`（一句话；给模型判断相关性）
    pub description: String,
}

/// 扫描 `<vault>/skills/*/SKILL.md`，返回每个技能的元数据。
///
/// 只读 frontmatter 的 `description` 字段——**不读正文**（正文靠 load_skill 拉）。
pub fn skill_catalog(vault_root: &Path) -> Vec<SkillMeta> {
    let dir = vault_root.join(crate::vault::SKILLS_DIR);
    let Ok(rd) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut out = Vec::new();
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for skill_dir in entries {
        if !skill_dir.is_dir() {
            continue;
        }
        let md = skill_dir.join("SKILL.md");
        let Ok(text) = std::fs::read_to_string(&md) else { continue };
        let Some(name) = skill_dir.file_name().and_then(|s| s.to_str()) else { continue };
        let description = frontmatter_field(&text, "description").unwrap_or_default();
        out.push(SkillMeta { name: name.to_string(), description });
    }
    out
}

/// 从 YAML frontmatter 提取顶层标量字段（仅用于 `description` 这类单行值）。
///
/// 手写轻量解析：避免为一句话引入 YAML 依赖链。只认 `---` 块内的 `key: value`。
fn frontmatter_field(text: &str, key: &str) -> Option<String> {
    let body = text.strip_prefix("---")?;
    let end = body.find("\n---")?;
    let fm = &body[..end];
    for line in fm.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(key) {
            let rest = rest.trim_start();
            if let Some(v) = rest.strip_prefix(':') {
                let v = v.trim().trim_matches('"').trim_matches('\'').trim();
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

/// 剥离 frontmatter，保留正文（对齐太墟 `stripFrontmatter`）。
pub fn strip_frontmatter(text: &str) -> &str {
    let Some(body) = text.strip_prefix("---") else { return text };
    match body.find("\n---") {
        Some(e) => {
            // 跳过闭合的 `---` 行
            let after = &body[e + 4..];
            after.strip_prefix('\n').unwrap_or(after)
        }
        None => text,
    }
}

/// 技能加载结果（区分正文与子资源两种形态）。
#[derive(Debug, Clone)]
pub enum SkillLoad {
    /// 技能正文（`path` 省略时）
    Body { name: String, content: String },
    /// 技能子资源（`path` 给出时）
    Resource { name: String, path: String, content: String },
}

impl SkillLoad {
    /// 渲染成回写给模型的工具结果文本（对齐太墟 `【技能已加载：…】` 口径）。
    pub fn render(&self) -> String {
        match self {
            SkillLoad::Body { name, content } => {
                format!("【技能已加载：{name}】\n{}", content.trim())
            }
            SkillLoad::Resource { name, path, content } => {
                format!("【技能资源：{name}/{path}】\n{}", content.trim())
            }
        }
    }
}

/// 加载技能：`name` 精确匹配（大小写无关），`sub_path` 为技能目录内的相对路径。
///
/// 匹配规则**对齐太墟**（防"选错技能"）：
///   1. 先精确匹配（name）；
///   2. 精确未命中才做模糊包含匹配；
///   3. 模糊命中**多条即报错**，绝不静默取第一条。
pub fn load_skill(vault_root: &Path, name: &str, sub_path: Option<&str>) -> Result<SkillLoad> {
    let query = name.trim().trim_start_matches('/');
    if query.is_empty() {
        bail!("name 不能为空");
    }
    let catalog = skill_catalog(vault_root);
    if catalog.is_empty() {
        bail!("当前 vault 没有已启用的技能（{} 下无 SKILL.md）。", crate::vault::SKILLS_DIR);
    }
    let q = query.to_lowercase();
    let exact: Vec<&SkillMeta> = catalog.iter().filter(|s| s.name.to_lowercase() == q).collect();
    let pool: Vec<&SkillMeta> = if !exact.is_empty() {
        exact
    } else {
        catalog.iter().filter(|s| s.name.to_lowercase().contains(&q)).collect()
    };
    let list = catalog.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join("、");
    let matched = match pool.len() {
        0 => bail!("未找到匹配的技能：{query}。可用技能：{list}"),
        1 => pool[0],
        _ => bail!(
            "技能名 {query} 匹配到多个技能：{}。请使用完整技能名重试。",
            pool.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join("、")
        ),
    };

    let skill_dir = vault_root.join(crate::vault::SKILLS_DIR).join(&matched.name);
    match sub_path.map(str::trim).filter(|p| !p.is_empty()) {
        None => {
            let md = skill_dir.join("SKILL.md");
            let text = std::fs::read_to_string(&md)
                .map_err(|e| anyhow::anyhow!("读取技能正文失败（{}）：{e}", md.display()))?;
            Ok(SkillLoad::Body { name: matched.name.clone(), content: strip_frontmatter(&text).to_string() })
        }
        Some(rel) => {
            let content = read_sub_resource(&skill_dir, rel).map_err(|e| {
                anyhow::anyhow!(
                    "技能资源不可读：{rel}（{}）。路径须为技能目录内的相对路径（如 references/foo.md / scripts/bar.sh）。",
                    e
                )
            })?;
            Ok(SkillLoad::Resource { name: matched.name.clone(), path: rel.to_string(), content })
        }
    }
}

/// 读技能目录内的子资源，**拒绝任何越界路径**（对齐太墟 `readSubResource`）。
///
/// 防护点（任一不满足即拒绝）：
///   * 相对路径本身不得含 `..` / 绝对路径 / 盘符
///   * 规范化后的真实路径必须仍以技能目录为前缀（软链逃逸也要挡住）
fn read_sub_resource(skill_dir: &Path, rel: &str) -> Result<String> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        bail!("不接受绝对路径");
    }
    for c in rel_path.components() {
        match c {
            Component::Normal(_) => {}
            Component::CurDir => {}
            _ => bail!("路径含非法片段（.. / 根 / 前缀）"),
        }
    }
    let joined = skill_dir.join(rel_path);
    let root = std::fs::canonicalize(skill_dir)
        .map_err(|e| anyhow::anyhow!("技能目录解析失败：{e}"))?;
    let target = std::fs::canonicalize(&joined)
        .map_err(|e| anyhow::anyhow!("文件不存在或不可读：{e}"))?;
    if !target.starts_with(&root) {
        bail!("路径越出技能目录");
    }
    if !target.is_file() {
        bail!("不是文件（目录或特殊文件）");
    }
    std::fs::read_to_string(&target).map_err(|e| anyhow::anyhow!("读取失败：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ne-agent-res-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_skill(root: &Path, name: &str, fm: &str, body: &str) {
        let dir = root.join(crate::vault::SKILLS_DIR).join(name);
        std::fs::create_dir_all(dir.join("references")).unwrap();
        std::fs::write(dir.join("SKILL.md"), format!("---\n{fm}\n---\n{body}")).unwrap();
    }

    #[test]
    fn load_builtin_rule() {
        let root = tmp("rule");
        let c = load_rule(&root, "workflow").expect("内置 workflow 应可用");
        assert!(c.contains("工作流") || !c.is_empty());
        assert!(load_rule(&root, "不存在").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rule_override_wins() {
        let root = tmp("ruleov");
        std::fs::create_dir_all(root.join(crate::vault::RULES_DIR)).unwrap();
        std::fs::write(root.join(crate::vault::RULES_DIR).join("workflow.md"), "我的工作流规则").unwrap();
        assert_eq!(load_rule(&root, "workflow").unwrap(), "我的工作流规则");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn available_rules_includes_builtin_and_override() {
        let root = tmp("rulelist");
        std::fs::create_dir_all(root.join(crate::vault::RULES_DIR)).unwrap();
        std::fs::write(root.join(crate::vault::RULES_DIR).join("custom.md"), "x").unwrap();
        let s = available_rule_names(&root);
        assert!(s.contains("workflow"));
        assert!(s.contains("custom"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn skill_catalog_reads_description_only() {
        let root = tmp("cat");
        write_skill(&root, "github-upload", "name: github-upload\ndescription: 推送仓库到 GitHub", "正文");
        let cat = skill_catalog(&root);
        assert_eq!(cat.len(), 1);
        assert_eq!(cat[0].name, "github-upload");
        assert_eq!(cat[0].description, "推送仓库到 GitHub");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn load_skill_body_strips_frontmatter() {
        let root = tmp("body");
        write_skill(&root, "demo", "description: 演示", "这是正文\n第二行");
        let got = load_skill(&root, "demo", None).unwrap();
        match got {
            SkillLoad::Body { content, .. } => {
                assert!(content.starts_with("这是正文"));
                assert!(!content.contains("description"));
            }
            _ => panic!("应是正文"),
        }
        // 大小写无关 + 去斜杠
        assert!(load_skill(&root, "/DEMO", None).is_ok());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn load_skill_sub_resource() {
        let root = tmp("sub");
        write_skill(&root, "demo", "description: d", "正文");
        let dir = root.join(crate::vault::SKILLS_DIR).join("demo").join("references");
        std::fs::write(dir.join("foo.md"), "子资源内容").unwrap();
        let got = load_skill(&root, "demo", Some("references/foo.md")).unwrap();
        assert!(matches!(got, SkillLoad::Resource { .. }));
        assert!(got.render().contains("子资源内容"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn load_skill_rejects_escape() {
        let root = tmp("esc");
        write_skill(&root, "demo", "description: d", "正文");
        // 目录外文件
        std::fs::write(root.join("secret.txt"), "机密").unwrap();
        assert!(load_skill(&root, "demo", Some("../secret.txt")).is_err());
        assert!(load_skill(&root, "demo", Some("/etc/passwd")).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn load_skill_ambiguous_fuzzy_errors() {
        let root = tmp("amb");
        write_skill(&root, "git-flow", "description: a", "x");
        write_skill(&root, "git-hooks", "description: b", "y");
        let err = load_skill(&root, "git", None).unwrap_err();
        assert!(err.to_string().contains("多个技能"), "模糊多命中应报错：{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn load_skill_empty_catalog_errors() {
        let root = tmp("empty");
        let err = load_skill(&root, "x", None).unwrap_err();
        assert!(err.to_string().contains("没有已启用"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
