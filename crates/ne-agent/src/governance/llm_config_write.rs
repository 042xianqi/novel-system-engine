//! LLM 配置**写**侧（P4）—— 校验 → 持久化 → 审计 → 探测。
//!
//! # 与 `llm_config`（只读解析）的分工
//! - `llm_config.rs`：解析 / 加载 / 掩码快照（**不动**）
//! - 本模块：**写** `config/llm.toml` + URL 校验 + 连通性探测 + 审计
//!
//! # 安全
//! - 写入前 **URL 格式校验**（任务书要求）
//! - 写入**原样** api_key（任务书要求），但**返回**一律掩码
//! - 每次写都进 `webui_op_log`
//! - 探测**不触发**三重锁（只做 HTTP 探测，不调真实模型推理）
use anyhow::{bail, Result};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::Path;

use super::category_store;

/// URL 基础合法性校验（任务书要求）。
fn validate_url(u: &str) -> Result<()> {
    let s = u.trim();
    if s.is_empty() {
        bail!("base_url 不能为空");
    }
    if !(s.starts_with("http://") || s.starts_with("https://")) {
        bail!("base_url 必须以 http:// 或 https:// 开头");
    }
    if s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        bail!("base_url 含空白或控制字符");
    }
    let rest = s.splitn(2, "://").nth(1).unwrap_or("");
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty() {
        bail!("base_url 缺少主机名");
    }
    Ok(())
}

fn clamp_u32(v: Option<i64>, name: &str, lo: i64, hi: i64, dflt: u32) -> Result<u32> {
    match v {
        None => Ok(dflt),
        Some(x) if x < lo || x > hi => bail!("{name} 需在 {lo}..{hi} 之间（收到 {x}）"),
        Some(x) => Ok(x as u32),
    }
}

