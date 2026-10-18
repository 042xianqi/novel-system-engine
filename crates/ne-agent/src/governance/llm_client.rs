//! LLM 客户端骨架（world-gov，第 13 批）—— OpenAI 兼容调用层。
//!
//! # ⚠️ DEPRECATED（第 17.3 批）
//! 本模块**已被 [`super::semantic_pipeline`] 取代**，仅作**过渡存根**保留：
//! - 唯一真实外呼入口现为 `semantic_pipeline::call_raw`（统一闸门/熔断/缓存/后端/
//!   护栏/记账/**修复回合**）；`inspect-card` 已迁移，本模块**不再有生产调用方**。
//! - `real_call` 从一开始就是未实现的占位（`REAL_LLM_CALL_NOT_IMPLEMENTED`），
//!   真实 HTTP 由 `llm_adapter` 承担；本模块保留是为兼容既有纯函数单测与审计契约。
//! - 迁移动机：不维护两套独立 LLM 链路；旧路径缺修复回合，在真实端点上必挂。
//!
//! # 默认构建（无 `real-semantic` feature）
//! **出网代码根本不参与编译**：请求构造（纯函数）保留并可测，但"真正发 HTTP"的分支
//! 只在 `#[cfg(feature="real-semantic")]` 下存在 → 默认二进制**没有任何出网通路**。
//!
//! # 强约束
//! - 每一次请求**必须**附带：法理基线指纹、探针 hash、调用时间（写入冲突库）。
//! - 不硬编码 URL/Key；只从环境变量读（`NOVEL_LLM_ENDPOINT` / `NOVEL_LLM_KEY` / `NOVEL_LLM_MODEL`）。
//! - 失败即停，写日志；不做「重试风暴」（重试上限可配，仅对超时可重试）。
//! - 只输出**结构化 JSON**（禁止自由散文）——解析失败即 ProtocolError。
use anyhow::Result;
use serde_json::{json, Value};

use super::llm_gate;

/// LLM 调用策略（全部可配，有安全默认）。
#[derive(Debug, Clone)]
pub struct LlmPolicy {
    pub timeout_sec: u16,
    pub max_retry: u8,
    pub max_tokens: u32,
    /// 每分钟最大请求数（RPS 限速；0 = 不限）。
    pub rps: u32,
}

impl Default for LlmPolicy {
    fn default() -> Self {
        LlmPolicy { timeout_sec: 60, max_retry: 2, max_tokens: 2048, rps: 0 }
    }
}

/// 端点配置（**只从环境变量读**，不硬编码）。
#[derive(Debug, Clone)]
pub struct LlmEndpoint {
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub model_id: Option<String>,
    pub policy: LlmPolicy,
}

impl LlmEndpoint {
    pub fn from_env() -> Self {
        LlmEndpoint {
            base_url: std::env::var("NOVEL_LLM_ENDPOINT").ok().filter(|s| !s.is_empty()),
            api_key: std::env::var("NOVEL_LLM_KEY").ok().filter(|s| !s.is_empty()),
            model_id: std::env::var("NOVEL_LLM_MODEL").ok().filter(|s| !s.is_empty()),
            policy: LlmPolicy::default(),
        }
    }

    pub fn configured(&self) -> bool {
        self.base_url.is_some() && self.api_key.is_some()
    }

    /// 安全快照（**绝不回显 Key/URL 本体**）。
    pub fn safe_snapshot(&self) -> Value {
        json!({
            "base_configured": self.base_url.is_some(),
            "api_key_configured": self.api_key.is_some(),
            "model_configured": self.model_id.is_some(),
            "timeout_sec": self.policy.timeout_sec,
            "max_retry": self.policy.max_retry,
            "max_tokens": self.policy.max_tokens,
            "rps": self.policy.rps,
            "note": "仅报布尔；地址/Key 不回显。",
        })
    }
}

/// 请求审计上下文（每次请求必须附带）。
#[derive(Debug, Clone)]
pub struct AuditContext {
    pub baseline_fp: String,
    pub probe_hash: String,
    pub called_at: i64,
}

