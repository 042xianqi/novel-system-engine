//! LLM 配置体系（world-gov，第 17 批）—— 从文件/环境变量读，**不硬编码**。
//!
//! # 配置来源（优先级：环境变量 > 文件 > 默认）
//! - 文件：`{work-root}/config/llm.toml`（不存在则由 `llm config init` 生成注释模板）
//! - 环境变量：`WOGOV_LLM_BACKEND` / `WOGOV_LLM_BASE_URL` / `WOGOV_LLM_KEY` / `WOGOV_LLM_MODEL`
//!
//! # 为什么手写 TOML 子集
//! 仓库既有范式是**零依赖手写**（USTAR / gzip / SHA-1 / HTTP 均自实现）。引入 `toml` crate
//! 会拖入 winnow/toml_edit 等 6+ crate。本模块只支持配置所需的最小 TOML 子集：
//! - 顶层键值 `k = v`；表头 `[name]` / `[name.sub]`
//! - 值为：字符串 `"..."`（含转义 \\ \" \n \t）、整数、浮点、布尔、字符串数组 `["a","b"]`
//! - 注释 `#`、空行；**不支持**多行字符串/数组表（配置用不上）。
//! 解析失败给**行号级**报错，绝不静默吞。
use anyhow::{bail, Result};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

/// 配置文件名（相对 work-root）。
pub const CONFIG_REL: &str = "config/llm.toml";

/// 一个端点的配置。
#[derive(Debug, Clone, Default)]
pub struct Endpoint {
    pub base_url: String,
    pub api_key: String,
    pub model_id: String,
}

/// 完整 LLM 配置。
#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub default_backend: String,
    /// backend 名 → 端点
    pub endpoints: std::collections::BTreeMap<String, Endpoint>,
    pub max_resp_tokens: u32,
    pub timeout_sec: u64,
    pub max_retry: u8,
    pub rps_limit: u32,
    pub inference_cache_enable: bool,
    pub cache_ttl_days: u64,
    /// 熔断：连续失败 N 次 → 临时降级
    pub breaker_threshold: u32,
}

impl Default for LlmConfig {
    fn default() -> Self {
        let mut endpoints = std::collections::BTreeMap::new();
        endpoints.insert(
            "ollama".to_string(),
            Endpoint { base_url: "http://127.0.0.1:11434".into(), api_key: String::new(), model_id: "llama3.1".into() },
        );
        endpoints.insert("openai_compat".to_string(), Endpoint::default());
        LlmConfig {
            default_backend: "ollama".into(),
            endpoints,
            max_resp_tokens: 2048,
            timeout_sec: 60,
            max_retry: 1,
            rps_limit: 0,
            inference_cache_enable: true,
            cache_ttl_days: 30,
            breaker_threshold: 5,
        }
    }
}

/// 生成的注释模板（`llm config init` 写盘用）。
pub fn template_toml() -> String {
    r#"# world-gov LLM 配置（第 17 批）—— 全部可选；不存在则用内置默认。
# ⚠ 本文件不随构建入 git；可自由修改。环境变量优先级高于本文件。
# 三重安全闸门默认锁死：必须同时满足 feature=llm_integration + WOGOV_ALLOW_REAL_LLM=yes + --real-semantic 才会真正出网。
# 注意：TOML 顶层标量键必须在所有 [表头] 之前，否则会被归入上一个表。

default_backend = "ollama"
max_resp_tokens = 2048
timeout_sec = 60
max_retry = 1
rps_limit = 0
inference_cache_enable = true
cache_ttl_days = 30
breaker_threshold = 5

# 本地 Ollama（默认，无密钥）
[endpoints.ollama]
base_url = "http://127.0.0.1:11434"
api_key = ""
model_id = "llama3.1"

# OpenAI 兼容端点（DeepSeek 等）；key 可留空并用环境变量 WOGOV_LLM_KEY 覆盖
[endpoints.openai_compat]
base_url = "https://api.deepseek.com/v1"
api_key = ""
model_id = "deepseek-chat"
"#
    .to_string()
}

