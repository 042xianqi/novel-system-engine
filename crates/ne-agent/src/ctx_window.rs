//! 上下文窗口策略（M5 截断/折叠 + M9 自动压缩）—— 从 `harness.rs` 拆出（架构门禁 ≤700 行）。
//!
//! 这里只做"把一份消息序列压到窗口内"的**无状态**处理，加上一个自动压缩触发判定。
//! 主循环（`harness.rs`）在每轮组装时调用 [`truncate_context`]，在会话开始时调用
//! [`maybe_auto_compact`]。拆开的好处：窗口策略可独立测试，且不把主循环撑爆。

use crate::config::ContextSection;
use crate::context;
use crate::session::SessionStore;

/// 应用上下文策略：老轮次工具结果截断 + 按预算折叠最早整轮。
///
/// 顺序（太墟同序）：
///   1. **截断老工具结果**（保护当前轮 + 最近 N 条）—— 不改条数，只压正文
///   2. **按折叠线丢最早整轮**（只在仍超限时）—— 保 tool_call⇄result 配对，至少留一轮
///
/// system 消息（首条）永不动。
pub(crate) fn truncate_context(
    cfg: &ContextSection,
    msgs: Vec<ne_engine::llm::Message>,
) -> Vec<ne_engine::llm::Message> {
    if msgs.is_empty() {
        return msgs;
    }
    // 转成可截断视图（system 标为非工具结果、不可删）
    let mut hist: Vec<context::HistMsg> = msgs
        .iter()
        .map(|m| context::HistMsg {
            role: m.role.clone(),
            content: m.content.clone(),
            is_tool_result: m.role == "tool",
        })
        .collect();

    // 1) 老轮次工具结果截断
    let (truncated, changed) =
        context::truncate_stale_tool_results(&hist, cfg.keep_recent_tool_results);
    hist = truncated;
    let _ = changed;

    // 2) 预算折叠：先估总 token，超折叠线才丢最早整轮
    let limit = context::folding_limit(
        cfg.max_prompt_tokens,
        cfg.reserve_tokens,
        context::TOOL_SCHEMA_RESERVE_TOKENS,
    );
    let est: usize = hist.iter().map(|m| context::estimate_tokens(&m.content) + 64).sum();
    if est > limit && hist.len() > 1 {
        // 首条是 system，永不折叠 → 从第 2 条起找轮边界
        let body = &hist[1..];
        let start = context::keep_from_index(
            body,
            limit.saturating_sub(context::estimate_tokens(&hist[0].content) + 64),
        );
        let mut kept = vec![hist[0].clone()];
        kept.extend(body[start..].iter().cloned());
        hist = kept;
    }

    hist.into_iter()
        .map(|m| ne_engine::llm::Message { role: m.role, content: m.content })
        .collect()
}

/// 自动压缩（M9）：若开启且投影 token 估算超过阈值，就把早期历史折叠成摘要。
///
/// **只做一次判定、不做循环补偿**：压缩后若仍超限，交给 [`truncate_context`] 的机械折叠
/// 兜底——机械折叠是无状态的、必然收敛，自动压缩只负责"把有意义的早期历史换成摘要"。
///
/// 失败一律**静默降级**：自动压缩是优化，绝不能因为它出问题而中断对话
/// （用户可手动 `compress` 查看结果）。
pub(crate) fn maybe_auto_compact(
    cfg: &ContextSection,
    store: &SessionStore,
    session: &str,
    llm: &ne_engine::llm::LlmParams,
) {
    if !cfg.auto_compaction {
        return;
    }
    let limit = context::folding_limit(
        cfg.max_prompt_tokens,
        cfg.reserve_tokens,
        context::TOOL_SCHEMA_RESERVE_TOKENS,
    );
    let threshold = limit.saturating_mul(cfg.auto_threshold_percent.clamp(1, 100) as usize) / 100;
    let Ok(proj) = crate::compaction::project(store, session) else {
        return;
    };
    // 已压缩过且摘要仍在 → 不再重复压缩（避免把摘要反复压进更短的摘要）
    if proj.summary.is_some() || proj.est_tokens < threshold {
        return;
    }
    let summarizer = crate::compaction::LlmSummarizer { llm: llm.clone() };
    let _ = crate::compaction::compact(
        store,
        session,
        crate::compaction::CompressMode::After,
        "",
        Some(&summarizer),
    );
}
