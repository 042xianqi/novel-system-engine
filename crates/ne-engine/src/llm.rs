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
}

impl Default for LlmParams {
    fn default() -> Self {
        LlmParams {
            base_url: "https://api.agnes-ai.cn/v1".into(),
            model: String::new(),
            keys: vec![],
            max_tokens: 4096,
            temperature: 1.0,
            enable_thinking: false,
            retries: 3,
            timeout: Duration::from_secs(600),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

impl Message {
    pub fn system(c: impl Into<String>) -> Self {
        Message { role: "system".into(), content: c.into() }
    }
    pub fn user(c: impl Into<String>) -> Self {
        Message { role: "user".into(), content: c.into() }
    }
}

/// 调用结果：正文 + 推理内容（thinking 透传）。
#[derive(Debug, Clone)]
pub struct LlmOutput {
    pub content: String,
    pub reasoning: String,
}

fn body_json(p: &LlmParams, messages: &[Message]) -> serde_json::Value {
    serde_json::json!({
        "model": p.model,
        "messages": messages.iter().map(|m| serde_json::json!({"role": m.role, "content": m.content})).collect::<Vec<_>>(),
        "max_tokens": p.max_tokens,
        "temperature": p.temperature,
        "enable_thinking": p.enable_thinking,
    })
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
                    if !content.trim().is_empty() {
                        return Ok(LlmOutput { content, reasoning });
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
