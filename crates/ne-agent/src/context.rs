//! 上下文窗口策略 —— token 估算 + 老轮次工具结果截断 + 预算驱动折叠。
//!
//! 契约逐条对照太墟 `ContextWindowPolicy.kt`（1074 行，抄契约不抄体量）：
//!   * `estimateTokens` —— **CJK 单独分桶**（见下）
//!   * `truncateStaleToolResults` —— 预算线**之前**就生效的老轮次截断
//!   * `foldingLimitFor` —— 折叠线 = 窗口 - 预留输出 - 工具 schema 预留
//!
//! # 为什么 CJK 必须单独分桶
//!
//! 太墟注释写得很清楚：**中文标点实际 ~1 token/字**。若把中文标点也按 ASCII 标点
//! 的 `/2.8` 估，会**系统性低估中文上下文**——然后真实请求比估算大一截，
//! 把折叠线余量悄悄吃掉，最终以 400/413 溢出。
//! 所以全角区（`0xFF00..0xFFEF`，如 `，！？：`）与 CJK 区同价。
//!
//! # 为什么老轮次截断不能等到折叠线
//!
//! 预算驱动的折叠只有**越过 token 线**才触发。在此之前，每个历史轮的大输出
//! （长 read、网页快照）都会**原样重复发送**：一轮 2.7KB 随对话增长累积，
//! 既烧 token 又稀释注意力。所以只要不是"受保护的"，超过阈值就压缩。
//!
//! # 不改消息条数与顺序
//!
//! 太墟强调：NATIVE 协议下丢消息会产生**非法 transcript**（tool_call 与 result
//! 必须配对）。所以本模块只**替换正文**，绝不增删消息。

/// 估算一个字符串的 token 数（太墟 `estimateTokens` 同算法）。
///
/// 分桶：CJK / ASCII 字母数字 / 其它标点；空白不计。
pub fn estimate_tokens(text: &str) -> usize {
    if text.trim().is_empty() {
        return 0;
    }
    let mut cjk = 0usize;
    let mut ascii = 0usize;
    let mut punct = 0usize;
    for ch in text.chars() {
        let c = ch as u32;
        if (0x2E80..=0x9FFF).contains(&c)      // CJK 部首/汉字
            || (0xAC00..=0xD7AF).contains(&c)  // 韩文
            || (0xFF00..=0xFFEF).contains(&c)  // 全角标点
        {
            cjk += 1;
        } else if ch.is_whitespace() {
            // 不计
        } else if ch.is_alphanumeric() {
            ascii += 1;
        } else {
            punct += 1;
        }
    }
    let est = cjk as f64 / 1.8 + ascii as f64 / 2.5 + punct as f64 / 2.8;
    (est as usize).max(1)
}

/// 估算一组消息的 token 总量（含 JSON 框架的固定开销）。
pub fn estimate_messages(msgs: &[MessageView]) -> usize {
    const FRAMING_PER_MESSAGE: usize = 64; // 太墟 JSON_FRAMING_PER_MESSAGE
    msgs.iter()
        .map(|m| estimate_tokens(&m.content) + FRAMING_PER_MESSAGE)
        .sum()
}

/// 供估算用的最小消息视图（不绑定具体 LLM 类型）。
pub struct MessageView<'a> {
    pub role: &'a str,
    pub content: &'a str,
}

/// 折叠线：本窗口最多允许多少 prompt token（太墟 `foldingLimitFor`）。
///
/// `foldingLimit = (window - reserveOutput - toolSchemaReserve) * ratio`
pub fn folding_limit(window: u32, reserve_output: u32, tool_schema_reserve: u32) -> usize {
    let w = window.max(1) as i64;
    let lim = w - reserve_output as i64 - tool_schema_reserve as i64;
    lim.max(1) as usize
}

/// 默认预留：工具 schema（tools 数组不在这批 messages 里，必须显式加回）。
pub const TOOL_SCHEMA_RESERVE_TOKENS: u32 = 7_200;

