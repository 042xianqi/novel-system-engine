//! 语义推理运行时（world-gov）—— **只定义契约、封装调用壳、占位重试/超时/熔断**。
//!
//! # 红线（铁则）
//! - ❌ 不引入 `reqwest` / 任何 http client；真实网络调用只留注释 `TODO`。
//! - ❌ 不内置任何模型地址、密钥、预置 prompt 全文（prompt 组装留在 probe 产出侧）。
//! - ❌ 不绕过三重门控：gate 未开 → 一律 [`RuntimeOutcome::GateLocked`]，绝不外呼。
//! - ✅ 沙盘替身 [`deterministic_semantic_mock`] 仅用于管线验证，**永不是正式推理路径**。
//! - ✅ 除 `task_jobs` / `risk_index` 外不新增任何表。
//! - 重试/超时只做**状态占位与计数**，不真 sleep 长阻塞（注入 stub 便于单测）。
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

// ────────────────────────────────── 强类型契约（非松散 JSON）

/// 一次语义推理请求。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SemanticRequest {
    pub job_id: String,
    /// 法理基线指纹（world-gov bootstrap 产物），用于校验"同一法理基准下推理"
    pub law_fingerprint: String,
    /// 探针组装好的提示词文本（本层不生成 prompt，只承载）
    pub prompt_text: String,
    pub max_tokens: u32,
    pub temperature: f32,
}

/// 单条语义发现。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SemanticFinding {
    pub item_id: String,
    pub location: String,
    pub kind: String,
    /// OK | WARN | HIGH-RISK | UNCERTAIN
    pub verdict: String,
    /// 判据：引用的上层法理原文/key（无依据留空）
    pub law_ref: String,
    pub confidence: f32,
    pub note: Option<String>,
}

/// 一次推理响应。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SemanticResponse {
    pub ok: bool,
    pub findings: Vec<SemanticFinding>,
    pub note: String,
    /// 是否含不确定结论（按铁则拿不准一律 UNCERTAIN）
    pub is_uncertain: bool,
}

/// 运行时结局（穷举分支，供管线状态机映射）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RuntimeOutcome {
    Success(SemanticResponse),
    Timeout,
    RetriesExhausted,
    GateLocked,
    ProtocolError(String),
}

impl RuntimeOutcome {
    pub fn tag(&self) -> &'static str {
        match self {
            RuntimeOutcome::Success(_) => "Success",
            RuntimeOutcome::Timeout => "Timeout",
            RuntimeOutcome::RetriesExhausted => "RetriesExhausted",
            RuntimeOutcome::GateLocked => "GateLocked",
            RuntimeOutcome::ProtocolError(_) => "ProtocolError",
        }
    }
}

/// 极简端点配置：**只从环境变量读**，不硬编码、不从资料库读密钥。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticEndpointConfig {
    pub base: Option<String>,
    pub api_key: Option<String>,
    pub max_retry: u8,
    pub timeout_sec: u16,
}

impl SemanticEndpointConfig {
    /// 从 `NOVEL_LLM_ENDPOINT` / `NOVEL_LLM_KEY` 读取；其余为固定占位默认。
    pub fn from_env() -> Self {
        SemanticEndpointConfig {
            base: std::env::var("NOVEL_LLM_ENDPOINT").ok(),
            api_key: std::env::var("NOVEL_LLM_KEY").ok(),
            max_retry: 2,
            timeout_sec: 30,
        }
    }

    /// 出口是否已配置（只报布尔，不泄露内容）。
    pub fn endpoint_configured(&self) -> bool {
        self.base.as_deref().map(|s| !s.is_empty()).unwrap_or(false)
    }

    /// 安全快照（**绝不输出密钥本体**）。
    pub fn safe_snapshot(&self) -> Value {
        json!({
            "base_configured": self.endpoint_configured(),
            "api_key_configured": self.api_key.as_deref().map(|s| !s.is_empty()).unwrap_or(false),
            "max_retry": self.max_retry,
            "timeout_sec": self.timeout_sec,
            "note": "仅报布尔；密钥/地址不回显。真实调用未实现。",
        })
    }
}

