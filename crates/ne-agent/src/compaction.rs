//! 上下文压缩（M9）—— **摘要下沉**：把早期轮次折叠成一段结构化摘要。
//!
//! # 与"截断"的本质区别
//!
//! `context::truncate_stale_tool_results` 只是把老工具结果的正文压短，**不减少条数**；
//! `keep_from_index` 只是从最早处丢整轮。二者都发生在"发给模型的那一份"上，
//! 是**无状态**的机械处理。本模块不同：
//!
//!   * 用一个（可选的）LLM 把被折叠的历史**理解并重写成摘要**，保住"我们做了什么、
//!     结论是什么"，而不是简单丢弃；
//!   * 摘要**持久化**（`compactions` 表），跨进程、跨轮次稳定；
//!   * **原文一律不删**——`messages` 表保持完整，`history_read` 可按 id 回读。
//!
//! # 投影（projection）
//!
//! 这是本模块的核心概念，也是对"为 UI 铺垫"的直接兑现：
//!
//! ```text
//!   原始历史（messages 表，全量）
//!       │
//!       ├─ 最早 N 条 ──┐
//!       │             └─→ 折叠为一条「摘要」消息（role=user，标注为压缩摘要）
//!       └─ 其余 ────────→ 原样保留
//!       │
//!       ▼
//!   投影结果 = [摘要] + [保留区]      ← 这才是发给模型 / UI 展示的"上下文"
//! ```
//!
//! UI 只要调用 [`project`] 就能拿到"实际上下文"与"压缩透明度"（折叠了几条、摘要是什么），
//! 不需要自己理解折叠规则——规则只有这一处。
//!
//! # 契约对照
//!
//! 契约对照太墟 `harness/compaction/CompactionManager.kt` + `CompactionModels.kt`。
//! 太墟 `resolveCompressAnchor(messages, mode, anchor)`：锚点必须是**原样、唯一**的用户消息摘录
//! （≥8 字符），`before` 压锚点轮之前、`after` 压当前轮之前；`CompactionPayload` 含摘要 + 折叠条数 +
//! 保留条数 + 压缩前 token 估算 + 时间戳。
//!
//! 本模块**取其契约**：`mode/anchor` 语义、越界与歧义校验、payload 字段。
//! **弃其体量**：太墟的 entry-sequence 水位线自愈、retainedMessagesJson 内嵌快照、分支摘要树，
//! 是为它"多面板并发 + 移动端 OOM 规避"设计的；ne-agent 顺序执行、单机 DB，
//! 用"折到哪条消息 id"一个指针即可，不引入水位线。

use anyhow::{bail, Result};

use crate::context;
use crate::session::{CompactionRow, SessionStore};

/// 锚点最短字符数（对齐太墟 `MIN_COMPRESS_ANCHOR_CHARS`）。
pub const MIN_COMPRESS_ANCHOR_CHARS: usize = 8;
/// 摘要长度上限（对齐太墟 `MAX_SUMMARY_CHARS`）。
pub const MAX_SUMMARY_CHARS: usize = 16_000;
/// 投影里摘要消息的固定抬头（UI / 模型都靠它识别"这是压缩摘要"）。
pub const SUMMARY_HEADER: &str = "【早前对话摘要（已压缩，原文可按需回读）】";

/// 压缩模式（对齐太墟 compress 工具的 `mode`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressMode {
    /// 压缩锚点轮**之前**的全部历史（保留锚点轮及之后）。
    Before,
    /// 压缩除最后一条用户消息（进行中轮次）外的全部**已完成**历史。
    After,
}

impl CompressMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "before" => Some(CompressMode::Before),
            "after" => Some(CompressMode::After),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            CompressMode::Before => "before",
            CompressMode::After => "after",
        }
    }
}

/// 锚点解析结果（对齐太墟 `CompressAnchorResult`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScrollOutcome {
    /// 解析成功：`keep_from` 这条消息**及之后**保留原文。
    Resolved { keep_from_msg_id: i64 },
    /// 校验失败：`message` 是可回写给模型/用户的说明。
    Invalid { message: String },
}

