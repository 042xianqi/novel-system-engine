//! LLM 调用层 —— `l5.py::call_llm` 的 Rust 移植，**去掉 7k/tokio/reqwest**。
//!
//! 协议：OpenAI 兼容 `POST {base_url}/chat/completions`。
//! 重试策略（逐条对齐 Python）：
//!   * 外层 attempt ∈ 0..(retries × len(pool))，key = pool[attempt % len(pool)]
//!   * 内层同 key 最多 4 次（首次 + 3 次 429 指数退避 2^n 秒）
//!   * 非 429 或第 4 次 429 → 切下一把 key
//!   * 每轮外交互后 sleep 1.5×(attempt%len(pool)+1)
//!   * content 为空 → 记 "empty content" 后切 key（不重试同 key）

use crate::http;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// 单次调用参数（对齐 l5.py 请求体）。
#[derive(Debug, Clone)]
pub struct LlmParams {
    pub base_url: String,
    pub model: String,
    pub keys: Vec<String>,
    pub max_tokens: u32,
    pub temperature: f64,
    pub enable_thinking: bool,
    pub retries: usize,
    pub timeout: Duration,
    /// 工具定义（空 = 纯聊天，请求体不注入 `tools`）。
    pub tools: Vec<ToolDefinition>,
    /// 工具调用模式（决定注入方式与解析优先级）。
    pub tool_call_mode: ToolCallMode,
}

impl Default for LlmParams {
    fn default() -> Self {
        LlmParams {
            // 占位默认值：实际参数一律由 providers::build_params 从 models.yml 填充，
            // 此处留空以免把任何具体端点/模型名写死进代码。
            base_url: String::new(),
            model: String::new(),
            keys: vec![],
            max_tokens: 4096,
            temperature: 1.0,
            enable_thinking: false,
            retries: 3,
            timeout: Duration::from_secs(600),
            tools: Vec::new(),
            tool_call_mode: ToolCallMode::Native,
        }
    }
}

impl LlmParams {
    /// 派生「纯聊天」副本：清空工具并把模式置为 `Disabled`。
    ///
    /// 供**非工具角色**（Planner、上下文摘要器等）使用：它们只用文本 I/O，
    /// 一旦请求里带了 `tools`，真实模型会优先返回 `tool_calls`（正文为空），
    /// 导致文本解析失败。对齐太墟
    /// `plannerModel.copy(pureChatMode = true, toolCallMode = DISABLED)`。
    pub fn pure_chat(&self) -> LlmParams {
        LlmParams {
            tools: Vec::new(),
            tool_call_mode: ToolCallMode::Disabled,
            ..self.clone()
        }
    }
}

/// 一次结构化工具调用。
///
/// **线格式对齐 OpenAI / 太墟 `ApiToolCall`**：`{"id","type":"function","function":{"name","arguments"}}`。
/// `arguments` 保持**原始 JSON 字符串**（OpenAI 协议如此），不在这一层解析成对象——
/// 参数解释权归工具层（它才知道自己接受哪些键），契约层只负责透传。
///
/// 因为线格式是嵌套的，这里手写 Serialize/Deserialize，不用 derive（derive 会拍平成
/// `{id,name,arguments}`，provider 会拒收）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

impl Serialize for ToolCall {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(Some(3))?;
        m.serialize_entry("id", &self.id)?;
        m.serialize_entry("type", "function")?;
        let f = serde_json::json!({"name": self.name, "arguments": self.arguments});
        m.serialize_entry("function", &f)?;
        m.end()
    }
}

impl<'de> Deserialize<'de> for ToolCall {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        let id = v.get("id").and_then(|x| x.as_str()).unwrap_or_default().to_string();
        let f = v.get("function").ok_or_else(|| serde::de::Error::missing_field("function"))?;
        let name = f.get("name").and_then(|x| x.as_str()).unwrap_or_default().to_string();
        let arguments = f
            .get("arguments")
            .and_then(|x| x.as_str())
            .unwrap_or("{}")
            .to_string();
        Ok(ToolCall { id, name, arguments })
    }
}

/// 工具定义（JSON Schema，对齐太墟 `ApiToolDefinition`）。
///
/// 发请求时放进 `tools` 数组，走 provider 原生 function calling 通道。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// 固定 "function"（OpenAI 兼容协议目前只有这一种）
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionDefinition,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDefinition {
    pub name: String,
    pub description: String,
    /// JSON Schema 对象（如 `{"type":"object","properties":{...},"required":[...]}`）
    pub parameters: serde_json::Value,
}