// ────────────────────────────────── 重试/超时占位（不真 sleep）

/// 可注入的 sleep stub：真实实现留 TODO，测试用零耗时替身。
/// ```text
/// TODO（放行令后）：替换为 tokio/std::thread::sleep(Duration::from_secs(sec))
/// ```
pub fn sleep_stub(_sec: u16) {
    // 故意为空：不真阻塞。
}

/// 重试壳：按 `max_retry` 尝试，仅对可重试结局（Timeout）继续。
/// 返回 (尝试次数, 最终结局)。**不真 sleep 长阻塞**（仅在重试间调用 stub）。
pub fn run_with_retry<F>(max_retry: u8, mut attempt: F) -> (u8, RuntimeOutcome)
where
    F: FnMut(u8) -> RuntimeOutcome,
{
    let mut tries = 0u8;
    loop {
        tries = tries.saturating_add(1);
        let outcome = attempt(tries - 1);
        match outcome {
            RuntimeOutcome::Success(_) => return (tries, outcome),
            RuntimeOutcome::Timeout => {
                if tries > max_retry {
                    return (tries, RuntimeOutcome::RetriesExhausted);
                }
                sleep_stub(0); // 占位：不真阻塞
            }
            // 其余结局不重试，直接返回
            other => return (tries, other),
        }
    }
}

// ────────────────────────────────── 主入口（门控收口 + 沙盘替身 + 死 TODO）

/// 运行一次语义推理。**所有真实外呼必经此处**，且门控未开绝不外呼。
///
/// - `gate_open`：由调用方经 `governance::get_semantic_gate(cli_semantic)` 求得（集中收口）。
/// - `dry_run`：沙盘回放，绝不外呼。
pub fn run_semantic_infer(
    req: &SemanticRequest,
    cfg: &SemanticEndpointConfig,
    gate_open: bool,
    dry_run: bool,
) -> Result<RuntimeOutcome> {
    // ① 门控未开 → 直接 GateLocked，绝不外呼
    if !gate_open {
        return Ok(RuntimeOutcome::GateLocked);
    }

    // ② 门控开 && 沙盘 → 确定性替身回放（不碰外部）
    if dry_run {
        let seed = seed_from(&req.job_id);
        return Ok(RuntimeOutcome::Success(deterministic_semantic_mock(seed)));
    }

    // ③ 门控开 && 非沙盘 → 真实调用**尚未实现**，留死 TODO
    //
    // TODO（放行令后填充）：此处插入真实 HTTP 调用，示例（**当前禁止引入 reqwest**）：
    //   let body = json!({ "model": "...", "input": req.prompt_text, "max_tokens": req.max_tokens });
    //   POST {cfg.base}/... with Authorization: Bearer {cfg.api_key}
    //   解析 -> SemanticResponse；超时 -> RuntimeOutcome::Timeout；非 2xx -> ProtocolError
    // 注意：不得绕过 get_semantic_gate()；不得硬编码 URL/Key（只用 cfg，来自环境变量）。
    let _ = (req, cfg);
    anyhow::bail!("REAL_LLM_NOT_IMPLEMENTED: 等待放行令填充 reqwest 调用")
}

// ────────────────────────────────── 沙盘替身（可复现）

fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn seed_from(job_id: &str) -> u64 {
    fnv1a(job_id)
}

/// 确定性 LCG（不引 rand 依赖），保证同 seed 同输出。
fn lcg(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *state
}

