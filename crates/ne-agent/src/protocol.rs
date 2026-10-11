//! 工具调用协议层 —— **原生 `tool_calls` 优先，文本标记回退**。
//!
//! 对齐太墟 `ProviderClient` + `TextToolCallCodec` 的双通道契约：
//!   * **NATIVE**（默认）：请求注入 provider 标准 `tools` 数组，读响应 `message.tool_calls`；
//!   * **JSON_TEXT**（兜底）：工具 schema 写进提示词，模型输出 `<tool_call>{…}</tool_call>`，
//!     由本模块解析。
//!
//! 决议顺序（太墟 `TextToolCallCodec.resolveCalls`）：
//! `structuredCalls.ifEmpty { textCalls }` —— 结构化通道有结果就**不再**看文本，
//! 防止同一调用被两条通道各解析一次而重复执行。
//!
//! 为什么必须补原生通道：文本协议在真实模型上基本失效——模型看不到 provider
//! 声明的 `tools`，会直接回答"我没有文件读取工具"。M1 曾以文本协议占位并在
//! 模块注释里写"留到 M4"，但 M4 做的是 web/MCP，这件事一直没兑现，直到真机验证才暴露。

use crate::tool::{self, Tool};
use ne_engine::llm::{ToolCall, ToolDefinition};

/// 一次模型回复的解析结果。
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    Text(String),
    ToolCalls(Vec<Call>),
    Empty,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub id: Option<String>,
    pub name: String,
    pub args: tool::ToolArgs,
}

/// 把原生结构化调用 + 文本解析结果**按优先级合成**最终调用列表。
///
/// 结构化优先；结构化为空才用文本回退（对齐太墟 `resolveCalls`）。
pub fn resolve(structured: &[ToolCall], text_body: &str) -> Reply {
    if !structured.is_empty() {
        let calls = structured
            .iter()
            .map(|c| {
                let mut args = tool::ToolArgs::new();
                if let Ok(serde_json::Value::Object(obj)) =
                    serde_json::from_str::<serde_json::Value>(&c.arguments)
                {
                    for (k, v) in obj {
                        let s = match v {
                            serde_json::Value::String(s) => s,
                            other => other.to_string(),
                        };
                        args = args.set(&k, s);
                    }
                }
                Call { id: Some(c.id.clone()), name: c.name.clone(), args }
            })
            .collect();
        return Reply::ToolCalls(calls);
    }
    parse_reply(text_body)
}

/// 解析模型输出中的工具调用（文本协议，NATIVE 失败时的兜底）。
///
/// 同时接受太墟风格 `[[tool_call]]…[[/tool_call]]` 与 `<tool_call>…</tool_call>`。
pub fn parse_reply(text: &str) -> Reply {
    const ODDS: [(&str, &str); 2] =
        [("[[tool_call]]", "[[/tool_call]]"), ("<tool_call>", "</tool_call>")];
    let mut calls = Vec::new();
    let mut rest = String::new();
    let mut cursor = 0usize;

    while cursor < text.len() {
        // 找最近的一个起始标记（两种形态取更靠前的）
        let next = ODDS
            .iter()
            .filter_map(|(o, _)| text[cursor..].find(o).map(|i| (cursor + i, *o, *o)))
            .min_by_key(|(i, _, _)| *i);
        let Some((abs_s, open, _)) = next else {
            rest.push_str(&text[cursor..]);
            break;
        };
        rest.push_str(&text[cursor..abs_s]);
        let close = ODDS.iter().find(|(o, _)| *o == open).map(|(_, c)| *c).unwrap_or("</tool_call>");
        let body_start = abs_s + open.len();
        let Some(e) = text[body_start..].find(close) else {
            rest.push_str(&text[abs_s..]);
            break;
        };
        let body = text[body_start..body_start + e].trim();
        if let Some(c) = parse_one_call(body) {
            calls.push(c);
        } else {
            rest.push_str(&text[abs_s..body_start + e + close.len()]);
        }
        cursor = body_start + e + close.len();
    }

    if calls.is_empty() {
        let t = rest.trim();
        return if t.is_empty() { Reply::Empty } else { Reply::Text(t.to_string()) };
    }
    Reply::ToolCalls(calls)
}