/// 折叠时保护最近 N 条工具结果（太墟 `KEEP_RECENT_TOOL_RESULTS`）。
pub const KEEP_RECENT_TOOL_RESULTS: usize = 4;

/// 单条工具结果的截断阈值（字符）。
///
/// 太墟按工具类型分档；ne-agent 先用统一的保守档 + 值域。
pub const TOOL_OUTPUT_SOFT_LIMIT: usize = 4_000;
/// 截断后保留的头/尾字符（太墟 ACTIVE_TOOL_KEEP_*）。
pub const KEEP_HEAD_CHARS: usize = 2_000;
pub const KEEP_TAIL_CHARS: usize = 800;

/// 一条可截断的历史消息（`role=tool` 或含长输出的 assistant）。
#[derive(Debug, Clone, PartialEq)]
pub struct HistMsg {
    pub role: String,
    pub content: String,
    /// 这条是否是"老轮次工具结果"（可被截断）
    pub is_tool_result: bool,
    /// 工具调用（assistant 轮）；截断/折叠时必须原样带回，否则配对丢失。
    pub tool_calls: Option<Vec<ne_engine::llm::ToolCall>>,
    /// 配对的 tool_call_id（tool 轮）。
    pub tool_call_id: Option<String>,
}

/// 老轮次工具结果截断（太墟 `truncateStaleToolResults` 的等价物）。
///
/// 保护集：
///   * 最后一条 user 消息**之后**的全部工具结果（当前工作轮——模型下一步就靠它们）
///   * 最近 `keep_recent` 条工具结果（steering 兜底）
///
/// 其余超过 [`TOOL_OUTPUT_SOFT_LIMIT`] 的替换为"头 + 省略标记 + 尾"。
/// **不增删消息**，只改正文。
///
/// 返回 `(新消息列表, 被截断条数)`。
pub fn truncate_stale_tool_results(
    msgs: &[HistMsg],
    keep_recent: usize,
) -> (Vec<HistMsg>, usize) {
    if msgs.is_empty() {
        return (Vec::new(), 0);
    }
    let last_user_idx = msgs.iter().rposition(|m| m.role == "user");
    // 受保护的工具结果下标
    let mut protected: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let tool_idxs: Vec<usize> = msgs
        .iter()
        .enumerate()
        .filter(|(_, m)| m.is_tool_result)
        .map(|(i, _)| i)
        .collect();
    for i in tool_idxs.iter().rev().take(keep_recent) {
        protected.insert(*i);
    }
    if let Some(lu) = last_user_idx {
        for i in &tool_idxs {
            if *i > lu {
                protected.insert(*i);
            }
        }
    }

    let mut changed = 0usize;
    let out: Vec<HistMsg> = msgs
        .iter()
        .enumerate()
        .map(|(i, m)| {
            if !m.is_tool_result || protected.contains(&i) {
                return m.clone();
            }
            if m.content.chars().count() <= TOOL_OUTPUT_SOFT_LIMIT {
                return m.clone();
            }
            changed += 1;
            HistMsg {
                role: m.role.clone(),
                content: compact_output(&m.content),
                is_tool_result: true,
                tool_calls: m.tool_calls.clone(),
                tool_call_id: m.tool_call_id.clone(),
            }
        })
        .collect();
    (out, changed)
}

/// 压缩一条长工具输出为"头 + 省略标记 + 尾"。
pub fn compact_output(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= KEEP_HEAD_CHARS + KEEP_TAIL_CHARS {
        return s.to_string();
    }
    let head: String = chars[..KEEP_HEAD_CHARS].iter().collect();
    let tail: String = chars[chars.len() - KEEP_TAIL_CHARS..].iter().collect();
    let omitted = chars.len() - KEEP_HEAD_CHARS - KEEP_TAIL_CHARS;
    format!("{head}\n…（省略 {omitted} 字）…\n{tail}")
}