/// 把"折叠边界"翻译成"保留区起点"的**纯函数**（对齐太墟 `resolveCompressAnchor`）。
///
/// `hist` 必须是当前分支的线性历史（`SessionStore::history` 的输出）。
///
/// * `Before`：锚点必须**原样、唯一**地出现在某条 user 消息里；保留该轮及之后。
/// * `After`：锚点仅作展示/审计，边界取**最后一条 user 消息**（进行中轮次不折）。
pub fn resolve_compress_anchor(
    hist: &[crate::session::MsgRow],
    mode: CompressMode,
    anchor: &str,
) -> ScrollOutcome {
    if hist.is_empty() {
        return ScrollOutcome::Invalid { message: "当前会话没有历史可压缩".into() };
    }
    let keep_idx = match mode {
        CompressMode::Before => {
            let a = anchor.trim();
            if a.chars().count() < MIN_COMPRESS_ANCHOR_CHARS {
                return ScrollOutcome::Invalid {
                    message: format!(
                        "anchor 必须原样摘自某条用户消息且足够独特（至少 {MIN_COMPRESS_ANCHOR_CHARS} 字符），用于唯一定位压缩边界"
                    ),
                };
            }
            let hits: Vec<usize> = hist
                .iter()
                .enumerate()
                .filter(|(_, m)| m.role == "user" && m.content.contains(a))
                .map(|(i, _)| i)
                .collect();
            match hits.len() {
                0 => {
                    return ScrollOutcome::Invalid {
                        message: "锚点在当前上下文中没有匹配到任何用户消息。若该轮已被折叠进既有摘要则无法再以它为锚点，请改用更近的轮次；否则请原样复制消息中的一段文字重试"
                            .into(),
                    }
                }
                1 => hits[0],
                n => {
                    return ScrollOutcome::Invalid {
                        message: format!(
                            "锚点匹配到 {n} 条用户消息，请提供更长、更独特的摘录以唯一定位"
                        ),
                    }
                }
            }
        }
        CompressMode::After => match hist.iter().rposition(|m| m.role == "user") {
            Some(i) => i,
            None => {
                return ScrollOutcome::Invalid {
                    message: "会话中没有用户轮次，无从界定'已完成历史'".into(),
                }
            }
        },
    };

    if keep_idx == 0 {
        return ScrollOutcome::Invalid {
            message: "边界之前没有可压缩的内容（它已是当前上下文的第一条消息）".into(),
        };
    }
    ScrollOutcome::Resolved { keep_from_msg_id: hist[keep_idx].id }
}

/// 一次压缩的产出（对齐太墟 `CompactionPayload` 的**必要子集**）。
#[derive(Debug, Clone)]
pub struct CompactionOutcome {
    pub summary: String,
    pub keep_from_msg_id: i64,
    pub folded_message_count: usize,
    pub retained_message_count: usize,
    pub est_tokens_before: usize,
    pub est_tokens_after: usize,
    /// 摘要是 LLM 生成（true）还是机械降级（false）——诚实标注，供 UI 显示。
    pub llm_summarized: bool,
}

/// 摘要生成的输入：被折叠区间的消息。
pub struct SummarizeInput<'a> {
    pub folded: &'a [crate::session::MsgRow],
}

/// 摘要生成器。默认用 LLM；失败/不可用时降级为机械摘要（**绝不因摘要失败而丢内容**）。
pub trait Summarizer {
    fn summarize(&self, input: &SummarizeInput) -> Result<String>;
}

/// 机械摘要：不调模型，把折叠区间压成"轮次 + 每轮首行"的骨架。
///
/// 降级路径**必须无损关键信息**：保留每条 user 轮的首段文字，避免把"用户要过什么"丢掉。
pub struct MechanicalSummarizer;

