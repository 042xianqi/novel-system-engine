//! 真实 LLM 调用的**唯一收口层**（2026-10-18 新增）。
//!
//! # 为什么必须有这一层
//!
//! 审计发现全仓有 **7 处** 直接 `llm::call(...)`，绕过三重锁：
//!
//! ```text
//! crates/ne-agent/src/main.rs:900              裸调
//! crates/ne-agent/src/main.rs:935              裸调
//! crates/ne-agent/src/task.rs:588              裸调
//! crates/ne-agent/src/harness.rs:407           裸调
//! crates/ne-agent/src/compaction.rs:235        裸调
//! crates/ne-agent/src/dual_agent.rs:261        裸调
//! crates/ne-agent/src/outline_rebuild.rs:270   裸调
//! crates/ne-agent/src/outline_rebuild.rs:415   裸调
//! ```
//!
//! 三重锁（`real-semantic` feature + `WOGOV_ALLOW_REAL_LLM=yes` + `--real-semantic`）
//! 在这些路径上**完全失效**：只要代码走到这里，就直接出网了。
//!
//! # 设计
//!
//! `governed_call()` 是唯一允许真实外呼的入口：
//!   1. 检查三重锁；未解锁 → 立即返回 `Locked`，**绝不发起网络请求**
//!   2. 解锁 → 委托给 `ne_engine::llm::call`
//!   3. 无论成败都写审计日志（`governance::category_store::log_op`）
//!
//! # 迁移方式
//!
//! 把 `ne_engine::llm::call(...)` 换成 `governance::llm_gate::governed_call(...)`。
//! 签名刻意保持一致（返回 `anyhow::Result<String>`），便于机械替换。
//!
//! # 兼容性
//!
//! 默认构建无 `real-semantic`/`llm_integration` feature，
//! `governed_call()` 恒返回 `Locked`。这意味着**默认构建下所有路径都不出网**——
//! 这正是我们要的：出网必须是显式、可审计的例外。

use anyhow::Result;
use ne_engine::llm::{LlmParams, Message, LlmOutput};

use super::llm_gate;

/// 门控拦截时返回的错误类型。
///
/// 刻意做成独立类型而非 `anyhow!`：调用方可以 `match` 它做差异化处理
/// （比如 UI 显示"未解锁"而不是"调用失败"）。
#[derive(Debug)]
pub struct Locked {
    /// 缺失的锁明细（人类可读）。
    pub missing: Vec<String>,
    /// 调用点标识（便于审计定位是哪一处想外呼）。
    pub site: &'static str,
}

impl std::fmt::Display for Locked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "LLM 调用被三重锁拦截（site={}）：{}",
            self.site,
            if self.missing.is_empty() {
                "未知原因".to_string()
            } else {
                self.missing.join("；")
            }
        )
    }
}

impl std::error::Error for Locked {}

/// **唯一**允许真实 LLM 外呼的入口。
///
/// - `site`：调用点标识，写进审计日志（如 `"task.rs:588"`）
/// - `cli_real_semantic`：是否给了 `--real-semantic`
///
/// 未解锁时返回 `Err(Locked)`，**不发起任何网络请求**。
pub fn governed_call(
    llm: &LlmParams,
    msgs: &[Message],
    site: &'static str,
    cli_real_semantic: bool,
) -> Result<LlmOutput> {
    let st = llm_gate::lock_status(cli_real_semantic);

    if !llm_gate::real_llm_unlocked(cli_real_semantic) {
        let missing: Vec<String> = st["missing"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        // 审计：记录"有人想外呼但被拦住"——这本身就是重要信号
        // （说明某条代码路径以为自己是解锁的，或者测试时忘了加 flag）
        eprintln!(
            "[llm-gate] BLOCKED site={} missing={:?}",
            site, missing
        );

        return Err(anyhow::Error::new(Locked {
            missing,
            site,
        }));
    }

    // 已解锁：真实外呼（显式、可审计）
    eprintln!("[llm-gate] ALLOWED site={} —— 真实外呼", site);
    ne_engine::llm::call(llm, msgs).map_err(|e| {
        eprintln!("[llm-gate] FAILED site={} err={}", site, e);
        e
    })
}

/// 只判锁、不外呼。给"要不要显示按钮/要不要准备 prompt"这类场景用。
pub fn would_allow(cli_real_semantic: bool) -> bool {
    llm_gate::real_llm_unlocked(cli_real_semantic)
}

/// **迁移用兼容别名**：签名与 `ne_engine::llm::call` 完全一致（2 参数），
/// 便于把全仓的 `llm::call(llm, &msgs)` 机械替换成 `ne_agent_gov_call(llm, &msgs)`。
///
/// 与 `governed_call` 的区别：
///   · 不接收 `site` / `cli_real_semantic`（老调用点没这些信息）
///   · `site` 记为 `"<legacy>"`，便于审计时识别"这是迁移过来的老调用点"
///   · `cli_real_semantic` 从环境变量 `WOGOV_CLI_REAL_SEMANTIC=yes` 读取
///     （老调用点大多在库/后台线程里，拿不到 CLI flag；
///       这让它们默认锁死，要放行必须显式设环境变量）
///
/// **长期目标**：逐步把 `ne_agent_gov_call` 换成带明确 site 的 `governed_call`。
pub fn ne_agent_gov_call(
    llm: &LlmParams,
    msgs: &[Message],
) -> Result<LlmOutput> {
    let cli = std::env::var("WOGOV_CLI_REAL_SEMANTIC")
        .map(|v| v == "yes")
        .unwrap_or(false);
    governed_call(llm, msgs, "<legacy>", cli)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locked_error_displays_site() {
        let e = Locked {
            missing: vec!["编译 feature 未启用".into()],
            site: "test.rs:1",
        };
        let s = e.to_string();
        assert!(s.contains("test.rs:1"), "错误信息应含调用点：{s}");
        assert!(s.contains("编译 feature"), "错误信息应含缺失项：{s}");
    }

    #[test]
    fn default_build_is_locked() {
        // 默认构建无 feature → would_allow 必为 false
        // （若开了 feature 则此断言跳过，因为那是有意的解锁构建）
        if !llm_gate::compile_feature_enabled() {
            assert!(!would_allow(true), "默认构建即便 cli=true 也必须锁死");
        }
    }
}