fn parse_one_call(body: &str) -> Option<Call> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let name = v.get("name")?.as_str()?.to_string();
    let mut args = tool::ToolArgs::new();
    // `args`（ne-agent 内建习惯）与 `arguments`（OpenAI 习惯）都接受
    let arg_obj = v
        .get("args")
        .or_else(|| v.get("arguments"))
        .and_then(|a| a.as_object());
    if let Some(obj) = arg_obj {
        for (k, val) in obj {
            let s = match val {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            args = args.set(k, s);
        }
    }
    Some(Call { id: None, name, args })
}

/// 出口修复：把内部消息整理成**协议合法**的发送序列。
///
/// 学太墟 `sanitizeApiTranscript`：
///   * `role=tool` 只有紧跟含对应 `tool_call_id` 的 `assistant(tool_calls)` 才合法；
///     错位/孤立的结果**转写为 user 文本**（信息保留、协议合法）；
///   * `assistant(tool_calls)` 后缺失结果时**补占位 tool 消息**，避免整轮被服务端拒。
///
/// 与 M1 的差异：M1 无条件把所有 `tool` 转 user（文本协议下没有配对可言）；
/// 现在保留合法配对，只修复真正非法的部分——否则模型看不到调用与结果的关联。
/// 稳定错误码：工具结果**未知**（非"失败"，非"缺失"）。
///
/// 语义对齐太墟 v0.22.0（`HarnessMessage.kt` 的 `errorCode` + `HarnessLoop` 文案
/// 「上次工具结果未知，继续前需核验实际状态」）：历史里 assistant 有 tool_call、
/// 但结果没落盘 → 那次调用**是否执行过无从判断**，可能已产生副作用。占位结果必须
/// 如实表达"未知"，而不是谎称"失败"——否则模型会放心重放，可能造成重复副作用。
pub const UNKNOWN_RESULT_CODE: &str = "unknown_tool_result";

/// 为"结果未知"的调用生成**结构化**占位结果（机读错误码 + 人读告警）。
fn unknown_result_placeholder(id: &str, name: &str) -> String {
    serde_json::json!({
        "error_code": UNKNOWN_RESULT_CODE,
        "outcome": "unknown",
        "tool_call_id": id,
        "tool": name,
        "message": "[ERROR] 工具结果未知：该调用可能未执行、已执行但未落盘、或结果在中断/裁剪中丢失。\
继续前必须先核验实际状态（读文件、查日志，或改用只读查询重放）；\
在未核验前**严禁重放原调用**——若它带副作用，可能已经生效。",
    })
    .to_string()
}

pub fn sanitize_for_provider(msgs: &[ne_engine::llm::Message]) -> Vec<ne_engine::llm::Message> {
    let mut awaiting: Vec<(String, String)> = Vec::new();
    let mut out: Vec<ne_engine::llm::Message> = Vec::with_capacity(msgs.len());

    // 中断处没等到结果的调用 → 补"结果未知"占位（保配对合法 + 如实标注未知）
    fn flush(out: &mut Vec<ne_engine::llm::Message>, awaiting: &mut Vec<(String, String)>) {
        for (id, name) in awaiting.drain(..) {
            out.push(ne_engine::llm::Message::tool_result(
                id.clone(),
                unknown_result_placeholder(&id, &name),
            ));
        }
    }

    for m in msgs {
        match m.role.as_str() {
            "assistant" if m.tool_calls.as_ref().is_some_and(|c| !c.is_empty()) => {
                flush(&mut out, &mut awaiting);
                awaiting = m
                    .tool_calls
                    .as_ref()
                    .unwrap()
                    .iter()
                    .map(|c| (c.id.clone(), c.name.clone()))
                    .collect();
                out.push(m.clone());
            }
            "tool" => {
                let id = m.tool_call_id.clone().unwrap_or_default();
                match awaiting.iter().position(|(aid, _)| *aid == id) {
                    Some(pos) if !id.is_empty() => {
                        awaiting.remove(pos);
                        out.push(m.clone());
                    }
                    _ => {
                        // 紧邻前一条不是含该 id 的 assistant(tool_calls) → 转写 user 文本
                        out.push(ne_engine::llm::Message::user(format!(
                            "【工具执行结果（历史顺序异常，已转写为文本）】\n{}",
                            m.content
                        )));
                    }
                }
            }
            _ => {
                if !awaiting.is_empty() {
                    flush(&mut out, &mut awaiting);
                }
                out.push(m.clone());
            }
        }
    }
    if !awaiting.is_empty() {
        flush(&mut out, &mut awaiting);
    }
    out
}