impl Summarizer for MechanicalSummarizer {
    fn summarize(&self, input: &SummarizeInput) -> Result<String> {
        let mut s = String::new();
        s.push_str("（机械摘要：摘要模型不可用时的降级结果）\n");
        let mut turns = 0usize;
        for m in input.folded {
            if m.role == "user" {
                turns += 1;
                let first = m.content.lines().next().unwrap_or("").trim();
                let brief: String = first.chars().take(120).collect();
                s.push_str(&format!("- 轮 {turns}：{brief}\n"));
            }
        }
        s.push_str(&format!("\n共折叠 {} 条消息、{turns} 个用户轮。", input.folded.len()));
        Ok(s)
    }
}

/// LLM 摘要：把折叠区间交给模型重写成结构化摘要。
pub struct LlmSummarizer {
    pub llm: ne_engine::llm::LlmParams,
}

impl Summarizer for LlmSummarizer {
    fn summarize(&self, input: &SummarizeInput) -> Result<String> {
        let mut transcript = String::new();
        for m in input.folded {
            let tag = match m.role.as_str() {
                "user" => "用户",
                "assistant" => "助手",
                "tool" => "工具结果",
                other => other,
            };
            let body: String = m.content.chars().take(4_000).collect();
            transcript.push_str(&format!("[{tag}] {body}\n\n"));
        }
        let sys = "你是上下文压缩器。把给定对话压缩成结构化摘要，供后续轮次继续工作。\n\
                   必须保留：①用户的目标与约束原话要点；②已做出的决定与其理由；③已完成的工作与产物路径；\
                   ④当前未决问题与下一步。\n\
                   删除：寒暄、重复、已被推翻的中间尝试、冗长的工具原始输出。\n\
                   只输出摘要正文，不要复述本指令，不要用代码围栏。";
        let user = format!(
            "请压缩以下 {} 条消息（这是会话早期、将被折叠、原文仍可回读的部分）：\n\n{transcript}",
            input.folded.len()
        );
        let msgs = vec![
            ne_engine::llm::Message::system(sys),
            ne_engine::llm::Message::user(user),
        ];
        let out = ne_engine::llm::call(&self.llm, &msgs)?;
        let mut text = out.content.trim().to_string();
        if text.is_empty() {
            bail!("摘要模型返回空文本");
        }
        if text.chars().count() > MAX_SUMMARY_CHARS {
            text = text.chars().take(MAX_SUMMARY_CHARS).collect::<String>();
            text.push_str("\n…（摘要过长已截断）");
        }
        Ok(text)
    }
}

/// 投影结果：**实际上下文** + **压缩透明度**（UI 直接可用）。
#[derive(Debug, Clone)]
pub struct Projection {
    /// 摘要文本（`None` = 该会话未压缩过）。
    pub summary: Option<String>,
    /// 折叠了几条消息进摘要（累计）。
    pub folded_count: usize,
    /// 保留区消息（原文）。
    pub retained: Vec<crate::session::MsgRow>,
    /// 投影后总 token 估算。
    pub est_tokens: usize,
    /// 投影前的全量历史 token 估算。
    pub est_tokens_full: usize,
}

impl Projection {
    /// 转成发送给模型的消息序列（摘要条 + 保留区）。
    ///
    /// 摘要以 **user** 角色注入（对齐太墟：不污染稳定 system 前缀 → 有利于 KV cache）。
    pub fn to_messages(&self) -> Vec<ne_engine::llm::Message> {
        let mut out = Vec::new();
        if let Some(s) = &self.summary {
            out.push(ne_engine::llm::Message::user(format!("{SUMMARY_HEADER}\n{s}")));
        }
        out.extend(self.retained.iter().map(|m| m.to_llm()));
        out
    }

    /// 人类可读渲染（CLI `context` / UI 面板）。
    pub fn render(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!(
            "上下文投影：折叠 {} 条 · 保留 {} 条 · 约 {} tokens（未压缩时 {} tokens）\n",
            self.folded_count,
            self.retained.len(),
            self.est_tokens,
            self.est_tokens_full
        ));
        if self.summary.is_none() {
            s.push_str("（尚未压缩）\n");
        }
        s
    }
}