/// 沙盘替身：给定 seed，**可复现**地输出 2 条模拟 findings（1 OK + 1 UNCERTAIN）。
/// 仅用于管线走通验证，**永不是正式推理路径**；不含任何真实语义结论。
pub fn deterministic_semantic_mock(seed: u64) -> SemanticResponse {
    let mut st = seed ^ 0x9e3779b97f4a7c15;
    let r1 = lcg(&mut st);
    let r2 = lcg(&mut st);
    let tag = format!("{:04x}", (seed & 0xffff));
    let conf1 = 0.3 + (r1 % 40) as f32 / 100.0; // 0.30~0.69
    let conf2 = 0.1 + (r2 % 30) as f32 / 100.0; // 0.10~0.39
    let findings = vec![
        SemanticFinding {
            item_id: format!("SIM-{tag}-A"),
            location: format!("mock/seed#{tag}"),
            kind: "沙盘验收".into(),
            verdict: "OK".into(),
            law_ref: String::new(),
            confidence: conf1,
            note: Some("沙盘替身：无真实语义结论".into()),
        },
        SemanticFinding {
            item_id: format!("SIM-{tag}-B"),
            location: format!("mock/seed#{tag}"),
            kind: "沙盘验收".into(),
            verdict: "UNCERTAIN".into(),
            law_ref: String::new(),
            confidence: conf2,
            note: Some("沙盘替身：拿不准一律 UNCERTAIN（按铁则不入语义风险）".into()),
        },
    ];
    SemanticResponse {
        ok: true,
        findings,
        note: format!("deterministic_semantic_mock(seed={seed})"),
        is_uncertain: true,
    }
}

/// 把强类型 findings 映射为 `emit_semantic_findings` 期望的 JSON 形态。
/// 字段映射：key=item_id, verdict, kind, law_ref, explain=note。
pub fn findings_to_json(findings: &[SemanticFinding]) -> Vec<Value> {
    findings
        .iter()
        .map(|f| {
            json!({
                "key": f.item_id,
                "verdict": f.verdict,
                "kind": f.kind,
                "law_ref": f.law_ref,
                "explain": f.note.clone().unwrap_or_default(),
                "confidence": f.confidence,
                "location": f.location,
            })
        })
        .collect()
}

// ────────────────────────────────── 单测

#[cfg(test)]
mod tests {
    use super::*;

    fn req(job: &str) -> SemanticRequest {
        SemanticRequest {
            job_id: job.into(),
            law_fingerprint: "c82ca31fce088818".into(),
            prompt_text: "（探针侧组装，本层不回显）".into(),
            max_tokens: 1024,
            temperature: 0.2,
        }
    }

    #[test]
    fn serde_roundtrip_request_response() {
        let r = req("job-1");
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(serde_json::from_str::<SemanticRequest>(&s).unwrap(), r);

        let resp = SemanticResponse {
            ok: true,
            findings: vec![SemanticFinding {
                item_id: "A".into(),
                location: "L".into(),
                kind: "k".into(),
                verdict: "WARN".into(),
                law_ref: "GUARD/铁则:禁止X".into(),
                confidence: 0.7,
                note: None,
            }],
            note: "n".into(),
            is_uncertain: false,
        };
        let s2 = serde_json::to_string(&resp).unwrap();
        assert_eq!(serde_json::from_str::<SemanticResponse>(&s2).unwrap(), resp);

        // 枚举各变体也可序列化
        for o in [
            RuntimeOutcome::GateLocked,
            RuntimeOutcome::Timeout,
            RuntimeOutcome::RetriesExhausted,
            RuntimeOutcome::ProtocolError("p".into()),
            RuntimeOutcome::Success(resp.clone()),
        ] {
            let t = serde_json::to_string(&o).unwrap();
            assert_eq!(serde_json::from_str::<RuntimeOutcome>(&t).unwrap(), o);
        }
    }

    #[test]
    fn gate_locked_never_calls_out() {
        // 门控未开：无论 dry_run 与否，必须 GateLocked
        let cfg = SemanticEndpointConfig::from_env();
        let o1 = run_semantic_infer(&req("j"), &cfg, false, false).unwrap();
        assert_eq!(o1, RuntimeOutcome::GateLocked);
        let o2 = run_semantic_infer(&req("j"), &cfg, false, true).unwrap();
        assert_eq!(o2, RuntimeOutcome::GateLocked);
    }

