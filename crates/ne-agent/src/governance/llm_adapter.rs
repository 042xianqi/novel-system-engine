//! 可插拔多后端 LLM 适配层（world-gov，第 17 批）。
//!
//! # 后端
//! - `DryRunMock`：**无网络**，回显确定性 JSON。单元测试与 `--dry-run` 预览用。
//! - `Ollama`：本地 `http://127.0.0.1:11434`（无密钥）。
//! - `OpenAiCompat`：任意 OpenAI 兼容端点（DeepSeek 等；base_url/api_key/model 可配）。
//!
//! # 出网隔离（关键）
//! 真正发 HTTP 的代码在 `#[cfg(feature = "llm_integration")]` 下；默认构建**不存在出网路径**，
//! 真实后端返回 `BackendUnavailable`。且即便 feature 开启，仍须**三重闸门全开**才允许调用
//! （见 `llm_gate::real_llm_unlocked`）——由 `semantic_pipeline` 统一收口。
//!
//! # 零 tokio / 零 reqwest
//! 复用 `ne_engine::http::post_json_value`（仓库既有的零依赖阻塞 HTTP 栈）。
use serde_json::{json, Value};

use super::llm_config::{Endpoint, LlmConfig};
use super::prompt_lib;

/// 一条对话消息。
#[derive(Debug, Clone)]
pub struct Msg {
    pub role: String,
    pub content: String,
}

impl Msg {
    pub fn user(c: impl Into<String>) -> Self { Msg { role: "user".into(), content: c.into() } }
    pub fn system(c: impl Into<String>) -> Self { Msg { role: "system".into(), content: c.into() } }
}

/// 结构化约束。
#[derive(Debug, Clone, Default)]
pub struct Constraint {
    pub max_tokens: u32,
    pub temperature: f64,
    /// 期望的 JSON Schema（用于校验；Value::Null 表示不校验）。
    pub schema: Value,
}

/// 调用产出。
#[derive(Debug, Clone)]
pub struct LlmOutput {
    /// 解析后的结构化 JSON。
    pub content: Value,
    /// 原始响应文本。
    pub raw: String,
    pub token_in: i64,
    pub token_out: i64,
    pub backend: String,
    pub model: String,
    pub cache_hit: bool,
}

/// 后端失败原因（可判定是否可重试）。
#[derive(Debug, Clone)]
pub enum BackendError {
    /// 默认构建无出网能力。
    Unavailable(String),
    /// 未配置端点。
    NotConfigured(String),
    /// 协议错误（不可重试）。
    Protocol(String),
    /// 网络/超时（可重试）。
    Transport(String),
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackendError::Unavailable(m) => write!(f, "后端不可用：{m}"),
            BackendError::NotConfigured(m) => write!(f, "端点未配置：{m}"),
            BackendError::Protocol(m) => write!(f, "协议错误：{m}"),
            BackendError::Transport(m) => write!(f, "传输错误：{m}"),
        }
    }
}

/// 统一后端接口（**同步**：仓库运行时是 std 阻塞单线程，铁律零 tokio）。
pub trait LlmBackend {
    fn name(&self) -> &str;
    fn model(&self) -> &str;
    /// 执行一次对话，返回结构化输出。
    fn chat(&self, sys_prompt: &str, messages: &[Msg], c: &Constraint) -> std::result::Result<LlmOutput, BackendError>;
}

// ────────────────────────────────── DryRunMock（无网络）
/// 确定性模拟后端：不联网，回显一个符合 schema 的空壳 JSON。
/// 用途：单测（**绝不外呼**）与 `--dry-run` 预览。
pub struct DryRunMock {
    pub model: String,
    /// 可注入的固定响应（测试用）；None 则按 schema 造一个最小合法对象。
    pub canned: Option<String>,
}

impl DryRunMock {
    pub fn new(model: &str) -> Self {
        DryRunMock { model: model.to_string(), canned: None }
    }
    pub fn with_canned(model: &str, raw: &str) -> Self {
        DryRunMock { model: model.to_string(), canned: Some(raw.to_string()) }
    }
}

impl LlmBackend for DryRunMock {
    fn name(&self) -> &str { "dry_run_mock" }
    fn model(&self) -> &str { &self.model }
    fn chat(&self, _sys: &str, _messages: &[Msg], c: &Constraint) -> std::result::Result<LlmOutput, BackendError> {
        let raw = self.canned.clone().unwrap_or_else(|| minimal_for_schema(&c.schema).to_string());
        let content = prompt_lib::extract_json(&raw)
            .map_err(|e| BackendError::Protocol(format!("mock 输出非法：{e}")))?;
        Ok(LlmOutput {
            content,
            raw,
            token_in: 0,
            token_out: 0,
            backend: self.name().into(),
            model: self.model.clone(),
            cache_hit: false,
        })
    }
}