/// 对某会话做**投影**——不修改任何数据，只算"实际上下文长什么样"。
///
/// 这是压缩状态的**唯一读口径**：`build_messages`（发给模型）与 UI 都走它。
pub fn project(store: &SessionStore, session: &str) -> Result<Projection> {
    let hist = store.history(session)?;
    let latest: Option<CompactionRow> = store.latest_compaction(session)?;
    let est_full: usize = hist.iter().map(|m| context::estimate_tokens(&m.content) + 64).sum();

    let Some(c) = latest else {
        return Ok(Projection {
            summary: None,
            folded_count: 0,
            retained: hist,
            est_tokens: est_full,
            est_tokens_full: est_full,
        });
    };

    // 保留区 = id >= keep_from_msg_id 的部分（历史按 id 升序）
    let retained: Vec<crate::session::MsgRow> = hist
        .into_iter()
        .filter(|m| m.id >= c.keep_from_msg_id)
        .collect();
    let folded = c.folded_message_count.max(0) as usize;
    let est = context::estimate_tokens(&c.summary)
        + 64
        + retained.iter().map(|m| context::estimate_tokens(&m.content) + 64).sum::<usize>();
    Ok(Projection {
        summary: Some(c.summary),
        folded_count: folded,
        retained,
        est_tokens: est,
        est_tokens_full: est_full,
    })
}

/// 执行一次压缩并落库（**不改动 messages 表**）。
///
/// `summarizer` 为 `None` → 用机械摘要（无 LLM 的确定性路径，测试/离线用）。
pub fn compact(
    store: &SessionStore,
    session: &str,
    mode: CompressMode,
    anchor: &str,
    summarizer: Option<&dyn Summarizer>,
) -> Result<CompactionOutcome> {
    let hist = store.history(session)?;
    let keep_from_msg_id = match resolve_compress_anchor(&hist, mode, anchor) {
        ScrollOutcome::Resolved { keep_from_msg_id } => keep_from_msg_id,
        ScrollOutcome::Invalid { message } => bail!("{message}"),
    };

    let (folded, retained): (Vec<_>, Vec<_>) =
        hist.into_iter().partition(|m| m.id < keep_from_msg_id);
    if folded.is_empty() {
        bail!("折叠区为空，无需压缩");
    }
    let est_before: usize =
        folded.iter().map(|m| context::estimate_tokens(&m.content) + 64).sum();

    // 摘要：优先 LLM，失败/缺失降级机械（诚实标注 llm_summarized）
    let input = SummarizeInput { folded: &folded };
    let (summary, llm_summarized) = match summarizer {
        Some(s) => match s.summarize(&input) {
            Ok(text) => (text, true),
            Err(_) => (MechanicalSummarizer.summarize(&input)?, false),
        },
        None => (MechanicalSummarizer.summarize(&input)?, false),
    };

    store.insert_compaction(
        session,
        &summary,
        keep_from_msg_id,
        folded.len() as i64,
        retained.len() as i64,
        est_before as i64,
        mode.as_str(),
    )?;

    let est_after = context::estimate_tokens(&summary)
        + 64
        + retained.iter().map(|m| context::estimate_tokens(&m.content) + 64).sum::<usize>();
    Ok(CompactionOutcome {
        summary,
        keep_from_msg_id,
        folded_message_count: folded.len(),
        retained_message_count: retained.len(),
        est_tokens_before: est_before,
        est_tokens_after: est_after,
        llm_summarized,
    })
}

impl CompactionOutcome {
    /// 供工具/CLI 回写给模型或用户。
    pub fn render(&self, mode: CompressMode) -> String {
        format!(
            "已按 {} 模式压缩：折叠 {} 条消息为结构化摘要（保留后 {} 条）。\n\
             估算 token：{} → {}。原文仍在会话记录中，需要细节时可用 `history_read` 按需回读。\n\
             摘要来源：{}",
            mode.as_str(),
            self.folded_message_count,
            self.retained_message_count,
            self.est_tokens_before,
            self.est_tokens_after,
            if self.llm_summarized { "模型生成" } else { "机械降级（模型不可用）" }
        )
    }
}

#[cfg(test)]
#[path = "compaction_tests.rs"]
mod tests;
