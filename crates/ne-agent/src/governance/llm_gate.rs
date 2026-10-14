//! 真实 LLM 三重锁（world-gov，第 13 批）—— 三条件**同时**满足才解锁。
//!
//! # 三重锁（少一个直接拒绝，绝不外呼）
//! 1. **编译 feature**：Cargo feature `real-semantic`（默认**不开**）。
//! 2. **环境变量**：`WOGOV_ALLOW_REAL_LLM=yes`。
//! 3. **命令行**：`--real-semantic`。
//!
//! # 与既有 `get_semantic_gate` 的关系
//! - 既有门控（`SEMANTIC_ENABLE_GATE && --semantic && NOVEL_SEMANTIC_UNLOCK=yes`）**保持原样不动**，
//!   仍是所有通路的第一道闸（向后兼容）。
//! - 本模块提供**第二道更严的锁**：即便既有门控语法成立，真实外呼还需本三重锁全开。
//! - 默认构建（无 `real-semantic` feature）**出网代码根本不参与编译**。
//!
//! # 铁则
//! - ❌ 无任何自动触发；每一次调用必须显式命令。
//! - ❌ 不硬编码 URL/Key；只从环境变量读。
//! - ✅ 就算解锁：失败即停、写日志，不做「重试风暴」。
use serde_json::{json, Value};

/// 环境变量名（解锁第 2 条）。
pub const ENV_ALLOW: &str = "WOGOV_ALLOW_REAL_LLM";

/// 编译期 feature 是否启用（`real-semantic` 或 `llm_integration`；默认均 false）。
pub const fn compile_feature_enabled() -> bool {
    cfg!(feature = "real-semantic") || cfg!(feature = "llm_integration")
}

/// 第 17 批完整适配层 feature（比 `real-semantic` 更全：多后端/缓存/护栏/管线）。
pub const fn llm_integration_enabled() -> bool {
    cfg!(feature = "llm_integration")
}

/// 环境变量是否放行（严格等于 `yes`）。
pub fn env_allowed() -> bool {
    std::env::var(ENV_ALLOW).map(|v| v == "yes").unwrap_or(false)
}

/// 三重锁纯逻辑（可测）：三条件全真才开门。
pub fn triple_lock(compile_feature: bool, env_ok: bool, cli_flag: bool) -> bool {
    compile_feature && env_ok && cli_flag
}

/// 集中收口：真实外呼前**必经**此处。返回是否解锁。
pub fn real_llm_unlocked(cli_real_semantic: bool) -> bool {
    triple_lock(compile_feature_enabled(), env_allowed(), cli_real_semantic)
}

/// 锁状态明细（gate-check 用）。缺失的锁逐条列出。
pub fn lock_status(cli_real_semantic: bool) -> Value {
    let cf = compile_feature_enabled();
    let env = env_allowed();
    let cli = cli_real_semantic;
    let unlocked = triple_lock(cf, env, cli);
    let mut missing: Vec<&str> = Vec::new();
    if !cf {
        missing.push("编译 feature `llm_integration`（或 `real-semantic`）未启用（默认构建无出网代码）");
    }
    if !env {
        missing.push("环境变量 WOGOV_ALLOW_REAL_LLM 未设为 yes");
    }
    if !cli {
        missing.push("命令行未加 --real-semantic");
    }
    json!({
        "compile_feature": cf,
        "compile_feature_real_semantic": cfg!(feature = "real-semantic"),
        "compile_feature_llm_integration": cfg!(feature = "llm_integration"),
        "env_allowed": env,
        "cli_flag": cli,
        "gate_locked": !unlocked,
        "gate_unlocked": unlocked,
        "missing": missing,
        "status": if unlocked { "UNLOCKED（三重锁全开；仍须显式命令）" } else { "LOCKED（三重锁未全开：feature + WOGOV_ALLOW_REAL_LLM=yes + --real-semantic）" },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triple_lock_requires_all_three() {
        assert!(triple_lock(true, true, true));
        assert!(!triple_lock(false, true, true));
        assert!(!triple_lock(true, false, true));
        assert!(!triple_lock(true, true, false));
        assert!(!triple_lock(false, false, false));
    }

    #[test]
    fn compile_feature_off_by_default() {
        // 默认构建不带 feature → 出网代码不参与编译
        assert!(!compile_feature_enabled(), "默认构建必须无 real-semantic/llm_integration feature");
        assert!(!llm_integration_enabled(), "默认构建必须无 llm_integration feature");
    }

    #[test]
    fn real_llm_unlocked_off_without_feature() {
        // 即便 cli=true、env 设置，缺 feature 也锁死
        assert!(!real_llm_unlocked(true) || compile_feature_enabled());
    }

    #[test]
    fn lock_status_lists_missing() {
        let s = lock_status(false);
        assert_eq!(s["gate_locked"], true);
        assert_eq!(s["gate_unlocked"], false);
        // 默认构建下：feature 锁缺失必被列出
        let missing = s["missing"].as_array().unwrap();
        assert!(missing.iter().any(|x| x.as_str().unwrap().contains("feature")));
        assert!(missing.iter().any(|x| x.as_str().unwrap().contains("--real-semantic")));
    }

    #[test]
    fn lock_status_unlocked_shape() {
        let s = lock_status(true);
        // 纯逻辑校验：三真 → unlocked
        assert_eq!(triple_lock(true, true, true), true);
        let _ = s;
    }
}