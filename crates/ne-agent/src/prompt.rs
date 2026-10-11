//! Prompt 组装 —— **分层** + 双层资源（编译内置 + 外部 override）+ `{{VAR}}` 严格校验。
//!
//! # 不可逆决定之二：system 段只放稳定字节
//!
//! 见 docs/ROADMAP-ne-agent.md 与 docs/ref_taixu_harness_notes.md（"prefix cache 契约"）。
//!
//! 教训来自太墟：system prompt 里任何**逐轮变化**的字节（时间戳、当前文件列表、
//! 本轮召回的记忆……）都会击穿其后全部对话的 KV cache，token 与延迟成本成倍上涨。
//!
//! 所以本模块把 prompt 明确切成两半：
//!
//! ```text
//!   SystemPrompt  ← 只放**稳定**内容：人格 / 约束 / 工具说明 / 项目规则
//!                   （同一会话相邻两轮之间，字节级不应变化）
//!
//!   TurnPrefix    ← 逐轮变化的内容：本轮召回的记忆等
//!                   挂在**最新 user 消息的前缀**，不污染 system 段
//! ```
//!
//! # 双层资源
//!
//! 前身来自 OMP 的 `include_str!` + 外部覆盖思路：
//!   * **内置**：`include_str!` 编进二进制，出厂即可用（离线、无需随附文件）
//!   * **override**：`<vault>/prompts/<name>.md` 存在且非空 → 覆盖内置
//!
//! CLI 是短进程，**每次执行重读** override，改完存盘即生效，无需重编译。
//! （长驻进程需重启才生效——这与 OMP 一致，不是缺陷。）

use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::path::Path;

/// 编译内置的默认 system prompt（出厂）。
pub const BUILTIN_ROOT_SYSTEM: &str = include_str!("../prompts/root_system.md");
/// 编译内置的对话任务规则。
pub const BUILTIN_CHAT_RULES: &str = include_str!("../prompts/chat_rules.md");

/// 段落名 → 内置内容。这是"出厂默认层"。
pub const BUILTIN: &[(&str, &str)] = &[
    ("root_system", BUILTIN_ROOT_SYSTEM),
    ("chat_rules", BUILTIN_CHAT_RULES),
];

/// 已完成组装的 system prompt（**稳定性已由结构保证**）。
#[derive(Debug, Clone)]
pub struct SystemPrompt {
    pub text: String,
}

/// 逐轮变化的前缀块，挂在最新 user 消息前。
#[derive(Debug, Clone, Default)]
pub struct TurnPrefix {
    pub blocks: Vec<String>,
}

impl TurnPrefix {
    pub fn is_empty(&self) -> bool {
        self.blocks.iter().all(|b| b.trim().is_empty())
    }

    /// 渲染成 prepend 到 user 消息的文本。
    pub fn render(&self) -> String {
        let mut s = String::new();
        for b in &self.blocks {
            if b.trim().is_empty() {
                continue;
            }
            s.push_str(b.trim_end());
            s.push_str("\n\n");
        }
        s
    }

    /// 把前缀与用户消息合成一条（**只有最新那轮**这样做）。
    pub fn apply_to(&self, user_text: &str) -> String {
        let p = self.render();
        if p.is_empty() {
            user_text.to_string()
        } else {
            format!("{p}{user_text}")
        }
    }
}

/// 取某段落内容：**override 优先，内置回退**。
///
/// `prompt_dir` 通常是 `<vault>/prompts`。
pub fn load_segment(prompt_dir: &Path, name: &str) -> String {
    let override_path = prompt_dir.join(format!("{name}.md"));
    if let Ok(text) = std::fs::read_to_string(&override_path) {
        if !text.trim().is_empty() {
            return text;
        }
    }
    for (n, body) in BUILTIN {
        if *n == name {
            return (*body).to_string();
        }
    }
    String::new()
}

/// system prompt 的稳定段构成（顺序即拼接顺序）。
pub const SYSTEM_SEGMENTS: &[&str] = &["root_system", "chat_rules"];

/// 组装 system prompt。
///
/// * `prompt_dir`：override 目录（vault 的 `prompts/`）
/// * `vars`：`{{VAR}}` 变量表（**缺失即报错**，不静默留空）
///
/// 注意：**不要**把逐轮变化的内容做成变量传进来，那会破坏稳定性。
/// 逐轮内容走 [`TurnPrefix`]。
pub fn build_system_prompt(prompt_dir: &Path, vars: &BTreeMap<&str, String>) -> Result<SystemPrompt> {
    let mut parts = Vec::new();
    for name in SYSTEM_SEGMENTS {
        let raw = load_segment(prompt_dir, name);
        if raw.trim().is_empty() {
            continue;
        }
        parts.push(render(&raw, vars).with_context(|| format!("渲染 prompt 段 `{name}` 失败"))?);
    }
    Ok(SystemPrompt { text: parts.join("\n\n") })
}