    #[test]
    fn real_call_is_hard_todo() {
        // 门控开 + 非沙盘 → 必须是未实现错误，绝不出网
        let cfg = SemanticEndpointConfig::from_env();
        let e = run_semantic_infer(&req("j"), &cfg, true, false).unwrap_err();
        assert!(e.to_string().contains("REAL_LLM_NOT_IMPLEMENTED"));
    }

    #[test]
    fn mock_is_deterministic_per_seed() {
        let a = deterministic_semantic_mock(42);
        let b = deterministic_semantic_mock(42);
        let c = deterministic_semantic_mock(43);
        assert_eq!(a, b, "同 seed 必须完全复现");
        assert_ne!(a, c, "不同 seed 应不同");
        assert_eq!(a.findings.len(), 2);
        assert_eq!(a.findings[0].verdict, "OK");
        assert_eq!(a.findings[1].verdict, "UNCERTAIN");
        assert!(a.is_uncertain);
    }

    #[test]
    fn dry_run_gate_open_returns_mock() {
        let cfg = SemanticEndpointConfig::from_env();
        let o = run_semantic_infer(&req("fixed-id"), &cfg, true, true).unwrap();
        match o {
            RuntimeOutcome::Success(resp) => {
                assert_eq!(resp.findings.len(), 2);
                assert!(resp.note.contains("deterministic_semantic_mock"));
            }
            other => panic!("expected Success, got {}", other.tag()),
        }
    }

    #[test]
    fn retry_counts_and_exhausts() {
        // 永远 Timeout → 尝试 max_retry+1 次后 RetriesExhausted
        let (n, out) = run_with_retry(2, |_| RuntimeOutcome::Timeout);
        assert_eq!(n, 3);
        assert_eq!(out, RuntimeOutcome::RetriesExhausted);
        // 第二次成功
        let (n2, out2) = run_with_retry(3, |i| {
            if i == 1 { RuntimeOutcome::Success(deterministic_semantic_mock(1)) }
            else { RuntimeOutcome::Timeout }
        });
        assert_eq!(n2, 2);
        assert_eq!(out2.tag(), "Success");
        // 不可重试结局立即返回
        let (n3, out3) = run_with_retry(5, |_| RuntimeOutcome::GateLocked);
        assert_eq!(n3, 1);
        assert_eq!(out3, RuntimeOutcome::GateLocked);
    }

    #[test]
    fn config_reads_only_env_and_hides_secrets() {
        // 不设环境变量时 base 未配置；safe_snapshot 不泄露内容
        let cfg = SemanticEndpointConfig { base: None, api_key: None, max_retry: 1, timeout_sec: 5 };
        let snap = cfg.safe_snapshot();
        assert_eq!(snap["base_configured"], false);
        assert_eq!(snap["api_key_configured"], false);
        // 即便有值，快照也只回布尔
        let cfg2 = SemanticEndpointConfig {
            base: Some("http://example.invalid".into()),
            api_key: Some("sk-secret".into()),
            max_retry: 1, timeout_sec: 5,
        };
        let s2 = cfg2.safe_snapshot().to_string();
        assert!(!s2.contains("sk-secret"), "快照不得泄露密钥");
        assert!(!s2.contains("example.invalid"), "快照不得回显地址");
        assert_eq!(cfg2.safe_snapshot()["base_configured"], true);
    }

    #[test]
    fn findings_to_json_maps_fields() {
        let resp = deterministic_semantic_mock(7);
        let js = findings_to_json(&resp.findings);
        assert_eq!(js.len(), 2);
        assert!(js[0]["key"].as_str().unwrap().starts_with("SIM-"));
        assert_eq!(js[1]["verdict"], "UNCERTAIN");
    }
}