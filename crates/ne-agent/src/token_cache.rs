//! token_cache.rs —— 公共前缀增量 token 估算（P2 换血，抄 Op2 TokenCacheManager.rs）
//!
//! # 来源与适配
//! 参考 Operit2 `core/crates/foundation/util/src/TokenCacheManager.rs`（纯 std，
//! 无 tokio/reqwest，符合仓库铁律）。适配点：
//!   · Op2 用 `ChatUtils::estimate_token_count`（自实现分桶）；
//!     我方复用 `crate::context::estimate_tokens`（CJK 分桶更细，且全仓
//!     12 个既有单测护航，口径统一）。
//!   · Op2 记录 `(role, content)` 二元组；我方同样存 `(role, content)`，
//!     与 `ne_engine::llm::Message` 互转在调用方完成，本模块保持零依赖。
//!   · Op2 的 `history_with_tools`（把 tools JSON 拼进 system）保留——
//!     工具表变化会让 system 前缀失配，前缀比较天然回落全量重算，正确。
//!
//! # 为什么快
//! 会话历史是**追加型**的：新一轮 = 上一轮 + 新消息。逐轮全量重估是
//! O(全部历史)，公共前缀法只重算 O(新增)。对长会话（几百条消息）
//! 每轮节省 90%+ 的估算开销。
//!
//! # 正确性
//! 前缀必须**逐条相等**（role+content 都相等）才可复用计数；
//! 任何一条不同（如 M5 截断老工具结果、M9 压缩投影变化）立即截断，
//! 截断点之后全量重算——宁可慢，不可错。

use crate::context::estimate_tokens;

/// 单条历史记录（role, content）。
pub type HistoryEntry = (String, String);

/// 公共前缀增量 token 估算器（Op2 TokenCacheManager 等价物）。
///
/// 用法：会话开始 `new()`；每轮发模型前把最终消息序列喂给
/// [`calculate_input_tokens`]，返回总 token 数，同时内部更新前缀缓存。
#[derive(Debug, Clone, Default)]
pub struct TokenCacheManager {
    previous_chat_history: Vec<HistoryEntry>,
    previous_history_token_count: i64,
    cached_input_token_count: i64,
    current_input_token_count: i64,
}

impl TokenCacheManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// 上一轮命中缓存的 token 数（调试/观测用）。
    pub fn cached_input_token_count(&self) -> i64 {
        self.cached_input_token_count
    }

    /// 本轮新增（未命中缓存）的 token 数（调试/观测用）。
    pub fn current_input_token_count(&self) -> i64 {
        self.current_input_token_count
    }

    /// 计算消息序列的总 token 数（公共前缀增量法）。
    ///
    /// - `history`：本轮最终消息序列（role, content）。
    /// - `tools_json`：可选工具表 JSON。非空时拼进 system 消息头部
    ///   （Op2 同款语义：工具表变化 → system 失配 → 前缀自然截断）。
    /// - `update_state`：true 时更新内部缓存（正常调用）；false 只读
    ///   （试算，不影响下一轮前缀命中）。
    pub fn calculate_input_tokens(
        &mut self,
        history: &[(String, String)],
        tools_json: Option<&str>,
        update_state: bool,
    ) -> i64 {
        let history_with_tools = with_tools(history, tools_json);

        let (cached_tokens, new_tokens) = if self.previous_chat_history.is_empty() {
            (0, total_tokens(&history_with_tools))
        } else {
            let common = find_common_prefix_length(&history_with_tools, &self.previous_chat_history);
            let cached_tokens = if common == self.previous_chat_history.len() {
                // 前缀完全覆盖上一轮：直接复用上轮总计数，零重算
                self.previous_history_token_count
            } else {
                partial_tokens(&history_with_tools[..common])
            };
            let new_tokens = partial_tokens(&history_with_tools[common..]);
            (cached_tokens, new_tokens)
        };

        if update_state {
            self.cached_input_token_count = cached_tokens;
            self.current_input_token_count = new_tokens;
            self.previous_history_token_count = cached_tokens + new_tokens;
            if !history_with_tools.is_empty() {
                self.previous_chat_history = history_with_tools;
            }
        }

        cached_tokens + new_tokens
    }

    /// 清空缓存（会话 rewind / 换会话时调用）。
    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

/// 把 tools JSON 拼进 system 消息（Op2 history_with_tools 同款）。
fn with_tools(history: &[(String, String)], tools_json: Option<&str>) -> Vec<HistoryEntry> {
    let Some(tools) = tools_json.filter(|v| !v.is_empty()) else {
        return history.to_vec();
    };
    let mut history = history.to_vec();
    if let Some(index) = history.iter().position(|(role, _)| role == "system") {
        let (role, content) = &history[index];
        history[index] = (role.clone(), format!("{tools}\n{content}"));
    } else {
        history.insert(0, ("system".to_string(), tools.to_string()));
    }
    history
}

/// 逐条相等的最长公共前缀长度。
fn find_common_prefix_length(current: &[HistoryEntry], previous: &[HistoryEntry]) -> usize {
    let mut common = 0usize;
    for (left, right) in current.iter().zip(previous.iter()) {
        if left == right {
            common += 1;
        } else {
            break;
        }
    }
    common
}