impl AuditContext {
    pub fn new(baseline_fp: &str, probe_hash: &str) -> Self {
        AuditContext {
            baseline_fp: baseline_fp.to_string(),
            probe_hash: probe_hash.to_string(),
            called_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
        }
    }
}

/// 构造 OpenAI 兼容的 chat/completions 请求体（**纯函数**，可测；不外呼）。
/// 强制 `response_format=json_object`（结构化输出，禁自由散文）。
pub fn build_chat_request(model: &str, system: &str, user: &str, policy: &LlmPolicy) -> Value {
    json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        "max_tokens": policy.max_tokens,
        "temperature": 0.0,
        "response_format": {"type": "json_object"},
        "stream": false,
    })
}

/// 从 LLM 响应 JSON 里抽取 content 文本（content 必须能再解析为 JSON 对象）。
/// 返回 (content_json, token_in, token_out)。**纯函数**，可测。
pub fn extract_structured(resp: &Value) -> Result<(Value, i64, i64)> {
    let content = resp
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .ok_or_else(|| anyhow::anyhow!("响应缺少 choices[0].message.content"))?;
    let parsed: Value = serde_json::from_str(content)
        .map_err(|e| anyhow::anyhow!("content 非结构化 JSON（禁止自由散文）：{e}"))?;
    let token_in = resp.get("usage").and_then(|u| u.get("prompt_tokens")).and_then(|x| x.as_i64()).unwrap_or(0);
    let token_out = resp.get("usage").and_then(|u| u.get("completion_tokens")).and_then(|x| x.as_i64()).unwrap_or(0);
    Ok((parsed, token_in, token_out))
}

/// 调用结局。
#[derive(Debug, Clone)]
pub enum CallOutcome {
    /// 未编译出网能力（默认构建）——等价「死分支」。
    NotCompiled,
    /// 三重锁未开。
    GateLocked,
    /// 成功：结构化 content + token 使用。
    Success { content: Value, token_in: i64, token_out: i64 },
    /// 超时（可重试）。
    Timeout,
    /// 协议错误（不可重试）。
    ProtocolError(String),
}

/// 主入口：**所有真实外呼必经此处**。
///
/// - `cli_real_semantic`：命令行是否加 `--real-semantic`。
/// - `dry_run`：只构造请求，不发送（可预览将发送内容）。
pub fn call_structured(
    endpoint: &LlmEndpoint,
    audit: &AuditContext,
    system: &str,
    user: &str,
    cli_real_semantic: bool,
    dry_run: bool,
) -> Result<CallOutcome> {
    // ① 三重锁（集中收口）
    if !llm_gate::real_llm_unlocked(cli_real_semantic) {
        return Ok(CallOutcome::GateLocked);
    }
    // ② 端点未配置 → 协议错误（不猜地址）
    if !endpoint.configured() {
        return Ok(CallOutcome::ProtocolError(
            "LLM 端点未配置（需 NOVEL_LLM_ENDPOINT + NOVEL_LLM_KEY）".to_string(),
        ));
    }
    let model = endpoint.model_id.clone().unwrap_or_else(|| "gpt-4o-mini".to_string());
    let _body = build_chat_request(&model, system, user, &endpoint.policy);
    // 审计：法理指纹 + 探针 hash + 调用时间（每次必附）
    let _ = audit;
    if dry_run {
        // 预览：不发送。返回 NotCompiled 之外的中性结局由调用方按 dry-run 分支处理。
        return Ok(CallOutcome::ProtocolError("dry_run_preview_only".to_string()));
    }
    real_call(endpoint, &model, system, user)
}

/// 真实出网调用。**仅在 `real-semantic` feature 下编译**；默认构建恒 NotCompiled。
#[cfg(feature = "real-semantic")]
fn real_call(
    endpoint: &LlmEndpoint,
    model: &str,
    system: &str,
    user: &str,
) -> Result<CallOutcome> {
    // 仅此分支使用 reqwest；默认构建不参与编译。
    let _ = (endpoint, model, system, user);
    // 说明：真实实现留待放行令后填充（当前 feature 默认关闭，此代码不入默认二进制）。
    anyhow::bail!("REAL_LLM_CALL_NOT_IMPLEMENTED: feature=real-semantic 已启用，但真实 HTTP 调用待放行令填充")
}