/// 依据 schema 造一个最小合法对象（required 字段按 type 填零值）。
pub fn minimal_for_schema(schema: &Value) -> Value {
    let Some(obj) = schema.as_object() else { return json!({}) };
    let mut out = serde_json::Map::new();
    if let Some(req) = obj.get("required").and_then(|x| x.as_array()) {
        let props = obj.get("properties").and_then(|x| x.as_object());
        for r in req.iter().filter_map(|x| x.as_str()) {
            let sub = props.and_then(|p| p.get(r)).cloned().unwrap_or(Value::Null);
            out.insert(r.to_string(), zero_for(&sub));
        }
    }
    Value::Object(out)
}

fn zero_for(sub: &Value) -> Value {
    // enum 优先：取首个合法值（mock 需产出 schema 合法值）
    if let Some(first) = sub.get("enum").and_then(|x| x.as_array()).and_then(|a| a.first()) {
        return first.clone();
    }
    match sub.get("type").and_then(|x| x.as_str()) {
        Some("array") => json!([]),
        Some("object") => json!({}),
        Some("string") => json!(""),
        Some("integer") | Some("number") => json!(0),
        Some("boolean") => json!(false),
        _ => Value::Null,
    }
}

// ────────────────────────────────── 真实后端（feature 门控）
/// 构造一个真实后端（Ollama / OpenAI 兼容）。**仅在 feature 开启时**真正可出网。
pub enum RealBackend {
    Ollama(Endpoint),
    OpenAiCompat(Endpoint),
}

impl RealBackend {
    pub fn from_config(cfg: &LlmConfig) -> Self {
        let ep = super::llm_config::active_endpoint(cfg);
        match cfg.default_backend.as_str() {
            "openai_compat" | "openai" | "deepseek" => RealBackend::OpenAiCompat(ep),
            _ => RealBackend::Ollama(ep),
        }
    }
}

impl LlmBackend for RealBackend {
    fn name(&self) -> &str {
        match self {
            RealBackend::Ollama(_) => "ollama",
            RealBackend::OpenAiCompat(_) => "openai_compat",
        }
    }
    fn model(&self) -> &str {
        match self {
            RealBackend::Ollama(e) => &e.model_id,
            RealBackend::OpenAiCompat(e) => &e.model_id,
        }
    }
    fn chat(&self, sys_prompt: &str, messages: &[Msg], c: &Constraint) -> std::result::Result<LlmOutput, BackendError> {
        match self {
            RealBackend::Ollama(e) => ollama_chat(e, sys_prompt, messages, c),
            RealBackend::OpenAiCompat(e) => openai_chat(e, sys_prompt, messages, c),
        }
    }
}

#[cfg(feature = "llm_integration")]
fn ollama_chat(ep: &Endpoint, sys: &str, messages: &[Msg], c: &Constraint) -> std::result::Result<LlmOutput, BackendError> {
    if ep.base_url.is_empty() {
        return Err(BackendError::NotConfigured("ollama base_url 为空".into()));
    }
    let url = format!("{}/api/chat", ep.base_url.trim_end_matches('/'));
    let mut msgs = vec![json!({"role": "system", "content": sys})];
    for m in messages {
        msgs.push(json!({"role": m.role, "content": m.content}));
    }
    let body = json!({
        "model": ep.model_id,
        "messages": msgs,
        "stream": false,
        "format": "json",
        "options": {"temperature": c.temperature, "num_predict": c.max_tokens},
    });
    let v = ne_engine::http::post_json_value(&url, &[], &body, std::time::Duration::from_secs(60))
        .map_err(|e| BackendError::Transport(e.to_string()))?;
    let content_str = v["message"]["content"].as_str().unwrap_or("");
    let content = prompt_lib::extract_json(content_str).map_err(|e| BackendError::Protocol(e.to_string()))?;
    let token_in = v["prompt_eval_count"].as_i64().unwrap_or(0);
    let token_out = v["eval_count"].as_i64().unwrap_or(0);
    Ok(LlmOutput { content, raw: content_str.to_string(), token_in, token_out, backend: "ollama".into(), model: ep.model_id.clone(), cache_hit: false })
}

#[cfg(not(feature = "llm_integration"))]
fn ollama_chat(_ep: &Endpoint, _sys: &str, _messages: &[Msg], _c: &Constraint) -> std::result::Result<LlmOutput, BackendError> {
    Err(BackendError::Unavailable("默认构建无出网能力（feature `llm_integration` 未启用）".into()))
}

