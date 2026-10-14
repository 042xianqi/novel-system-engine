//! LLM 深度管线收口（world-gov，第 17 批）—— 所有真实调用在此聚合。
//!
//! # 收口顺序（每一步都可短路）
//! 1. **三重闸门**：`llm_gate::real_llm_unlocked` 未开 → `GateLocked`（绝不外呼）。
//! 2. **熔断**：`llm_obs::breaker_tripped` → `BreakerOpen`（退化为静态模式 + 告警）。
//! 3. **缓存**：命中 → 直接返回（**不外呼**）；miss → 继续。
//! 4. **后端**：`llm_adapter` 调真实后端（默认构建为死分支，返回 `Unavailable`）。
//! 5. **护栏**：`prompt_lib::extract_json` + `validate_schema` + 实体白名单。
//! 6. **记账**：`llm_obs::record`（token/耗时/缓存命中/成败）；成功写缓存。
//!
//! # --dry-run
//! 只渲染 prompt + 算 cache_key，**不发请求、不写缓存**（交由 CLI 打印）。
use anyhow::Result;
use serde_json::{json, Map, Value};
use std::path::Path;

use super::{digest, inference_cache, llm_adapter, llm_config, llm_gate, llm_obs, prompt_lib};

/// 一次管线调用的输入。
#[derive(Debug, Clone)]
pub struct PipelineRequest {
    /// 模板名（check_world / entity_extract / sandbox_step / …）。
    pub template: String,
    /// 模板变量。
    pub vars: Map<String, Value>,
    /// 探针指纹（缓存 key 用）。
    pub probe_hash: String,
    pub temperature: f64,
    pub max_tokens: u32,
    /// 强制跳过缓存。
    pub bypass_cache: bool,
    /// 只用 mock 后端（测试/预览）。
    pub use_mock: bool,
}

impl Default for PipelineRequest {
    fn default() -> Self {
        PipelineRequest {
            template: "check_world".into(),
            vars: Map::new(),
            probe_hash: String::new(),
            temperature: 0.0,
            // 0 = 用 config.max_resp_tokens（避免硬编码覆盖配置；reasoning 模型需更大配额）
            max_tokens: 0,
            bypass_cache: false,
            use_mock: false,
        }
    }
}

/// 管线结局。
#[derive(Debug, Clone)]
pub enum Outcome {
    /// 三重闸门未开。
    GateLocked(Value),
    /// 熔断打开。
    BreakerOpen,
    /// 后端不可用（默认构建）。
    Unavailable(String),
    /// 缓存命中。
    CacheHit { content: Value, key: String },
    /// 成功（含护栏校验结果）。
    Success { content: Value, key: String, guards: Value, token_in: i64, token_out: i64, backend: String, model: String },
    /// 护栏拒绝（schema/JSON/白名单）。
    GuardRejected { reason: String, guards: Value },
    /// 模型输出经重试后仍非合法 JSON（标记 malformed；**未写缓存、未记账为成功**）。
    Malformed { attempts: u32, last_raw_head: String },
    /// 后端错误。
    BackendError { kind: String, message: String },
}

impl Outcome {
    pub fn to_json(&self) -> Value {
        match self {
            Outcome::GateLocked(st) => json!({"status": "GATE_LOCKED", "gate": st}),
            Outcome::BreakerOpen => json!({"status": "BREAKER_OPEN", "note": "连续失败触发熔断；退化为静态模式，稍后自动恢复"}),
            Outcome::Unavailable(m) => json!({"status": "UNAVAILABLE", "note": m}),
            Outcome::CacheHit { content, key } => json!({"status": "CACHE_HIT", "content": content, "cache_key": key, "cache_hit": true}),
            Outcome::Success { content, key, guards, token_in, token_out, backend, model } =>
                json!({"status": "OK", "content": content, "cache_key": key, "guards": guards,
                       "token_in": token_in, "token_out": token_out, "backend": backend, "model": model, "cache_hit": false}),
            Outcome::GuardRejected { reason, guards } => json!({"status": "GUARD_REJECTED", "reason": reason, "guards": guards}),
            Outcome::Malformed { attempts, last_raw_head } =>
                json!({"status": "MALFORMED", "attempts": attempts, "last_raw_head": last_raw_head,
                       "note": "模型输出非法 JSON；已重试修复仍失败，标记 malformed（未写缓存）"}),
            Outcome::BackendError { kind, message } => json!({"status": "BACKEND_ERROR", "kind": kind, "message": message}),
        }
    }
}