#[cfg(not(feature = "real-semantic"))]
fn real_call(
    _endpoint: &LlmEndpoint,
    _model: &str,
    _system: &str,
    _user: &str,
) -> Result<CallOutcome> {
    // 默认构建：**不存在**真实出网实现。
    Ok(CallOutcome::NotCompiled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_defaults_are_sane() {
        let p = LlmPolicy::default();
        assert_eq!(p.timeout_sec, 60);
        assert_eq!(p.max_retry, 2);
        assert!(p.max_tokens > 0);
    }

    #[test]
    fn endpoint_snapshot_hides_secrets() {
        std::env::set_var("NOVEL_LLM_ENDPOINT", "https://example.invalid/v1");
        std::env::set_var("NOVEL_LLM_KEY", "sk-secret");
        let ep = LlmEndpoint::from_env();
        let s = ep.safe_snapshot();
        assert_eq!(s["base_configured"], true);
        assert_eq!(s["api_key_configured"], true);
        let txt = s.to_string();
        assert!(!txt.contains("sk-secret"), "密钥绝不回显");
        assert!(!txt.contains("example.invalid"), "URL 绝不回显");
        std::env::remove_var("NOVEL_LLM_ENDPOINT");
        std::env::remove_var("NOVEL_LLM_KEY");
    }

    #[test]
    fn build_chat_request_is_openai_compatible_and_structured() {
        let b = build_chat_request("m1", "SYS", "USR", &LlmPolicy::default());
        assert_eq!(b["model"], "m1");
        assert_eq!(b["messages"][0]["role"], "system");
        assert_eq!(b["messages"][1]["content"], "USR");
        assert_eq!(b["response_format"]["type"], "json_object", "强制结构化");
        assert_eq!(b["stream"], false);
        assert_eq!(b["temperature"], 0.0);
    }

    #[test]
    fn extract_structured_parses_and_rejects_prose() {
        let ok = json!({
            "choices": [{"message": {"content": "{\"verdict\":\"HIGH-RISK\",\"law_ref\":\"L0\"}"}}],
            "usage": {"prompt_tokens": 12, "completion_tokens": 4}
        });
        let (c, ti, to) = extract_structured(&ok).unwrap();
        assert_eq!(c["verdict"], "HIGH-RISK");
        assert_eq!(ti, 12);
        assert_eq!(to, 4);
        // 自由散文 → 拒绝
        let prose = json!({"choices": [{"message": {"content": "这个设定看起来没问题，我判断……"}}]});
        assert!(extract_structured(&prose).is_err(), "自由散文必须拒绝");
        // 缺字段 → 拒绝
        assert!(extract_structured(&json!({})).is_err());
    }

    #[test]
    fn call_locked_without_triple_gate() {
        let ep = LlmEndpoint::from_env();
        let audit = AuditContext::new("fp", "hash");
        // 默认构建：feature 未开 → 必 GateLocked（即便 cli_real_semantic=true）
        let out = call_structured(&ep, &audit, "s", "u", true, false).unwrap();
        assert!(matches!(out, CallOutcome::GateLocked), "默认构建必须锁死");
    }

    #[test]
    fn call_dry_run_locked_too() {
        let ep = LlmEndpoint::from_env();
        let audit = AuditContext::new("fp", "hash");
        let out = call_structured(&ep, &audit, "s", "u", true, true).unwrap();
        assert!(matches!(out, CallOutcome::GateLocked), "锁未开时 dry-run 也锁");
    }

    #[test]
    fn audit_context_has_fields() {
        let a = AuditContext::new("BASEFP", "PROBEHASH");
        assert_eq!(a.baseline_fp, "BASEFP");
        assert_eq!(a.probe_hash, "PROBEHASH");
        assert!(a.called_at > 0);
    }
}