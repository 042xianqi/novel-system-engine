//! `ask_user` 工具的参数模型与渲染 —— 契约对齐太墟 `AskUserModels.kt`。
//!
//! 模型一次可提 1~4 个问题，每题可带候选项（label + 说明）并允许自定义输入。
//! 解析与渲染共用同一份解析，避免两处口径漂移。
//!
//! # 谁提供答案
//!
//! 工具本身**只负责解析与格式化**；真正的"问用户"由外部的交互回调
//! （[`crate::tool::ToolCtx::asker`]）完成——交互式 CLI 在 stdin 上问，
//! 非交互环境（子智能体 Lane / 脚本）则没有回调，工具会明确报错并交回主智能体。
//! 这与太墟的分工一致：`ToolExecutor` 负责落成 pending 请求，UI 负责呈现与回填。

/// 单个问题的候选项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskOption {
    pub label: String,
    pub description: Option<String>,
}

/// 单个问题。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskQuestion {
    pub question: String,
    pub header: Option<String>,
    pub options: Vec<AskOption>,
    pub allow_custom: bool,
}

/// 上限（对齐太墟 `MAX_QUESTIONS` / `MAX_OPTIONS`）。
pub const MAX_QUESTIONS: usize = 4;
pub const MAX_OPTIONS: usize = 6;

/// 解析并校验 `questions` 参数（JSON 数组字符串）。
///
/// 不合法时返回 `Err(文案)`，文案可直接回写模型让它纠错。
pub fn parse(raw: &str) -> Result<Vec<AskQuestion>, String> {
    let v: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| format!("questions 不是合法 JSON：{e}"))?;
    let arr = v.as_array().ok_or("questions 必须是问题对象数组")?;
    if arr.is_empty() || arr.len() > MAX_QUESTIONS {
        return Err(format!("questions 数组长度必须在 1~{MAX_QUESTIONS} 之间"));
    }
    let mut out = Vec::with_capacity(arr.len());
    for (i, el) in arr.iter().enumerate() {
        let obj = el.as_object().ok_or_else(|| format!("questions[{i}] 必须是 JSON 对象"))?;
        let question = obj
            .get("question")
            .and_then(|x| x.as_str())
            .map(|s| s.trim())
            .unwrap_or("");
        if question.is_empty() {
            return Err(format!("questions[{i}].question 不能为空"));
        }
        let options = match obj.get("options").and_then(|x| x.as_array()) {
            Some(items) => {
                let mut opts = Vec::with_capacity(items.len());
                for (j, o) in items.iter().enumerate() {
                    let oo = o
                        .as_object()
                        .ok_or_else(|| format!("questions[{i}].options[{j}] 必须是 JSON 对象"))?;
                    let label = oo
                        .get("label")
                        .and_then(|x| x.as_str())
                        .map(|s| s.trim())
                        .unwrap_or("");
                    if label.is_empty() {
                        return Err(format!("questions[{i}].options[{j}].label 不能为空"));
                    }
                    let desc = oo
                        .get("description")
                        .and_then(|x| x.as_str())
                        .map(|s| s.trim())
                        .filter(|s| !s.is_empty())
                        .map(String::from);
                    opts.push(AskOption { label: label.to_string(), description: desc });
                }
                opts
            }
            None => Vec::new(),
        };
        if options.len() > MAX_OPTIONS {
            return Err(format!("questions[{i}].options 最多 {MAX_OPTIONS} 项"));
        }
        // 重复选项 → 拒（否则用户/解析无法区分）
        for j in 0..options.len() {
            for k in (j + 1)..options.len() {
                if options[j].label == options[k].label {
                    return Err(format!(
                        "questions[{i}] 存在重复选项「{}」，请使用互斥的候选项",
                        options[j].label
                    ));
                }
            }
        }
        let allow_custom = obj
            .get("allow_custom")
            .and_then(|x| x.as_bool())
            .unwrap_or(!options.is_empty());
        // 纯自由文本问题恒允许自定义
        let allow_custom = allow_custom || options.is_empty();
        let header = obj
            .get("header")
            .and_then(|x| x.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(String::from);
        out.push(AskQuestion {
            question: question.to_string(),
            header,
            options,
            allow_custom,
        });
    }
    Ok(out)
}

/// 把问题渲染成交互回调可展示的文本（编号 + 候选项 + 自定义说明）。
pub fn render_questions(qs: &[AskQuestion]) -> String {
    let mut s = String::new();
    for (i, q) in qs.iter().enumerate() {
        if let Some(h) = &q.header {
            s.push_str(&format!("【{h}】\n"));
        }
        s.push_str(&format!("{}. {}", i + 1, q.question));
        s.push('\n');
        for (j, o) in q.options.iter().enumerate() {
            s.push_str(&format!("   {}) {}", (b'a' + j as u8) as char, o.label));
            if let Some(d) = &o.description {
                s.push_str(&format!(" —— {d}"));
            }
            s.push('\n');
        }
        if q.options.is_empty() {
            s.push_str("   （自由文本回答）\n");
        } else if q.allow_custom {
            s.push_str("   （可选项，也可自定义输入）\n");
        }
    }
    s.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_free_text_question() {
        let qs = parse(r#"[{"question":"继续吗？"}]"#).unwrap();
        assert_eq!(qs.len(), 1);
        assert!(qs[0].allow_custom, "纯自由文本问题恒允许自定义");
        assert!(qs[0].options.is_empty());
    }

    #[test]
    fn parse_with_options_and_header() {
        let raw = r#"[{"question":"选哪个？","header":"方案","options":[{"label":"A","description":"甲"},{"label":"B"}]}]"#;
        let qs = parse(raw).unwrap();
        assert_eq!(qs[0].header.as_deref(), Some("方案"));
        assert_eq!(qs[0].options.len(), 2);
        assert_eq!(qs[0].options[0].description.as_deref(), Some("甲"));
        assert!(qs[0].allow_custom, "有 options 时默认允许自定义");
    }

    #[test]
    fn parse_rejects_empty_and_too_many() {
        assert!(parse("[]").is_err());
        assert!(parse(r#"[{"question":""}]"#).is_err());
        let too_many: Vec<_> = (0..5).map(|i| format!(r#"{{"question":"q{i}"}}"#)).collect();
        assert!(parse(&format!("[{}]", too_many.join(","))).is_err());
    }

    #[test]
    fn parse_rejects_duplicate_options() {
        let raw = r#"[{"question":"x","options":[{"label":"A"},{"label":"A"}]}]"#;
        let e = parse(raw).unwrap_err();
        assert!(e.contains("重复选项"), "{e}");
    }

    #[test]
    fn allow_custom_can_be_disabled_when_options_present() {
        let raw = r#"[{"question":"x","allow_custom":false,"options":[{"label":"A"}]}]"#;
        let qs = parse(raw).unwrap();
        assert!(!qs[0].allow_custom);
    }

    #[test]
    fn render_includes_options() {
        let qs = parse(r#"[{"question":"Q","options":[{"label":"A","description":"d"}]}]"#).unwrap();
        let r = render_questions(&qs);
        assert!(r.contains("1. Q"));
        assert!(r.contains("a) A"));
        assert!(r.contains("—— d"));
    }
}