/// 只渲染 prompt（不发请求、不写缓存）；供 `--dry-run` / `--preview-prompt`。
pub fn preview(work_root: &Path, cfg: &llm_config::LlmConfig, req: &PipelineRequest) -> Result<Value> {
    let tpl = prompt_lib::load(work_root, &req.template)?;
    let system = tpl.system.clone();
    let user = prompt_lib::render(&tpl.user, &req.vars);
    let ep = llm_config::active_endpoint(cfg);
    let key = inference_cache::cache_key(&cfg.default_backend, &ep.model_id, &system, &req.probe_hash, req.temperature);
    Ok(json!({
        "command": "world-gov llm preview",
        "template": req.template,
        "system": system,
        "user": user,
        "cache_key": key,
        "backend": cfg.default_backend,
        "model": ep.model_id,
        "schema_present": tpl.schema != Value::Null,
        "note": "只渲染不发送；--dry-run 语义。",
    }))
}

/// 直接给定 system/user 的调用输入（**统一收口的第二入口**）。
///
/// 供 `inspect-card` / `inspect-batch` 等「题面由 `prompt_builder` 组装、不经
/// `prompt_lib` 模板」的通路复用——它们与模板路径**共享同一闸门/熔断/缓存/后端/
/// 护栏/记账/修复回合**，避免维护两套 LLM 调用链路（第 17.3 批迁移）。
#[derive(Debug, Clone)]
pub struct RawPrompt {
    pub system: String,
    pub user: String,
    /// 缓存 key 的稳定锚（通常为探针指纹）。
    pub probe_hash: String,
    /// 期望 schema（`Value::Null` = 不校验；inspect 通路用宽容判定）。
    pub schema: Value,
    /// 白名单校验的实体字段（空 = 跳过白名单）。
    pub entity_fields: Vec<String>,
    pub temperature: f64,
    /// 0 = 用 `cfg.max_resp_tokens`。
    pub max_tokens: u32,
    pub bypass_cache: bool,
    pub use_mock: bool,
}

impl Default for RawPrompt {
    fn default() -> Self {
        RawPrompt {
            system: String::new(),
            user: String::new(),
            probe_hash: String::new(),
            schema: Value::Null,
            entity_fields: Vec::new(),
            temperature: 0.0,
            max_tokens: 0,
            bypass_cache: false,
            use_mock: false,
        }
    }
}

/// 执行一次管线调用（模板路径；**薄包装**，委托 [`call_raw`]）。
///
/// 保留此签名以向后兼容既有调用方；收口逻辑全部下沉到 `call_raw`。
pub fn call(conn: &rusqlite::Connection, work_root: &Path, cfg: &llm_config::LlmConfig, cli_real_semantic: bool, req: &PipelineRequest) -> Result<Outcome> {
    let tpl = prompt_lib::load(work_root, &req.template)?;
    let user = prompt_lib::render(&tpl.user, &req.vars);
    let raw = RawPrompt {
        system: tpl.system.clone(),
        user,
        probe_hash: req.probe_hash.clone(),
        schema: tpl.schema.clone(),
        entity_fields: tpl.entity_fields.clone(),
        temperature: req.temperature,
        max_tokens: req.max_tokens,
        bypass_cache: req.bypass_cache,
        use_mock: req.use_mock,
    };
    call_raw(conn, work_root, cfg, cli_real_semantic, &raw)
}