impl ToolDefinition {
    pub fn function(name: impl Into<String>, description: impl Into<String>, parameters: serde_json::Value) -> Self {
        ToolDefinition {
            kind: "function".into(),
            function: FunctionDefinition {
                name: name.into(),
                description: description.into(),
                parameters,
            },
        }
    }
}

/// 一条消息。对齐太墟 `ApiMessage`：`content` 可空（工具调用轮常无正文），
/// 并可携带 `tool_calls`（assistant 发起调用）与 `tool_call_id`（tool 结果配对）。
///
/// 后两个字段额外参与**序列化**：`None` 时完全不出现在 JSON 里，
/// 从而对既有调用方（不传这两个字段）保持**逐字节兼容**。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    #[serde(default)]
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn system(c: impl Into<String>) -> Self {
        Message { role: "system".into(), content: c.into(), ..Default::default() }
    }
    pub fn user(c: impl Into<String>) -> Self {
        Message { role: "user".into(), content: c.into(), ..Default::default() }
    }
    /// assistant 文本（无工具调用）。
    pub fn assistant(c: impl Into<String>) -> Self {
        Message { role: "assistant".into(), content: c.into(), ..Default::default() }
    }
    /// assistant 发起工具调用（对齐太墟：content 为空串，不 omit）。
    pub fn assistant_tool_calls(calls: Vec<ToolCall>) -> Self {
        Message {
            role: "assistant".into(),
            content: String::new(),
            tool_calls: Some(calls),
            tool_call_id: None,
        }
    }
    /// tool 结果（必须紧跟对应 assistant(tool_calls)）。
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Message {
            role: "tool".into(),
            content: content.into(),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

/// 调用结果：正文 + 推理内容（thinking 透传）+ 结构化工具调用。
#[derive(Debug, Clone)]
pub struct LlmOutput {
    pub content: String,
    pub reasoning: String,
    /// 原生 `tool_calls`（provider 结构化通道）。空 = 本轮没有工具调用。
    pub tool_calls: Vec<ToolCall>,
}

/// 工具调用模式（对齐太墟 `ToolCallMode`）。
///
/// * `Native`：注入 provider 标准 `tools` 数组，解析 `message.tool_calls`（默认）；
/// * `JsonText`：工具 schema 写进 system 提示词，模型用文本标记输出调用（兼容网关兜底）；
/// * `Disabled`：纯聊天，不注入任何工具。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolCallMode {
    #[default]
    Native,
    JsonText,
    Disabled,
}

impl LlmParams {
    /// 本参数是否配置了工具（决定要不要注入 `tools`）。默认无 → 不改变既有请求体。
    pub fn has_tools(&self) -> bool {
        !self.tools.is_empty() && self.tool_call_mode != ToolCallMode::Disabled
    }
}

fn body_json(p: &LlmParams, messages: &[Message]) -> serde_json::Value {
    let msgs: Vec<serde_json::Value> = messages
        .iter()
        .map(|m| {
            // 与太墟一致：content 永远发（严格网关要求它是 String），空串也不省。
            let mut obj = serde_json::Map::new();
            obj.insert("role".into(), serde_json::json!(m.role));
            obj.insert("content".into(), serde_json::json!(m.content));
            if let Some(id) = &m.tool_call_id {
                obj.insert("tool_call_id".into(), serde_json::json!(id));
            }
            if let Some(calls) = &m.tool_calls {
                obj.insert("tool_calls".into(), serde_json::json!(calls));
            }
            serde_json::Value::Object(obj)
        })
        .collect();

    let mut body = serde_json::json!({
        "model": p.model,
        "messages": msgs,
        "max_tokens": p.max_tokens,
        "temperature": p.temperature,
        "enable_thinking": p.enable_thinking,
    });

    // 仅在配置了工具时注入 `tools` —— 默认（空）时请求体与既有逐字节一致。
    if p.has_tools() {
        body["tools"] = serde_json::to_value(&p.tools).unwrap_or(serde_json::Value::Null);
    }
    body
}