/// 解析 TOML 子集 → 扁平 JSON 对象（键用 `.` 连接）。
pub fn parse_toml_subset(text: &str) -> Result<Value> {
    let mut root = Map::new();
    let mut table = String::new();
    for (lineno, raw) in text.lines().enumerate() {
        let n = lineno + 1;
        let line = strip_comment(raw).trim().to_string();
        if line.is_empty() {
            continue;
        }
        if let Some(inner) = line.strip_prefix('[') {
            let name = inner.strip_suffix(']').ok_or_else(|| anyhow::anyhow!("第 {n} 行：表头缺少 ']'"))?;
            table = name.trim().to_string();
            continue;
        }
        let (k, v) = line
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("第 {n} 行：缺少 '='：{line}"))?;
        let key = k.trim();
        if key.is_empty() {
            bail!("第 {n} 行：键为空");
        }
        let full = if table.is_empty() { key.to_string() } else { format!("{table}.{key}") };
        let val = parse_value(v.trim(), n)?;
        root.insert(full, val);
    }
    Ok(Value::Object(root))
}

/// 去行内注释（`#` 之后），但**保留字符串内**的 `#`。
fn strip_comment(line: &str) -> String {
    let mut out = String::new();
    let mut in_str = false;
    let mut escaped = false;
    for c in line.chars() {
        if escaped {
            out.push(c);
            escaped = false;
            continue;
        }
        match c {
            '\\' if in_str => { out.push(c); escaped = true; }
            '"' => { in_str = !in_str; out.push(c); }
            '#' if !in_str => break,
            _ => out.push(c),
        }
    }
    out
}

fn parse_value(s: &str, n: usize) -> Result<Value> {
    if s.is_empty() {
        bail!("第 {n} 行：值为空");
    }
    if s == "true" { return Ok(json!(true)); }
    if s == "false" { return Ok(json!(false)); }
    if let Some(inner) = s.strip_prefix('[') {
        let inner = inner.strip_suffix(']').ok_or_else(|| anyhow::anyhow!("第 {n} 行：数组缺少 ']'"))?;
        let mut arr = Vec::new();
        for part in split_top_commas(inner) {
            let p = part.trim();
            if p.is_empty() { continue; }
            arr.push(parse_value(p, n)?);
        }
        return Ok(Value::Array(arr));
    }
    if s.starts_with('"') {
        let inner = s.strip_prefix('"').and_then(|x| x.strip_suffix('"'))
            .ok_or_else(|| anyhow::anyhow!("第 {n} 行：字符串缺少结束 '\"'"))?;
        return Ok(json!(unescape(inner)));
    }
    if let Ok(i) = s.parse::<i64>() { return Ok(json!(i)); }
    if let Ok(f) = s.parse::<f64>() { return Ok(json!(f)); }
    bail!("第 {n} 行：无法识别的值：{s}")
}