/// 执行一次管线调用（**唯一真实外呼入口**；模板路径与 inspect 通路共用）。
pub fn call_raw(
    conn: &rusqlite::Connection,
    work_root: &Path,
    cfg: &llm_config::LlmConfig,
    cli_real_semantic: bool,
    raw: &RawPrompt,
) -> Result<Outcome> {
    // ① 三重闸门（mock 后端豁免：测试/预览不联网）
    if !raw.use_mock && !llm_gate::real_llm_unlocked(cli_real_semantic) {
        return Ok(Outcome::GateLocked(llm_gate::lock_status(cli_real_semantic)));
    }
    // ② 熔断（仅真实后端）
    if !raw.use_mock && llm_obs::breaker_tripped(conn).unwrap_or(false) {
        return Ok(Outcome::BreakerOpen);
    }
    let ep = llm_config::active_endpoint(cfg);
    let backend_name = if raw.use_mock { "dry_run_mock".to_string() } else { llm_adapter::make_backend(&cfg.default_backend, cfg).name().to_string() };
    let model = if raw.use_mock { "mock".to_string() } else { ep.model_id.clone() };
    // 缓存 key：与模板路径同式（system + probe_hash 天然区分题面）。
    let key = inference_cache::cache_key(&backend_name, &model, &raw.system, &raw.probe_hash, raw.temperature);

    // ③ 缓存命中（仅真实后端启用缓存；mock 不缓存）
    if !raw.use_mock && !raw.bypass_cache && cfg.inference_cache_enable {
        if let Some(hit) = inference_cache::get(work_root, &key)? {
            let content = prompt_lib::extract_json(&hit.output).unwrap_or(json!({}));
            let _ = llm_obs::record(conn, &llm_obs::CallRecord {
                backend: backend_name, model, token_in: hit.token_in, token_out: hit.token_out,
                elapsed_ms: 0, cache_hit: true, ok: true, note: "cache_hit".into(),
            }, cfg.breaker_threshold, 60);
            return Ok(Outcome::CacheHit { content, key });
        }
    }

    // ④ 后端调用
    let backend = if raw.use_mock { llm_adapter::make_backend("mock", cfg) } else { llm_adapter::make_backend(&cfg.default_backend, cfg) };
    let constraint = llm_adapter::Constraint {
        max_tokens: if raw.max_tokens > 0 { raw.max_tokens } else { cfg.max_resp_tokens },
        temperature: raw.temperature,
        schema: raw.schema.clone(),
    };
    let start = std::time::Instant::now();
    let mut res = backend.chat(&raw.system, &[llm_adapter::Msg::user(raw.user.clone())], &constraint);
    let mut elapsed = start.elapsed().as_millis() as i64;
    // ④b 修复回合（spec §3：输出非法 → 有限次重试后标记 malformed）。
    // 触发条件：真实后端返回 Ok 但 content 非对象（模型吐了散文/markdown）。
    let max_retry = cfg.max_retry;
    let mut attempts: u32 = 1;
    while let Ok(o) = &res {
        if !o.content.is_null() || o.raw.trim().is_empty() || raw.use_mock {
            break;
        }
        if attempts >= max_retry.saturating_add(1) as u32 || max_retry == 0 {
            break;
        }
        let repairs = llm_adapter::repair_messages(&o.raw, &raw.schema);
        let r2 = backend.chat(&raw.system, &repairs, &constraint);
        elapsed = start.elapsed().as_millis() as i64;
        res = r2;
        attempts += 1;
    }

    let out = match res {
        Err(llm_adapter::BackendError::Unavailable(m)) => {
            let _ = llm_obs::record(conn, &llm_obs::CallRecord { backend: backend_name, model, token_in: 0, token_out: 0, elapsed_ms: elapsed, cache_hit: false, ok: false, note: "unavailable".into() }, cfg.breaker_threshold, 60);
            return Ok(Outcome::Unavailable(m));
        }
        Err(e) => {
            let kind = match e { llm_adapter::BackendError::NotConfigured(_) => "not_configured", llm_adapter::BackendError::Protocol(_) => "protocol", llm_adapter::BackendError::Transport(_) => "transport", _ => "other" };
            let _ = llm_obs::record(conn, &llm_obs::CallRecord { backend: backend_name, model, token_in: 0, token_out: 0, elapsed_ms: elapsed, cache_hit: false, ok: false, note: kind.into() }, cfg.breaker_threshold, 60);
            return Ok(Outcome::BackendError { kind: kind.into(), message: e.to_string() });
        }
        Ok(o) => o,
    };

    // ④c 重试后仍非合法 JSON → 标记 malformed（不写缓存、不算成功）。
    if out.content.is_null() {
        let head: String = out.raw.chars().take(200).collect();
        let _ = llm_obs::record(conn, &llm_obs::CallRecord {
            backend: backend_name, model, token_in: out.token_in, token_out: out.token_out,
            elapsed_ms: elapsed, cache_hit: false, ok: false, note: "malformed".into(),
        }, cfg.breaker_threshold, 60);
        return Ok(Outcome::Malformed { attempts, last_raw_head: head });
    }

    // ⑤ 护栏：schema
    let mut guards = json!({});
    if raw.schema != Value::Null {
        let errs = prompt_lib::validate_schema(&out.content, &raw.schema);
        guards["schema_ok"] = json!(errs.is_empty());
        guards["schema_errors"] = json!(errs);
        if !errs.is_empty() {
            let _ = llm_obs::record(conn, &llm_obs::CallRecord { backend: backend_name, model, token_in: out.token_in, token_out: out.token_out, elapsed_ms: elapsed, cache_hit: false, ok: false, note: "schema_reject".into() }, cfg.breaker_threshold, 60);
            return Ok(Outcome::GuardRejected { reason: "schema".into(), guards });
        }
    }
    // ⑤b 实体白名单（字段为空则跳过）
    if !raw.entity_fields.is_empty() {
        let pool = prompt_lib::entity_pool_from_library(conn);
        let wl = prompt_lib::check_whitelist(&out.content, &raw.entity_fields, &pool);
        guards["whitelist"] = wl.clone();
    }

    // ⑥ 记账 + 写缓存（mock 不写缓存）
    let (_tc, _tf, tripped) = llm_obs::record(conn, &llm_obs::CallRecord {
        backend: backend_name.clone(), model: model.clone(), token_in: out.token_in, token_out: out.token_out,
        elapsed_ms: elapsed, cache_hit: false, ok: true, note: String::new(),
    }, cfg.breaker_threshold, 60)?;
    if tripped {
        guards["breaker_tripped"] = json!(true);
    }
    if !raw.use_mock && cfg.inference_cache_enable {
        let entry = inference_cache::make_entry(key.clone(), &backend_name, &model, out.raw.clone(), out.raw.clone(), out.token_in, out.token_out, vec![]);
        let _ = inference_cache::put(work_root, &entry);
    }
    Ok(Outcome::Success { content: out.content, key, guards, token_in: out.token_in, token_out: out.token_out, backend: backend_name, model })
}