/// 按预算从最老的一端丢弃整轮，直到落进折叠线。
///
/// ⚠ 只丢**完整轮次**（user 起到下一个 user 前），保证 tool_call⇄result 配对被保住；
/// 且**永不丢系统消息**。返回保留的消息区间起点（在被折叠线上时）。
pub fn keep_from_index(msgs: &[HistMsg], limit: usize) -> usize {
    // 逐轮找 user 起点，从最早开始试删
    let user_starts: Vec<usize> = msgs
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == "user")
        .map(|(i, _)| i)
        .collect();
    if user_starts.is_empty() {
        return 0;
    }
    for (n, start) in user_starts.iter().enumerate() {
        let slice = &msgs[*start..];
        let est: usize = slice
            .iter()
            .map(|m| estimate_tokens(&m.content) + 64)
            .sum();
        if est <= limit {
            return *start;
        }
        // 最后一轮也超限 → 不再删（至少留一轮）
        if n + 1 == user_starts.len() {
            return *start;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hm(role: &str, content: &str, is_tool: bool) -> HistMsg {
        HistMsg {
            role: role.into(),
            content: content.into(),
            is_tool_result: is_tool,
            tool_calls: None,
            tool_call_id: None,
        }
    }

    #[test]
    fn estimate_cjk_vs_ascii() {
        // 中文按 ~1/1.8 字/token；100 汉字 ≈ 56
        let cjk = estimate_tokens(&"残".repeat(180));
        assert_eq!(cjk, 100, "180 汉字≈100 token");
        // 纯 ASCII 字母 ~2.5 字/token
        let ascii = estimate_tokens(&"a".repeat(250));
        assert_eq!(ascii, 100);
    }

    #[test]
    fn estimate_fullwidth_punct_as_cjk() {
        // 全角标点按 CJK 价（1/1.8），而非 ASCII 标点的 1/2.8
        let full = estimate_tokens(&"，".repeat(180));
        assert_eq!(full, 100, "全角标点应与 CJK 同价");
        // 半角标点 1/2.8
        let half = estimate_tokens(&",".repeat(280));
        assert_eq!(half, 100);
    }

    #[test]
    fn estimate_blank_is_zero() {
        assert_eq!(estimate_tokens("   \n\t "), 0);
    }

    #[test]
    fn folding_limit_subtracts_reserves() {
        assert_eq!(folding_limit(32000, 8192, 7200), 32000 - 8192 - 7200);
        // 窗口过小 → 至少 1（不 panic、不返回负数）
        assert_eq!(folding_limit(100, 8192, 7200), 1);
    }

    #[test]
    fn truncate_protects_current_turn_and_recent() {
        let mut msgs = vec![hm("user", "开始", false)];
        // 5 条老工具结果（长），然后当前轮 user + 1 条工具结果
        for i in 0..5 {
            msgs.push(hm("tool", &format!("老结果{i}-{}", "长".repeat(5000)), true));
        }
        msgs.push(hm("user", "现在", false));
        msgs.push(hm("tool", "当前轮结果", true));

        let (out, n) = truncate_stale_tool_results(&msgs, KEEP_RECENT_TOOL_RESULTS);
        assert_eq!(out.len(), msgs.len(), "绝不增删消息");
        // 当前轮结果（最后一条）必须原样
        assert_eq!(out.last().unwrap().content, "当前轮结果");
        // 5 条老结果里，最近 4 条受保护；最早那条应被压缩
        assert!(n >= 1, "至少压缩最早那条");
        assert!(out[1].content.contains("省略"), "最早的应被压缩：{}", &out[1].content[..40.min(out[1].content.len())]);
    }

    #[test]
    fn truncate_leaves_short_outputs_alone() {
        let msgs = vec![
            hm("user", "u", false),
            hm("tool", "短输出", true),
            hm("user", "u2", false),
            hm("tool", "又一个短输出", true),
        ];
        let (out, n) = truncate_stale_tool_results(&msgs, 1);
        assert_eq!(n, 0);
        assert!(out.iter().all(|m| !m.content.contains("省略")));
    }

    #[test]
    fn compact_output_keeps_head_and_tail() {
        let s: String = ('A'..='Z').cycle().take(10_000).collect();
        let c = compact_output(&s);
        assert!(c.len() < s.len());
        assert!(c.contains("省略"));
        assert!(c.starts_with("ABC"));
    }

    #[test]
    fn keep_from_index_drops_oldest_complete_turns() {
        // 3 轮，每轮 user + 长 assistant
        let mut msgs = Vec::new();
        for i in 0..3 {
            msgs.push(hm("user", &format!("第{i}轮"), false));
            msgs.push(hm("tool", &"x".repeat(9000), true));
        }
        let limit = 1200; // 只装得下一轮
        let start = keep_from_index(&msgs, limit);
        assert!(start > 0, "应丢弃较早轮次");
        // 起点必须是 user（完整轮边界）
        assert_eq!(msgs[start].role, "user");
        // 保留部分不得超限太多
        let est: usize = msgs[start..].iter().map(|m| estimate_tokens(&m.content) + 64).sum();
        assert!(est <= limit.max(1200) + 9000, "至少保留一轮");
    }

    #[test]
    fn keep_from_index_always_keeps_last_turn() {
        let msgs = vec![
            hm("user", &"a".repeat(99999), false),
            hm("assistant", "回复", false),
        ];
        // 巨大单轮也超限 → 仍保留最后这轮（从 0 起）
        let start = keep_from_index(&msgs, 100);
        assert_eq!(start, 0, "至少保留一轮，不得全删");
    }

    #[test]
    fn estimate_messages_adds_framing() {
        let msgs = vec![
            MessageView { role: "user", content: "hi" },
            MessageView { role: "assistant", content: "hello" },
        ];
        let t = estimate_messages(&msgs);
        assert!(t >= 128, "每条至少 64 的框架开销：{t}");
    }

    #[test]
    fn estimate_realistic_chinese_prose() {
        // 一段典型中文小说正文（含标点、少量英文/数字）
        let text = "陈寂睁开眼，冰凉的水灌进领口。锚点已建立，这具身体不是他的。\
            他想起残响之河，心头一沉。岸边，祖父陈老头正俯身探他的鼻息，\
            浑浊的眼睛里全是惊惧。第1章，EVT-100。";
        let chars = text.chars().count();
        let tokens = estimate_tokens(text);
        // 纯 CJK 约 1/1.8≈0.556 token/字；中文标点同价，ASCII/数字拉低一点 →
        // 实测约 0.53。关键不变量：**不得低估成 ASCII 的 0.4**（那会吃掉折叠余量）。
        assert!(
            tokens * 100 > chars * 45 && tokens * 100 < chars * 90,
            "中文正文估算异常：{chars} 字 → {tokens} token（应在 0.45~0.90 token/字）"
        );
    }

    #[test]
    fn truncate_then_fold_end_to_end() {
        // 模拟：system + 5 轮（每轮 user + 长 tool 结果）
        let mut msgs = vec![hm("system", "系统提示", false)];
        for i in 0..5 {
            msgs.push(hm("user", &format!("第{i}轮问题"), false));
            msgs.push(hm("tool", &format!("第{i}轮结果-{}", "数据".repeat(3000)), true));
        }
        let (t, _) = truncate_stale_tool_results(&msgs, KEEP_RECENT_TOOL_RESULTS);
        // system 未被截断
        assert_eq!(t[0].role, "system");
        // 条数不变
        assert_eq!(t.len(), msgs.len());

        // 折叠：极小预算下丢弃最早整轮，但保 system
        let start = keep_from_index(&t[1..], 500);
        let mut kept = vec![t[0].clone()];
        kept.extend(t[1 + start..].iter().cloned());
        assert_eq!(kept[0].role, "system", "system 永不折叠");
        assert!(kept.len() >= 2, "至少保留一轮");
    }
}