/// 必须提供的变量（渲染时缺一个都报错）。
///
/// 与太墟 `PromptAssetLoader` 同策略：**缺变量直接失败**，
/// 而不是留下 `{{TOOLS}}` 这种残渣进模型上下文。
pub fn required_vars() -> Vec<&'static str> {
    vec!["AGENT_NAME", "TOOLS", "VAULT_ROOT"]
}

/// 校验：`required_vars()` 是否都在 `vars` 中。
pub fn validate_vars(vars: &BTreeMap<&str, String>) -> Result<()> {
    let missing: Vec<&str> = required_vars()
        .into_iter()
        .filter(|k| !vars.contains_key(k))
        .collect();
    if !missing.is_empty() {
        bail!("prompt 缺少必需变量：{}", missing.join(", "));
    }
    Ok(())
}

/// 渲染 `{{VAR}}`（大写字母/数字/下划线）。**未提供的变量 → 报错**。
///
/// ⚠ UTF-8 安全：必须按**子串查找**推进，不能按字节索引取 `char`
/// （那样会把多字节字符拆碎，中文变乱码——已由 `render_substitutes_vars` 回归覆盖）。
pub fn render(template: &str, vars: &BTreeMap<&str, String>) -> Result<String> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(pos) = rest.find("{{") {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 2..];
        let Some(e) = after.find("}}") else {
            bail!("prompt 模板有未闭合的 `{{{{`");
        };
        let key = after[..e].trim();
        if is_var_name(key) {
            match vars.get(key) {
                Some(v) => out.push_str(v),
                None => bail!("prompt 变量未提供：{{{{{key}}}}}"),
            }
            rest = &after[e + 2..];
        } else {
            // 不是变量占位（如 JSON 片段里的 `{{`），原样保留
            out.push_str("{{");
            rest = after;
        }
    }
    out.push_str(rest);
    Ok(out)
}

fn is_var_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && s.chars().any(|c| c.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn vars() -> BTreeMap<&'static str, String> {
        let mut m = BTreeMap::new();
        m.insert("AGENT_NAME", "ne-agent".to_string());
        m.insert("TOOLS", "read, write".to_string());
        m.insert("VAULT_ROOT", "/tmp/v".to_string());
        m
    }

    #[test]
    fn builtin_used_when_no_override() {
        let dir = std::env::temp_dir().join(format!("ne-agent-pr-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(load_segment(&dir, "root_system"), BUILTIN_ROOT_SYSTEM);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn override_wins_when_present_and_nonempty() {
        let dir = std::env::temp_dir().join(format!("ne-agent-pr-ov-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("root_system.md"), "我是被覆盖的人格").unwrap();
        assert_eq!(load_segment(&dir, "root_system"), "我是被覆盖的人格");
        // 空 override 要回退内置（不能把 system prompt 变空）
        std::fs::write(dir.join("chat_rules.md"), "   \n  ").unwrap();
        assert_eq!(load_segment(&dir, "chat_rules"), BUILTIN_CHAT_RULES);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_substitutes_vars() {
        let m = vars();
        assert_eq!(render("你好 {{AGENT_NAME}}", &m).unwrap(), "你好 ne-agent");
    }

    #[test]
    fn render_fails_on_missing_var() {
        let m = BTreeMap::new();
        let err = render("{{AGENT_NAME}} 与 {{TOOLS}}", &m).unwrap_err();
        assert!(err.to_string().contains("AGENT_NAME"), "报错应指出缺失变量名");
    }

    #[test]
    fn render_keeps_non_var_braces() {
        let m = vars();
        // 小写/中文不是变量，应原样保留（避免 JSON 示例被误吃）
        assert_eq!(render("{\"a\": 1} {{AGENT_NAME}}", &m).unwrap(), "{\"a\": 1} ne-agent");
    }

    #[test]
    fn validate_vars_reports_missing() {
        let mut m = vars();
        m.remove("TOOLS");
        let err = validate_vars(&m).unwrap_err();
        assert!(err.to_string().contains("TOOLS"));
    }

    #[test]
    fn build_system_prompt_is_stable_for_same_inputs() {
        // 稳定性：同样的输入，两次组装必须字节级一致（prefix cache 的前提）
        let dir = std::env::temp_dir().join(format!("ne-agent-pr-stable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = build_system_prompt(&dir, &vars()).unwrap();
        let b = build_system_prompt(&dir, &vars()).unwrap();
        assert_eq!(a.text, b.text);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn turn_prefix_does_not_touch_system() {
        let sp = build_system_prompt(Path::new("/nonexistent"), &vars()).unwrap();
        let before = sp.text.clone();
        let tp = TurnPrefix { blocks: vec!["<recalled_memory>旧记忆</recalled_memory>".into()] };
        let user = tp.apply_to("继续写");
        assert!(user.contains("旧记忆") && user.ends_with("继续写"));
        // system 段不受影响
        assert_eq!(sp.text, before);
    }
}