fn split_top_commas(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_str = false;
    for c in s.chars() {
        match c {
            '"' => { in_str = !in_str; cur.push(c); }
            ',' if !in_str => { out.push(std::mem::take(&mut cur)); }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() { out.push(cur); }
    out
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(o) => { out.push('\\'); out.push(o); }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn get_str(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(|s| s.to_string())
}
fn get_u64(v: &Value, key: &str) -> Option<u64> {
    v.get(key).and_then(|x| x.as_u64())
}
fn get_bool(v: &Value, key: &str) -> Option<bool> {
    v.get(key).and_then(|x| x.as_bool())
}

/// 从扁平 JSON 构造 LlmConfig（缺省走默认）。
pub fn from_flat(v: &Value) -> LlmConfig {
    let mut cfg = LlmConfig::default();
    if let Some(b) = get_str(v, "default_backend") {
        if !b.is_empty() { cfg.default_backend = b; }
    }
    // 端点：从扁平键 `endpoints.<name>.<field>` 还原（扁平解析不产生嵌套对象）
    let mut names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if let Some(m) = v.as_object() {
        for k in m.keys() {
            if let Some(rest) = k.strip_prefix("endpoints.") {
                if let Some((name, _field)) = rest.split_once('.') {
                    names.insert(name.to_string());
                }
            }
        }
    }
    for name in names {
        let e = cfg.endpoints.entry(name.clone()).or_default();
        if let Some(s) = get_str(v, &format!("endpoints.{name}.base_url")) { e.base_url = s; }
        if let Some(s) = get_str(v, &format!("endpoints.{name}.api_key")) { e.api_key = s; }
        if let Some(s) = get_str(v, &format!("endpoints.{name}.model_id")) { e.model_id = s; }
    }
    if let Some(n) = get_u64(v, "max_resp_tokens") { cfg.max_resp_tokens = n as u32; }
    if let Some(n) = get_u64(v, "timeout_sec") { cfg.timeout_sec = n; }
    if let Some(n) = get_u64(v, "max_retry") { cfg.max_retry = n as u8; }
    if let Some(n) = get_u64(v, "rps_limit") { cfg.rps_limit = n as u32; }
    if let Some(b) = get_bool(v, "inference_cache_enable") { cfg.inference_cache_enable = b; }
    if let Some(n) = get_u64(v, "cache_ttl_days") { cfg.cache_ttl_days = n; }
    if let Some(n) = get_u64(v, "breaker_threshold") { cfg.breaker_threshold = n as u32; }
    cfg
}

/// 加载配置：文件（若存在）→ 再叠加环境变量覆盖。
pub fn load(work_root: &Path) -> Result<LlmConfig> {
    let path = work_root.join(CONFIG_REL);
    let mut cfg = if path.is_file() {
        let text = std::fs::read_to_string(&path)?;
        let flat = parse_toml_subset(&text)?;
        from_flat(&flat)
    } else {
        LlmConfig::default()
    };
    apply_env(&mut cfg);
    Ok(cfg)
}

/// 环境变量覆盖（优先级最高）。
pub fn apply_env(cfg: &mut LlmConfig) {
    if let Ok(v) = std::env::var("WOGOV_LLM_BACKEND") { if !v.is_empty() { cfg.default_backend = v; } }
    let backend = cfg.default_backend.clone();
    let e = cfg.endpoints.entry(backend).or_default();
    if let Ok(v) = std::env::var("WOGOV_LLM_BASE_URL") { if !v.is_empty() { e.base_url = v; } }
    if let Ok(v) = std::env::var("WOGOV_LLM_KEY") { if !v.is_empty() { e.api_key = v; } }
    if let Ok(v) = std::env::var("WOGOV_LLM_MODEL") { if !v.is_empty() { e.model_id = v; } }
}

/// 当前生效端点（按 default_backend）。
pub fn active_endpoint(cfg: &LlmConfig) -> Endpoint {
    cfg.endpoints.get(&cfg.default_backend).cloned().unwrap_or_default()
}

/// 生成注释模板到 `{work-root}/config/llm.toml`（已存在则不覆盖，除非 overwrite）。
pub fn init_template(work_root: &Path, overwrite: bool) -> Result<PathBuf> {
    let path = work_root.join(CONFIG_REL);
    if path.exists() && !overwrite {
        return Ok(path);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, template_toml())?;
    Ok(path)
}

/// 安全快照（**绝不回显 key 本体**）。
pub fn safe_snapshot(cfg: &LlmConfig) -> Value {
    let mut eps = Map::new();
    for (name, e) in &cfg.endpoints {
        eps.insert(name.clone(), json!({
            "base_configured": !e.base_url.is_empty(),
            "base_loopback": e.base_url.contains("127.0.0.1") || e.base_url.contains("localhost"),
            "api_key_configured": !e.api_key.is_empty(),
            "model_configured": !e.model_id.is_empty(),
        }));
    }
    json!({
        "default_backend": cfg.default_backend,
        "endpoints": eps,
        "max_resp_tokens": cfg.max_resp_tokens,
        "timeout_sec": cfg.timeout_sec,
        "max_retry": cfg.max_retry,
        "rps_limit": cfg.rps_limit,
        "inference_cache_enable": cfg.inference_cache_enable,
        "cache_ttl_days": cfg.cache_ttl_days,
        "breaker_threshold": cfg.breaker_threshold,
        "note": "仅报布尔与数值；地址/Key 不回显。",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_toml() {
let v = parse_toml_subset(template_toml().as_str()).unwrap();
        assert_eq!(v["default_backend"], "ollama");
        assert_eq!(v["endpoints.ollama.base_url"], "http://127.0.0.1:11434");
        assert_eq!(v["max_resp_tokens"], 2048);
        assert_eq!(v["inference_cache_enable"], true);
    }

    #[test]
    fn parse_array_and_comments() {
        let v = parse_toml_subset("xs = [\"a\", \"b\", \"c\"]\n# comment\n").unwrap();
        assert_eq!(v["xs"], json!(["a", "b", "c"]));
    }

    #[test]
    fn hash_inside_string_is_kept() {
        let v = parse_toml_subset("k = \"a#b\" # real comment\n").unwrap();
        assert_eq!(v["k"], "a#b");
    }

    #[test]
    fn parse_error_has_line_number() {
        let e = parse_toml_subset("a = 1\nb =\n").unwrap_err().to_string();
        assert!(e.contains("第 2 行"), "应报行号：{e}");
    }

    #[test]
    fn floats_and_bools_and_negative() {
        let v = parse_toml_subset("x = 0.0\ny = false\nz = -3\n").unwrap();
        assert_eq!(v["x"], 0.0);
        assert_eq!(v["y"], false);
        assert_eq!(v["z"], -3);
    }

    #[test]
    fn from_flat_builds_config() {
        let v = parse_toml_subset(template_toml().as_str()).unwrap();
        let cfg = from_flat(&v);
        assert_eq!(cfg.default_backend, "ollama");
        assert_eq!(cfg.endpoints["ollama"].base_url, "http://127.0.0.1:11434");
        assert_eq!(cfg.endpoints["openai_compat"].model_id, "deepseek-chat");
        assert_eq!(cfg.max_retry, 1);
    }

    #[test]
    fn safe_snapshot_hides_secrets() {
        let mut cfg = LlmConfig::default();
        let e = cfg.endpoints.get_mut("openai_compat").unwrap();
        e.api_key = "sk-secret".into();
        e.base_url = "https://api.example.invalid/v1".into();
        let s = safe_snapshot(&cfg);
        let txt = s.to_string();
        assert!(!txt.contains("sk-secret"), "key 不回显");
        assert!(!txt.contains("example.invalid"), "URL 不回显");
        assert_eq!(s["endpoints"]["openai_compat"]["api_key_configured"], true);
    }

    #[test]
    fn init_template_writes_then_skips() {
        let dir = std::env::temp_dir().join(format!("wg_llmcfg_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let p = init_template(&dir, false).unwrap();
        assert!(p.is_file());
        std::fs::write(&p, "x = 1\n").unwrap();
        init_template(&dir, false).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "x = 1\n");
        init_template(&dir, true).unwrap();
        assert!(std::fs::read_to_string(&p).unwrap().contains("default_backend"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_file_is_default() {
        let dir = std::env::temp_dir().join(format!("wg_llmcfg_absent_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cfg = load(&dir).unwrap();
        assert_eq!(cfg.default_backend, "ollama");
        assert!(active_endpoint(&cfg).base_url.contains("127.0.0.1"));
    }
}