/// 为未执行的调用补配对结果（配对规则的落点，单独可测）。
pub fn aborted_placeholders(calls: &[Call], why: &str) -> Vec<tool::ToolResult> {
    calls
        .iter()
        .map(|c| tool::ToolResult::err(format!("未执行（{}）：{}", why, c.name)))
        .collect()
}

/// 生成发给 provider 的工具定义（JSON Schema，NATIVE 模式注入 `tools`）。
///
/// MCP 工具是动态的（`mcp__<服务>__<工具>`，连上才知道），此处不枚举；
/// 它们仍可通过文本回退通道调用。内建工具逐个人工写 schema——
/// schema 与 `Tool::params()` 同源，改一处不该漏另一处（见该函数注释）。
pub fn builtin_tool_definitions(enabled: &[Tool]) -> Vec<ToolDefinition> {
    enabled.iter().filter_map(|t| tool_definition(*t)).collect()
}

fn tool_definition(t: Tool) -> Option<ToolDefinition> {
    let d = |name: &str, desc: &str, props: serde_json::Value, required: &[&str]| {
        let mut params = serde_json::json!({"type": "object", "properties": props});
        if !required.is_empty() {
            params["required"] = serde_json::json!(required);
        }
        Some(ToolDefinition::function(name, desc, params))
    };
    let s = |ty: &str, desc: &str| serde_json::json!({"type": ty, "description": desc});
    match t {
        Tool::Read => d("read", t.description(), serde_json::json!({"path": s("string", "相对工作区的文件路径")}), &["path"]),
        Tool::Write => d("write", t.description(), serde_json::json!({"path": s("string", "相对工作区的文件路径"), "content": s("string", "要写入的完整内容（覆盖）")}), &["path", "content"]),
        Tool::Edit => d("edit", t.description(), serde_json::json!({"path": s("string", "文件路径"), "old": s("string", "要被替换的原文（必须唯一匹配）"), "new": s("string", "替换后的文本")}), &["path", "old", "new"]),
        Tool::Ls => d("ls", t.description(), serde_json::json!({"path": s("string", "目录路径，默认 .")}), &[]),
        Tool::Base => d("base", t.description(), serde_json::json!({"command": s("string", "要执行的 shell 命令")}), &["command"]),
        Tool::WebFetch => d("web_fetch", t.description(), serde_json::json!({"url": s("string", "http/https URL"), "max_chars": s("integer", "截断上限（可选）")}), &["url"]),
        Tool::WebSearch => d("web_search", t.description(), serde_json::json!({"query": s("string", "搜索词"), "engine": s("string", "引擎（可选）"), "limit": s("integer", "条数（可选）")}), &["query"]),
        Tool::Capability => d("use_capability", t.description(), serde_json::json!({
            "action": s("string", "list=列出已启用的 MCP 服务（不启动进程）；inspect=查看某服务的工具清单与参数；call=调用某服务的某工具"),
            "server": s("string", "MCP 服务名（list 时省略；inspect/call 必填）"),
            "tool": s("string", "目标工具名（call 必填；inspect 省略则列出该服务全部工具）"),
            "arguments": serde_json::json!({"type": "object", "description": "call 时传给目标工具的实参对象，结构与 inspect 输出的参数 schema 一致"})
        }), &["action"]),
        Tool::Plan => d("plan", t.description(), serde_json::json!({
            "action": s("string", "replace_active/get_active/advance/clear_active"),
            "goal": s("string", "总体目标（replace_active 时）"),
            "steps": serde_json::json!({"type": "array", "description": "步骤列表，每项 {id,title,status}", "items": {"type": "object"}}),
            "status": s("string", "整体状态（可选）")
        }), &["action"]),
        Tool::Scratchpad => d("scratchpad", t.description(), serde_json::json!({"action": s("string", "save/get/list/delete/clear"), "key": s("string", "键"), "value": s("string", "值")}), &["action"]),
        Tool::AskUser => d("ask_user", t.description(), serde_json::json!({"questions": serde_json::json!({"type": "array", "description": "问题列表，每项 {header,question,options?}","items": {"type": "object"}})}), &["questions"]),
        Tool::LoadRule => d("load_rule", t.description(), serde_json::json!({"rule": s("string", "规则块名")}), &["rule"]),
        Tool::LoadSkill => d("load_skill", t.description(), serde_json::json!({"name": s("string", "技能名"), "path": s("string", "技能内子资源路径（可选）")}), &["name"]),
        Tool::Subagents => d("subagents", t.description(), serde_json::json!({"subagents": serde_json::json!({"type": "array", "items": {"type": "object"}}), "max_tasks": s("integer", "（可选）")}), &["subagents"]),
        Tool::DualAgent => d("dual_agent", t.description(), serde_json::json!({"prompt": s("string", "任务描述与验收标准"), "max_steps": s("integer", "（可选）")}), &["prompt"]),
        Tool::Compress => d("compress", t.description(), serde_json::json!({"mode": s("string", "before/after"), "anchor": s("string", "锚点原文摘录（≥8 字符）")}), &["mode"]),
        Tool::HistoryRead => d("history_read", t.description(), serde_json::json!({"id": s("integer", "消息 id")}), &["id"]),
        Tool::HistorySearch => d("history_search", t.description(), serde_json::json!({"query": s("string", "搜索词"), "limit": s("integer", "（可选）")}), &["query"]),
        // MCP 动态工具不在此枚举
        Tool::Mcp => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plain_text() {
        assert_eq!(parse_reply("你好啊"), Reply::Text("你好啊".into()));
    }

    #[test]
    fn parse_xml_tool_call() {
        let r = parse_reply(r#"<tool_call>{"name":"read","args":{"path":"a.txt"}}</tool_call>"#);
        match r {
            Reply::ToolCalls(c) => {
                assert_eq!(c.len(), 1);
                assert_eq!(c[0].name, "read");
                assert_eq!(c[0].args.get("path"), Some("a.txt"));
            }
            other => panic!("应为 ToolCalls，得到 {other:?}"),
        }
    }

    #[test]
    fn parse_bracket_tool_call_and_arguments_key() {
        // 太墟风格 [[tool_call]] + OpenAI 习惯的 arguments 键
        let r = parse_reply(r#"[[tool_call]]{"name":"ls","arguments":{"path":"."}}[[/tool_call]]"#);
        match r {
            Reply::ToolCalls(c) => {
                assert_eq!(c[0].name, "ls");
                assert_eq!(c[0].args.get("path"), Some("."));
            }
            other => panic!("应为 ToolCalls，得到 {other:?}"),
        }
    }

    #[test]
    fn structured_takes_priority_over_text() {
        let structured = vec![ToolCall {
            id: "call_1".into(),
            name: "read".into(),
            arguments: r#"{"path":"s.txt"}"#.into(),
        }];
        // 文本里还有个调用，但结构化非空 → 只用结构化（防重复执行）
        let r = resolve(&structured, r#"<tool_call>{"name":"ls","args":{}}</tool_call>"#);
        match r {
            Reply::ToolCalls(c) => {
                assert_eq!(c.len(), 1, "结构化非空时不应再解析文本");
                assert_eq!(c[0].name, "read");
                assert_eq!(c[0].id.as_deref(), Some("call_1"));
            }
            other => panic!("应为 ToolCalls，得到 {other:?}"),
        }
    }

    #[test]
    fn fallback_to_text_when_no_structured() {
        let r = resolve(&[], r#"<tool_call>{"name":"ls","args":{"path":"."}}</tool_call>"#);
        match r {
            Reply::ToolCalls(c) => assert_eq!(c[0].name, "ls"),
            other => panic!("应为 ToolCalls，得到 {other:?}"),
        }
    }

    #[test]
    fn sanitize_keeps_paired_tool_result() {
        let calls = vec![ToolCall { id: "c1".into(), name: "read".into(), arguments: "{}".into() }];
        let msgs = vec![
            ne_engine::llm::Message::user("读文件"),
            ne_engine::llm::Message::assistant_tool_calls(calls),
            ne_engine::llm::Message::tool_result("c1", "[OK] 内容"),
        ];
        let out = sanitize_for_provider(&msgs);
        assert_eq!(out[2].role, "tool", "配对合法，不应被转写");
        assert_eq!(out[2].tool_call_id.as_deref(), Some("c1"));
    }

    #[test]
    fn sanitize_rewrites_orphan_tool_result() {
        let msgs = vec![ne_engine::llm::Message::tool_result("ghost", "[OK] x")];
        let out = sanitize_for_provider(&msgs);
        assert_eq!(out[0].role, "user", "孤立结果应转写为 user 文本");
        assert!(out[0].content.contains("历史顺序异常"));
    }

    #[test]
    fn sanitize_fills_missing_result() {
        let calls = vec![ToolCall { id: "c9".into(), name: "read".into(), arguments: "{}".into() }];
        let msgs = vec![
            ne_engine::llm::Message::assistant_tool_calls(calls),
            ne_engine::llm::Message::user("下一句"),
        ];
        let out = sanitize_for_provider(&msgs);
        // assistant 后应补一条 tool 占位，再是 user
        assert_eq!(out[1].role, "tool");
        assert_eq!(out[1].tool_call_id.as_deref(), Some("c9"));
        assert_eq!(out[2].role, "user");
    }

    /// 占位结果必须表达"**未知**"（可能已产生副作用），而非谎称"失败/缺失"——
    /// 否则模型会放心重放，可能造成重复副作用（对齐太墟 v0.22.0 语义）。
    #[test]
    fn missing_result_placeholder_says_unknown_not_failed() {
        let calls = vec![ToolCall {
            id: "c7".into(),
            name: "write".into(),
            arguments: r#"{"path":"a.txt"}"#.into(),
        }];
        let msgs = vec![
            ne_engine::llm::Message::assistant_tool_calls(calls),
            ne_engine::llm::Message::user("继续"),
        ];
        let out = sanitize_for_provider(&msgs);
        assert_eq!(out[1].role, "tool");
        let v: serde_json::Value = serde_json::from_str(&out[1].content)
            .expect("占位应是结构化 JSON（机读错误码）");
        assert_eq!(v["error_code"], UNKNOWN_RESULT_CODE);
        assert_eq!(v["outcome"], "unknown");
        assert_eq!(v["tool"], "write", "要保留工具名，便于模型判断该核验什么");
        let msg = v["message"].as_str().unwrap();
        assert!(msg.contains("严禁重放"), "必须明确禁止未核验重放：{msg}");
    }

    /// 两个都等不到结果的调用不应互相覆盖（旧实现用 BTreeSet，多个同 id 会丢）。
    #[test]
    fn multiple_missing_results_all_placeholder() {
        let calls = vec![
            ToolCall { id: "a".into(), name: "read".into(), arguments: "{}".into() },
            ToolCall { id: "b".into(), name: "ls".into(), arguments: "{}".into() },
        ];
        let msgs = vec![
            ne_engine::llm::Message::assistant_tool_calls(calls),
            ne_engine::llm::Message::user("继续"),
        ];
        let out = sanitize_for_provider(&msgs);
        let ids: Vec<_> = out
            .iter()
            .filter(|m| m.role == "tool")
            .map(|m| m.tool_call_id.clone().unwrap())
            .collect();
        assert_eq!(ids, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn builtin_definitions_cover_enabled_tools() {
        let all = builtin_tool_definitions(Tool::all());
        // 19 个内建工具里 18 个有原生 schema（MCP 动态工具除外，走 use_capability 代理发现）
        assert_eq!(all.len(), 18, "应有 18 个内建工具 schema（MCP 经 use_capability 代理）");
        for d in &all {
            assert!(!d.function.name.is_empty());
            assert_eq!(d.function.parameters["type"], "object");
        }
    }
}