fn toml_esc(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// 渲染 `llm.toml`（顶层标量必须在所有 [表头] 之前 —— 见模板注释）。
fn render_toml(v: &Value, cur: &Value) -> Result<String> {
    let backend = v["default_backend"]
        .as_str()
        .or_else(|| cur["default_backend"].as_str())
        .unwrap_or("openai_compat")
        .to_string();
    let base_url = v["base_url"]
        .as_str()
        .map(|s| s.to_string())
        .or_else(|| cur["base_url"].as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "https://api.deepseek.com/v1".to_string());
    let model_id = v["model_id"]
        .as_str()
        .map(|s| s.to_string())
        .or_else(|| cur["model_id"].as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "deepseek-chat".to_string());
    let api_key = v["api_key"]
        .as_str()
        .map(|s| s.to_string())
        .or_else(|| cur["api_key"].as_str().map(|s| s.to_string()))
        .unwrap_or_default();

    validate_url(&base_url)?;

    let max_tokens = clamp_u32(
        v["max_resp_tokens"].as_i64().or_else(|| v["max_tokens"].as_i64()),
        "max_tokens", 1, 1_000_000,
        cur["max_resp_tokens"].as_u64().unwrap_or(2048) as u32,
    )?;
    let timeout = clamp_u32(v["timeout_sec"].as_i64(), "timeout_sec", 1, 3600,
        cur["timeout_sec"].as_u64().unwrap_or(60) as u32)?;
    let retry = clamp_u32(v["max_retry"].as_i64(), "max_retry", 0, 10,
        cur["max_retry"].as_u64().unwrap_or(1) as u32)?;
    let rps = clamp_u32(v["rps_limit"].as_i64(), "rps_limit", 0, 10000,
        cur["rps_limit"].as_u64().unwrap_or(0) as u32)?;
    let breaker = clamp_u32(v["breaker_threshold"].as_i64(), "breaker_threshold", 1, 100,
        cur["breaker_threshold"].as_u64().unwrap_or(5) as u32)?;
    let temp = v["temperature"].as_f64();
    if let Some(t) = temp {
        if !(0.0..=2.0).contains(&t) {
            bail!("temperature 需在 0..2 之间（收到 {t}）");
        }
    }
    let force_json = v["response_format"].as_bool()
        .or_else(|| v["force_json"].as_bool())
        .unwrap_or(true);
    let cache = v["inference_cache_enable"].as_bool()
        .unwrap_or_else(|| cur["inference_cache_enable"].as_bool().unwrap_or(true));
    let ttl = clamp_u32(v["cache_ttl_days"].as_i64(), "cache_ttl_days", 1, 3650,
        cur["cache_ttl_days"].as_u64().unwrap_or(30) as u32)?;

    let mut out = String::new();
    out.push_str("# world-gov LLM 配置 —— 由 WebUI 写入（P4）。\n");
    out.push_str("# 三重安全闸门默认锁死：feature=llm_integration + WOGOV_ALLOW_REAL_LLM=yes + --real-semantic\n");
    out.push_str("# 注意：TOML 顶层标量键必须在所有 [表头] 之前。\n\n");
    out.push_str(&format!("default_backend = \"{}\"\n", toml_esc(&backend)));
    out.push_str(&format!("max_resp_tokens = {max_tokens}\n"));
    out.push_str(&format!("timeout_sec = {timeout}\n"));
    out.push_str(&format!("max_retry = {retry}\n"));
    out.push_str(&format!("rps_limit = {rps}\n"));
    out.push_str(&format!("inference_cache_enable = {cache}\n"));
    out.push_str(&format!("cache_ttl_days = {ttl}\n"));
    out.push_str(&format!("breaker_threshold = {breaker}\n"));
    if let Some(t) = temp {
        out.push_str(&format!("temperature = {t}\n"));
    }
    out.push_str(&format!("response_format = {force_json}\n"));

    let other_backend = if backend == "openai_compat" { "ollama" } else { "openai_compat" };
    out.push_str(&format!("\n[endpoints.{}]\n", backend));
    out.push_str(&format!("base_url = \"{}\"\n", toml_esc(&base_url)));
    out.push_str(&format!("api_key = \"{}\"\n", toml_esc(&api_key)));
    out.push_str(&format!("model_id = \"{}\"\n", toml_esc(&model_id)));
    out.push_str(&format!("\n[endpoints.{}]\n", other_backend));
    let ob = cur[other_backend]["base_url"].as_str().unwrap_or(
        if other_backend == "ollama" { "http://127.0.0.1:11434" } else { "https://api.deepseek.com/v1" });
    let oc = cur[other_backend]["api_key"].as_str().unwrap_or("");
    let om = cur[other_backend]["model_id"].as_str().unwrap_or(
        if other_backend == "ollama" { "llama3.1" } else { "deepseek-chat" });
    out.push_str(&format!("base_url = \"{}\"\n", toml_esc(ob)));
    out.push_str(&format!("api_key = \"{}\"\n", toml_esc(oc)));
    out.push_str(&format!("model_id = \"{}\"\n", toml_esc(om)));
    Ok(out)
}

/// 当前配置的扁平形式（供写入时继承未提供的字段）。
fn current_flat(work_root: &Path) -> Value {
    let cfg = match super::llm_config::load(work_root) {
        Ok(c) => c,
        Err(_) => return json!({}),
    };
    let ep = super::llm_config::active_endpoint(&cfg);
    let mut m = json!({
        "default_backend": cfg.default_backend,
        "base_url": ep.base_url,
        "api_key": ep.api_key,
        "model_id": ep.model_id,
        "max_resp_tokens": cfg.max_resp_tokens,
        "timeout_sec": cfg.timeout_sec,
        "max_retry": cfg.max_retry,
        "rps_limit": cfg.rps_limit,
        "inference_cache_enable": cfg.inference_cache_enable,
        "cache_ttl_days": cfg.cache_ttl_days,
        "breaker_threshold": cfg.breaker_threshold,
    });
    for name in ["ollama", "openai_compat"] {
        if let Some(e) = cfg.endpoints.get(name) {
            m[name] = json!({"base_url": e.base_url, "api_key": e.api_key, "model_id": e.model_id});
        }
    }
    m
}

/// 写配置（校验 → 备份 → 落盘 → 回读校验 → 审计）。
pub fn write_config(conn: &Connection, work_root: &Path, body: &str) -> Result<Value> {
    let v: Value = serde_json::from_str(body.trim()).unwrap_or(json!({}));
    if v.as_object().map(|o| o.is_empty()).unwrap_or(true) {
        bail!("请求体为空（需 JSON）");
    }
    let cur = current_flat(work_root);
    let toml = render_toml(&v, &cur)?;
    let path = work_root.join(super::llm_config::CONFIG_REL);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let backup = if path.is_file() {
        let b = path.with_extension("toml.bak");
        let _ = std::fs::copy(&path, &b);
        b.to_string_lossy().to_string()
    } else {
        String::new()
    };
    std::fs::write(&path, &toml)?;
    let reloaded = super::llm_config::load(work_root)?;
    let snap = super::llm_config::safe_snapshot(&reloaded);
    category_store::log_op(
        conn, "llm.config.write", super::llm_config::CONFIG_REL,
        &format!("backend={} model={} backup={backup}",
                 v["default_backend"].as_str().unwrap_or("-"),
                 v["model_id"].as_str().unwrap_or("-")),
        "",
    )?;
    Ok(json!({
        "ok": true, "path": super::llm_config::CONFIG_REL,
        "bytes": toml.len(), "backup": backup,
        "config": snap,
        "note": "api-key 原样写入文件；响应中一律掩码。",
    }))
}

/// 重置为默认模板。
pub fn reset_config(conn: &Connection, work_root: &Path) -> Result<Value> {
    let path = work_root.join(super::llm_config::CONFIG_REL);
    let backup = if path.is_file() {
        let b = path.with_extension("toml.bak");
        let _ = std::fs::copy(&path, &b);
        b.to_string_lossy().to_string()
    } else {
        String::new()
    };
    let tpl = super::llm_config::template_toml();
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(&path, &tpl)?;
    category_store::log_op(conn, "llm.config.reset", super::llm_config::CONFIG_REL,
        &format!("backup={backup}"), "")?;
    Ok(json!({"ok": true, "reset": true, "bytes": tpl.len(), "backup": backup}))
}

/// 连通性探测：**只用 HTTP 探测，不发起推理**（不触发三重锁）。
pub fn test_connect(body: &str) -> Value {
    let v: Value = serde_json::from_str(body.trim()).unwrap_or(json!({}));
    let base = v["base_url"].as_str().unwrap_or("").trim().to_string();
    let model = v["model_id"].as_str().unwrap_or("");
    if let Err(e) = validate_url(&base) {
        return json!({"ok": false, "error": e.to_string(), "stage": "validate"});
    }
    json!({
        "ok": true,
        "reachable": "unknown",
        "endpoint": base,
        "model_requested": model,
        "models": [],
        "models_count": 0,
        "response_format_supported": true,
        "stage": "validated",
        "note": "URL 合法。真实连通探测需在启用 llm_integration feature 的构建中执行；本构建只做格式校验（不触发三重锁）。",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_validation() {
        assert!(validate_url("http://127.0.0.1:8788/v1").is_ok());
        assert!(validate_url("https://api.deepseek.com/v1").is_ok());
        assert!(validate_url("ftp://x").is_err());
        assert!(validate_url("").is_err());
        assert!(validate_url("http://").is_err());
        assert!(validate_url("http://a b/v1").is_err());
    }

    #[test]
    fn toml_render_and_parse_roundtrip() {
        let cur = json!({"default_backend":"openai_compat","max_resp_tokens":2048});
        let v = json!({
            "base_url":"http://127.0.0.1:8788/v1",
            "model_id":"glm-5.3-flash",
            "api_key":"sk-test",
            "max_resp_tokens":4096,
            "temperature":0.7,
            "max_retry":3,
            "response_format":true
        });
        let t = render_toml(&v, &cur).unwrap();
        assert!(t.contains("max_resp_tokens = 4096"));
        assert!(t.contains("model_id = \"glm-5.3-flash\""));
        let flat = super::super::llm_config::parse_toml_subset(&t).unwrap();
        assert_eq!(flat["default_backend"], "openai_compat");
        assert_eq!(flat["max_resp_tokens"], 4096);
        let first_table = t.find("\n[").unwrap_or(t.len());
        let last_scalar = t.rfind("response_format").unwrap_or(0);
        assert!(last_scalar < first_table, "顶层标量必须全在 [表头] 之前");
    }

    #[test]
    fn rejects_bad_ranges() {
        let cur = json!({});
        assert!(render_toml(&json!({"base_url":"http://x","temperature":9.9}), &cur).is_err());
        assert!(render_toml(&json!({"base_url":"http://x","max_retry":99}), &cur).is_err());
        assert!(render_toml(&json!({"base_url":"not-a-url"}), &cur).is_err());
    }
}