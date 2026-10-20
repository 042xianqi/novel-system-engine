//! 上下文窗口策略（M5 截断/折叠 + M9 自动压缩）—— 从 `harness.rs` 拆出（架构门禁 ≤700 行）。
//!
//! 这里只做"把一份消息序列压到窗口内"的**无状态**处理，加上一个自动压缩触发判定。
//! 主循环（`harness.rs`）在每轮组装时调用 [`truncate_context`]，在会话开始时调用
//! [`maybe_auto_compact`]。拆开的好处：窗口策略可独立测试，且不把主循环撑爆。

use crate::config::ContextSection;
use crate::context;
use crate::session::SessionStore;
use crate::token_cache::TokenCacheManager;

/// 进程级 token 前缀缓存单例（P2 换血）。
///
/// # 为什么是单例而不是挂在 HarnessCtx 上
/// `HarnessCtx` 有 5 处构造点（task.rs×2 / subagent_runner / dual_agent×2），
/// 挂字段要同步改 5 处且 `&'a` 生命周期复杂；而前缀算法对**失配天然安全**
/// ——不同会话/不同 system 的序列前缀必然逐条失配，自动回落全量重算，
/// 不会算错，只会少省。所以进程级单例即可，正确性由算法保证。
///
/// # 为什么 Mutex 而不是 AtomicU64
/// 缓存状态是复合结构（前缀 Vec + 计数），必须互斥；争用极低
/// （单线程网关 + 每轮一次），`unwrap_or_else(into_inner)` 防中毒。
fn token_cache_singleton() -> &'static std::sync::Mutex<TokenCacheManager> {
    use std::sync::OnceLock;
    static C: OnceLock<std::sync::Mutex<TokenCacheManager>> = OnceLock::new();
    C.get_or_init(|| std::sync::Mutex::new(TokenCacheManager::new()))
}

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
            tool_calls: m.tool_calls.clone(),
            tool_call_id: m.tool_call_id.clone(),
        })
        .collect();

    // 1) 老轮次工具结果截断
    let (truncated, changed) =
        context::truncate_stale_tool_results(&hist, cfg.keep_recent_tool_results);
    hist = truncated;
    let _ = changed;

    // 2) 预算折叠：先估总 token，超折叠线才丢最早整轮
    //    【P2 换血】全量逐条重算 → TokenCacheManager 公共前缀增量估算：
    //    会话历史是追加型的，前缀命中时零重算；失配（截断/压缩改写历史）
    //    时算法自动回落全量重算，正确性由逐条相等比较保证。
    let limit = context::folding_limit(
        cfg.max_prompt_tokens,
        cfg.reserve_tokens,
        context::TOOL_SCHEMA_RESERVE_TOKENS,
    );
    let est: usize = {
        let entries: Vec<(String, String)> = hist
            .iter()
            .map(|m| (m.role.clone(), m.content.clone()))
            .collect();
        let mut cache = token_cache_singleton().lock().unwrap_or_else(|e| e.into_inner());
        cache.calculate_input_tokens(&entries, None, true) as usize
    };
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
        .map(|m| ne_engine::llm::Message {
            role: m.role,
            content: m.content,
            tool_calls: m.tool_calls,
            tool_call_id: m.tool_call_id,
        })
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