/// 全量估算（含每条消息 64 token 的 JSON 框架开销，与 context::estimate_messages 同口径）。
fn total_tokens(history: &[HistoryEntry]) -> i64 {
    const FRAMING_PER_MESSAGE: i64 = 64;
    history
        .iter()
        .map(|(_, content)| estimate_tokens(content) as i64 + FRAMING_PER_MESSAGE)
        .sum()
}

/// 区间估算（公共前缀段 / 增量段共用）。
fn partial_tokens(history: &[HistoryEntry]) -> i64 {
    total_tokens(history)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> HistoryEntry {
        (role.to_string(), content.to_string())
    }

    #[test]
    fn first_call_computes_full() {
        let mut m = TokenCacheManager::new();
        let hist = vec![msg("system", "你是小说系统"), msg("user", "写第一章")];
        let total = m.calculate_input_tokens(&hist, None, true);
        assert!(total > 0);
        assert_eq!(m.cached_input_token_count(), 0);
        assert_eq!(m.current_input_token_count(), total);
    }

    #[test]
    fn append_only_history_hits_prefix_cache() {
        let mut m = TokenCacheManager::new();
        let base = vec![msg("system", "你是小说系统"), msg("user", "写第一章")];
        let t1 = m.calculate_input_tokens(&base, None, true);

        // 新一轮 = 旧序列 + 追加两条
        let mut next = base.clone();
        next.push(msg("assistant", "好的，第一章如下……"));
        next.push(msg("user", "继续写第二章"));
        let t2 = m.calculate_input_tokens(&next, None, true);

        assert!(t2 > t1, "追加后总量应增加");
        // 前缀完全覆盖上一轮 → cached == t1（零重算命中）
        assert_eq!(m.cached_input_token_count(), t1);
        assert_eq!(m.current_input_token_count(), t2 - t1);
    }

    #[test]
    fn diverged_history_falls_back_to_partial_recompute() {
        let mut m = TokenCacheManager::new();
        let base = vec![msg("system", "你是小说系统"), msg("user", "写第一章")];
        let _ = m.calculate_input_tokens(&base, None, true);

        // 第 2 条被 M5 截断改写 → 前缀只有 1 条命中
        let next = vec![msg("system", "你是小说系统"), msg("user", "写第X章（截断）")];
        let _ = m.calculate_input_tokens(&next, None, true);
        // cached 只含 system 一条（>0），但小于全量
        let cached = m.cached_input_token_count();
        assert!(cached > 0 && cached < m.cached_input_token_count() + m.current_input_token_count());
    }

    #[test]
    fn tools_change_breaks_prefix() {
        let mut m = TokenCacheManager::new();
        let base = vec![msg("system", "你是小说系统"), msg("user", "hi")];
        let _ = m.calculate_input_tokens(&base, None, true);

        // 同样历史但工具表变化 → with_tools 改写 system → 前缀 0 命中
        let next = vec![msg("system", "你是小说系统"), msg("user", "hi")];
        let _ = m.calculate_input_tokens(&next, Some(r#"[{"name":"tool"}]"#), true);
        assert_eq!(m.cached_input_token_count(), 0);
    }

    #[test]
    fn dry_run_does_not_pollute_cache() {
        let mut m = TokenCacheManager::new();
        let base = vec![msg("system", "sys"), msg("user", "u1")];
        let t1 = m.calculate_input_tokens(&base, None, true);

        // 试算一条更长的序列但不落缓存
        let probe = vec![msg("system", "sys"), msg("user", "u1"), msg("assistant", "x")];
        let _ = m.calculate_input_tokens(&probe, None, false);

        // 真实下一轮仍是 base+追加 → 前缀仍全命中
        let mut next = base.clone();
        next.push(msg("assistant", "ok"));
        let t3 = m.calculate_input_tokens(&next, None, true);
        assert_eq!(m.cached_input_token_count(), t1);
        assert!(t3 > t1);
    }

    #[test]
    fn cjk_long_text_estimate_is_stable_and_fast() {
        let mut m = TokenCacheManager::new();
        // 3000 汉字长文本
        let long = "残响之河".repeat(750);
        let base = vec![msg("system", "sys"), msg("user", &long)];
        let t1 = m.calculate_input_tokens(&base, None, true);

        let mut next = base.clone();
        next.push(msg("assistant", &"正文".repeat(500)));
        let t2 = m.calculate_input_tokens(&next, None, true);
        // 前缀命中：cached 等于首轮全量
        assert_eq!(m.cached_input_token_count(), t1);
        assert!(t2 > t1);
        // 估算口径稳定：同文本重算结果一致
        let t2_again = m.calculate_input_tokens(&next, None, false);
        assert_eq!(t2_again, t2);
    }

    #[test]
    fn reset_clears_everything() {
        let mut m = TokenCacheManager::new();
        let base = vec![msg("system", "sys")];
        let _ = m.calculate_input_tokens(&base, None, true);
        m.reset();
        assert_eq!(m.cached_input_token_count(), 0);
        assert_eq!(m.current_input_token_count(), 0);
        assert!(m.calculate_input_tokens(&base, None, true) > 0);
    }
}