#[cfg(feature = "llm_integration")]
fn openai_chat(ep: &Endpoint, sys: &str, messages: &[Msg], c: &Constraint) -> std::result::Result<LlmOutput, BackendError> {
    if ep.base_url.is_empty() {
        return Err(BackendError::NotConfigured("openai_compat base_url 为空".into()));
    }
    let url = format!("{}/chat/completions", ep.base_url.trim_end_matches('/'));
    let mut msgs = vec![json!({"role": "system", "content": sys})];
    for m in messages {
        msgs.push(json!({"role": m.role, "content": m.content}));
    }
    let body = json!({
        "model": ep.model_id,
        "messages": msgs,
        "max_tokens": c.max_tokens,
        "temperature": c.temperature,
        "response_format": {"type": "json_object"},
        "stream": false,
    });
    let mut headers: Vec<(String, String)> = vec![("Content-Type".into(), "application/json".into())];
    if !ep.api_key.is_empty() {
        headers.push(("Authorization".into(), format!("Bearer {}", ep.api_key)));
    }
    let v = ne_engine::http::post_json_value(&url, &headers, &body, std::time::Duration::from_secs(60))
        .map_err(|e| BackendError::Transport(e.to_string()))?;
    let content_str = v["choices"][0]["message"]["content"].as_str().unwrap_or("");
    let content = prompt_lib::extract_json(content_str).map_err(|e| BackendError::Protocol(e.to_string()))?;
    let token_in = v["usage"]["prompt_tokens"].as_i64().unwrap_or(0);
    let token_out = v["usage"]["completion_tokens"].as_i64().unwrap_or(0);
    Ok(LlmOutput { content, raw: content_str.to_string(), token_in, token_out, backend: "openai_compat".into(), model: ep.model_id.clone(), cache_hit: false })
}

#[cfg(not(feature = "llm_integration"))]
fn openai_chat(_ep: &Endpoint, _sys: &str, _messages: &[Msg], _c: &Constraint) -> std::result::Result<LlmOutput, BackendError> {
    Err(BackendError::Unavailable("默认构建无出网能力（feature `llm_integration` 未启用）".into()))
}

/// 构造后端：`mock` 恒为 DryRunMock；其余为真实后端。
pub fn make_backend(name: &str, cfg: &LlmConfig) -> Box<dyn LlmBackend> {
    if name == "dry_run_mock" || name == "mock" {
        Box::new(DryRunMock::new("mock"))
    } else {
        Box::new(RealBackend::from_config(cfg))
    }
}

/// 可用后端清单（供 gate-check / `llm backends`）。
pub fn available_backends(cfg: &LlmConfig) -> Value {
    json!({
        "real_backends": ["ollama", "openai_compat"],
        "test_backend": "dry_run_mock",
        "default_backend": cfg.default_backend,
        "feature_enabled": cfg!(feature = "llm_integration"),
        "note": if cfg!(feature = "llm_integration") { "feature 已开；仍需三重闸门" } else { "默认构建：真实后端为死分支，仅 dry_run_mock 可用" },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_mock_is_deterministic_no_network() {
        let b = DryRunMock::new("m");
        let c = Constraint { max_tokens: 10, temperature: 0.0, schema: json!({"type":"object","required":["verdict"],"properties":{"verdict":{"type":"string"}}}) };
        let a = b.chat("s", &[Msg::user("u")], &c).unwrap();
        let d = b.chat("s", &[Msg::user("u")], &c).unwrap();
        assert_eq!(a.content, d.content);
        assert_eq!(a.backend, "dry_run_mock");
        assert_eq!(a.content["verdict"], "");
    }

    #[test]
    fn dry_run_mock_canned_parses() {
        let b = DryRunMock::with_canned("m", "{\"verdict\":\"OK\"}");
        let out = b.chat("s", &[], &Constraint::default()).unwrap();
        assert_eq!(out.content["verdict"], "OK");
    }

    #[test]
    fn real_backend_unavailable_without_feature() {
        // 默认构建下真实后端必须报 Unavailable（死分支）
        if !cfg!(feature = "llm_integration") {
            let b = RealBackend::Ollama(Endpoint { base_url: "http://127.0.0.1:11434".into(), api_key: "".into(), model_id: "m".into() });
            let r = b.chat("s", &[Msg::user("u")], &Constraint::default());
            assert!(matches!(r, Err(BackendError::Unavailable(_))));
        }
    }

    #[test]
    fn minimal_schema_object() {
        let s = json!({"type":"object","required":["a","b"],"properties":{"a":{"type":"string"},"b":{"type":"array"}}});
        let m = minimal_for_schema(&s);
        assert_eq!(m["a"], "");
        assert_eq!(m["b"], json!([]));
    }

    #[test]
    fn available_backends_reports_feature() {
        let v = available_backends(&LlmConfig::default());
        assert_eq!(v["default_backend"], "ollama");
        assert_eq!(v["test_backend"], "dry_run_mock");
    }

    #[test]
    fn make_backend_mock_and_real() {
        let cfg = LlmConfig::default();
        assert_eq!(make_backend("mock", &cfg).name(), "dry_run_mock");
        assert_eq!(make_backend("ollama", &cfg).name(), "ollama");
    }
}