/// 探针指纹（缓存 key 用）——对探针 JSON 取 SHA-256。
pub fn probe_hash(probe: &Value) -> String {
    digest::sha256_hex(probe.to_string().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn setup() -> (Connection, std::path::PathBuf) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE library_index(key TEXT);").unwrap();
        conn.execute("INSERT INTO library_index(key) VALUES('CHAR-99-001')", []).unwrap();
        llm_obs::ensure_table(&conn).unwrap();
        let dir = std::env::temp_dir().join(format!("wg_pipe_{}_{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (conn, dir)
    }

    #[test]
    fn gate_locked_blocks_real_call() {
        let (conn, dir) = setup();
        let cfg = llm_config::LlmConfig::default();
        let req = PipelineRequest { template: "check_world".into(), probe_hash: "ph".into(), ..Default::default() };
        // cli_real_semantic=false → 必 GateLocked
        let o = call(&conn, &dir, &cfg, false, &req).unwrap();
        assert!(matches!(o, Outcome::GateLocked(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mock_bypasses_gate_and_succeeds() {
        let (conn, dir) = setup();
        let cfg = llm_config::LlmConfig::default();
        let mut vars = Map::new();
        vars.insert("card_text".into(), json!("CARD"));
        let req = PipelineRequest { template: "check_world".into(), vars, probe_hash: "ph".into(), use_mock: true, ..Default::default() };
        let o = call(&conn, &dir, &cfg, false, &req).unwrap();
        match o {
            Outcome::Success { content, guards, .. } => {
                assert_eq!(content["verdict"], "OK");  // mock 按 schema enum 取首值
                assert_eq!(guards["schema_ok"], true);
            }
            other => panic!("期望 Success，得到 {:?}", other),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preview_renders_without_sending() {
        let (_conn, dir) = setup();
        let cfg = llm_config::LlmConfig::default();
        let mut vars = Map::new();
        vars.insert("card_text".into(), json!("HELLO"));
        let req = PipelineRequest { template: "check_world".into(), vars, probe_hash: "ph".into(), ..Default::default() };
        let v = preview(&dir, &cfg, &req).unwrap();
        assert!(v["user"].as_str().unwrap().contains("HELLO"));
        assert_eq!(v["cache_key"].as_str().unwrap().len(), 64);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn breaker_and_gate_order_gate_first() {
        let (conn, dir) = setup();
        let cfg = llm_config::LlmConfig::default();
        // 人为触发熔断
        for _ in 0..cfg.breaker_threshold { llm_obs::record(&conn, &llm_obs::CallRecord {
            backend: "x".into(), model: "m".into(), token_in: 0, token_out: 0, elapsed_ms: 0, cache_hit: false, ok: false, note: "".into() }, cfg.breaker_threshold, 60).unwrap(); }
        let req = PipelineRequest { template: "check_world".into(), probe_hash: "ph".into(), ..Default::default() };
        // 默认构建：feature 未开 → 闸门优先拦截（即使已熔断，也比"出网"更安全）
        let o = call(&conn, &dir, &cfg, true, &req).unwrap();
        assert!(matches!(o, Outcome::GateLocked(_)), "闸门优先于熔断");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn breaker_blocks_when_gate_would_be_open() {
        // 直接验证熔断判定（不经闸门）：模拟"若闸门已开"的情形
        let (conn, dir) = setup();
        let cfg = llm_config::LlmConfig::default();
        for _ in 0..cfg.breaker_threshold {
            llm_obs::record(&conn, &llm_obs::CallRecord { backend: "x".into(), model: "m".into(), token_in: 0, token_out: 0, elapsed_ms: 0, cache_hit: false, ok: false, note: "".into() }, cfg.breaker_threshold, 60).unwrap();
        }
        assert!(llm_obs::breaker_tripped(&conn).unwrap(), "熔断已生效");
        assert!(llm_gate::real_llm_unlocked(true) == false, "默认构建闸门仍锁");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn probe_hash_is_sha256() {
        let h = probe_hash(&json!({"k": 1}));
        assert_eq!(h.len(), 64);
        assert_ne!(h, probe_hash(&json!({"k": 2})));
    }
}