/// 解析一次响应里的 `message.tool_calls`（结构化通道；缺失/空数组 → 空 Vec）。
fn parse_tool_calls(msg: &serde_json::Value) -> Vec<ToolCall> {
    let Some(arr) = msg.get("tool_calls").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|c| {
            let f = c.get("function")?;
            let name = f.get("name")?.as_str()?.trim().to_string();
            if name.is_empty() {
                return None;
            }
            // 部分网关对无参函数不下发 arguments → 兜底 "{}"（对齐太墟）
            let arguments = f
                .get("arguments")
                .and_then(|a| a.as_str())
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .unwrap_or("{}")
                .to_string();
            // id 缺失时合成一个稳定 id（与 tool 结果配对用）
            let id = c
                .get("id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("call_{}", crate::llm::short_hash(&name)));
            Some(ToolCall { id, name, arguments })
        })
        .collect()
}

/// 无外部依赖的短哈希（配对 id 用，只要进程内唯一即可）。
pub(crate) fn short_hash(s: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}{:x}", h.finish(), now)
}

fn key_tail(k: &str) -> String {
    k.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect()
}

/// 主入口：带 key 池轮转与 429 退避的同步调用。
pub fn call(p: &LlmParams, messages: &[Message]) -> Result<LlmOutput> {
    let endpoint = format!("{}/chat/completions", p.base_url.trim_end_matches('/'));
    if p.keys.is_empty() {
        return Err(anyhow!("key 池为空"));
    }
    let n = p.keys.len();
    let total = p.retries.max(1) * n;
    let mut last_err = String::from("未知");

    for attempt in 0..total {
        let key = &p.keys[attempt % n];
        let mut backoff_round = 0usize;
        let mut round = 0usize;
        while round < 4 {
            round += 1;
            let headers = vec![
                ("Content-Type".to_string(), "application/json".to_string()),
                ("Authorization".to_string(), format!("Bearer {key}")),
            ];
            match http::post_json_value(&endpoint, &headers, &body_json(p, messages), p.timeout) {
                Ok(v) => {
                    let msg = &v["choices"][0]["message"];
                    let content = msg["content"].as_str().unwrap_or("").to_string();
                    let reasoning = msg["reasoning_content"]
                        .as_str()
                        .or_else(|| msg["reasoning"].as_str())
                        .unwrap_or("")
                        .to_string();
                    let tool_calls = parse_tool_calls(msg);
                    // 工具调用轮正文常为空（模型只发调用）→ 有 tool_calls 也算有效响应，
                    // 不能按"empty content"切 key（对齐太墟：content 与 toolCalls 任一非空即有效）。
                    if !content.trim().is_empty() || !tool_calls.is_empty() {
                        return Ok(LlmOutput { content, reasoning, tool_calls });
                    }
                    last_err = "empty content".into();
                    break; // 切 key
                }
                Err(e) => {
                    let msg = e.to_string();
                    if msg.starts_with("HTTP 429") && backoff_round < 3 {
                        let wait = 1u64 << backoff_round; // 2^n
                        std::thread::sleep(Duration::from_secs(wait));
                        backoff_round += 1;
                        last_err = format!(
                            "HTTP 429 限流（key …{} 第{backoff_round}次退避 {wait}s）",
                            key_tail(key)
                        );
                        continue; // 同 key 退避后重试
                    }
                    if msg.starts_with("HTTP 429") {
                        last_err = format!("HTTP 429 限流（key …{} 退避 3 次仍限流，切下一把）", key_tail(key));
                    } else {
                        last_err = msg;
                    }
                    break; // 切 key
                }
            }
        }
        std::thread::sleep(Duration::from_millis(1500 * ((attempt % n) as u64 + 1)));
    }
    Err(anyhow!("[ne-engine] LLM 调用失败（{}×{} 次）：{last_err}", p.retries, n))
}

/// 剥元行与代码围栏 —— 移植 `l5.py::strip_meta`。
pub fn strip_meta(text: &str) -> String {
    let mut lines: Vec<&str> = text.trim().lines().collect();
    // 去开头【技法计划…】行
    while let Some(f) = lines.first() {
        if f.trim().starts_with("【技法计划") {
            lines.remove(0);
        } else {
            break;
        }
    }
    // 去 [修复记录] 行
    let joined = lines
        .into_iter()
        .filter(|l| !l.trim().starts_with("[修复记录]"))
        .collect::<Vec<_>>()
        .join("\n");
    // 去 markdown 围栏
    let re_open = regex::Regex::new(r"(?m)^```[a-zA-Z]*\n").unwrap();
    let s = re_open.replace(&joined, "").to_string();
    let re_close = regex::Regex::new(r"(?m)\n```$").unwrap();
    let s = re_close.replace(s.trim(), "").to_string();
    s.trim().to_string()
}
