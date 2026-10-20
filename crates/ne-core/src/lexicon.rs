//! 词表单一真值源（SSOT）——**全系统唯一的层词/禁词定义处**。
//!
//! # 为什么在这里
//! 历史教训：设计层词/禁词曾散落在 8 处（`chapter.rs` 的 `BANNED_WORDS` /
//! `MYTH_NEUTRAL` / `DESIGN_LAYER_NEUTRAL`、`health.rs` 的 `BOOT_DESIGN_WORDS`、
//! `outline_rebuild.rs` 的 `DESIGN_WORDS`、`gates.rs` 的 `MYTH_BANNED` /
//! `G13_STOP_WORDS` / `G18_DESIGN_LEAK_WORDS`、`gate14.rs` 的 `MYTH_BANNED`），
//! 改一处必漏五处，且已实测漂移（`BOOT_DESIGN_WORDS` 含「分支演算」而
//! `DESIGN_WORDS` 却是「分支」）。收敛到此处，一处改全系统生效。
//!
//! # 分层模型
//! - **神话词**（`MYTH_NEUTRAL`/`MYTH_BANNED`）：金手指来源神话，设计层可见、正文禁引用。
//! - **设计层词**（`DESIGN_LAYER_NEUTRAL`/`DESIGN_LEAK_WORDS`）：影子演算/工程代号，
//!   只属于设计者，不属于角色即时感知。
//! - **正典禁词**（`BANNED_WORDS`）：世界观废弃概念（归墟解禁后仅剩熵兽系）。
//! - **AI 痕词 / 指令泄漏词**：文风与门禁用。
//!
//! 各 crate 一律 `use ne_core::lexicon::…`，**禁止再定义本地副本**。

// ───────────────────────────── ① 神话词（正文禁引用，注入前中性化）

/// 来源神话 6 词 → 中性词（照抄 MYTH_NEUTRAL）。
/// 键集合 == `MYTH_BANNED`，勿分别维护。
pub const MYTH_NEUTRAL: [(&str, &str); 6] = [
    ("存在之基", "某样东西"),
    ("存在之核", "某样东西"),
    ("高维观测者文明", "未知来源"),
    ("高维观测者", "未知来源"),
    ("阿莱夫·零", "未知来源"),
    ("金色立方体", "未知来源"),
];

/// 来源神话 6 词硬闸（g14）。键集合与 `MYTH_NEUTRAL` 同源。
pub const MYTH_BANNED: [&str; 6] = [
    "存在之基", "存在之核", "高维观测者文明", "高维观测者", "阿莱夫·零", "金色立方体",
];

// ───────────────────────────── ② 设计层词（工程代号）

/// 设计层词 → 中性词（注入前替换长组合，短词兜底）。
pub const DESIGN_LAYER_NEUTRAL: [(&str, &str); 10] = [
    ("推演进化面板", "那股说不清的东西"),
    ("万象推演面板", "那股说不清的东西"),
    ("推演面板", "那股说不清的东西"),
    ("本源推演", "本源一次动用"),
    ("多维推演", "多维辅助"),
    ("未来推演", "预判"),
    ("回溯推演", "回溯"),
    ("跨界推演", "跨界感知"),
    ("基础推演", "基础辅助"),
    ("面板", "那股东西"),
];

/// 设计层词**探测集**（含组合词根），用于门禁/自检的「是否含设计层词」扫描。
/// 与 `DESIGN_LAYER_NEUTRAL` 的键同源；此处含独立词根以覆盖无替换的长组合。
///
/// # 2026-10-16 收窄（误报根治）
/// 原 9 词中混入大量**日常义 / 正典义 / 系统词**，造成系统性误报，已按全库实测剔除：
/// - `算账`——全库 12 处全为「柜台后算账」「爱算账，但底线分明」等日常义（误报）。
/// - `因果流`——3 处：2 处是「因果流**转**」被截断命中，1 处在 L5 提示词的禁用词表自身。
/// - `漏斗`——17 处全在 `00_系统配置` / `05_管线框架`，指「L0→L5 漏斗管线」工程义，
///   与金手指无关；资料库 `materials` 命中 0。
/// - `算路径` / `算因果` / `分支演算`——全库 0 处。
/// - `影子层`——全库仅 1 处，出现在禁用词表自身。
///
/// 保留的真值词根（与 `DESIGN_LAYER_NEUTRAL` 的兜底键一致）：
/// - `推演`：**主角专属金手指专名根**（权威源 `ITEM-99-005`：「推演进化面板：陈寂金手指本体」）。
///   全库**无**「推演」系列世界观职业/机构——原 `CANON_EXEMPT` 豁免表系误造，已删除（2026-10-16）。
/// - `面板`：金手指界面。
pub const DESIGN_LEAK_WORDS: [&str; 2] = ["推演", "面板"];

/// 计算「设计层词」的命中：返回 (词, 命中数)，命中 0 的条目被剔除。
/// 这是门禁 / 自检 / outline 扫描的**统一口径**，勿在调用处另写一套。
///
/// 2026-10-16：原「扣正典专名」逻辑已删除——经实测，`推演` 是**主角专属金手指**
/// （`ITEM-99-005`），不存在需要豁免的世界观职业/机构；原豁免表 8 词全库出现 0 次，
/// 属误造，会把真实泄漏错减为 0。
pub fn design_leak_hits(text: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    for kw in DESIGN_LEAK_WORDS.iter() {
        let n = text.matches(*kw).count();
        if n > 0 {
            out.push(((*kw).to_string(), n));
        }
    }
    out
}

// ───────────────────────────── ③ 正典禁词（世界观废弃概念）

/// 正典禁词（照抄 BANNED_WORDS；归墟解禁后仅剩熵兽系）。
pub const BANNED_WORDS: [&str; 3] = ["归墟", "熵兽", "热寂前夜"];

// ───────────────────────────── ④ 文风 / 指令泄漏词

/// AI 痕迹词（照抄 AI_TRACE_WORDS）。
pub const AI_TRACE_WORDS: [&str; 8] =
    ["不禁", "竟然", "不可思议", "值得一提", "事实上", "众所周知", "缓缓", "微微"];

/// 指令泄漏词（阻断级，g16）。
pub const LEAK_WORDS: [&str; 8] =
    ["不解释", "不展开", "不说明", "不交代", "不给出", "不复述", "不得杜撰", "不硬凑"];

// ───────────────────────────── ⑤ 显式禁令行标记（声明规则而合法提及设计词）

/// 「显式禁令行」标记：含这些词的行是为**声明规则**而提及设计词，属合法。
pub const NEG_MARKS: [&str; 10] = [
    "严禁", "禁止", "不许", "禁出现", "绝不", "永不", "不能出现", "不得出现", "一概不", "不要",
];

/// 某行是否为「显式禁令行」。sanitize / 扫描共用同一口径。
pub fn is_prohibition_line(l: &str) -> bool {
    NEG_MARKS.iter().any(|n| l.contains(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn myth_neutral_keys_match_myth_banned() {
        let mut keys: Vec<&str> = MYTH_NEUTRAL.iter().map(|(k, _)| *k).collect();
        keys.sort();
        let mut banned: Vec<&str> = MYTH_BANNED.to_vec();
        banned.sort();
        assert_eq!(keys, banned, "神话词中性化表与硬闸表键集合必须一致");
    }

    #[test]
    fn design_probe_covers_replacement_roots() {
        // 每个「面板」类组合都应以「面板」这一词根被探测到。
        assert!(DESIGN_LEAK_WORDS.contains(&"面板"));
        // 兜底替换「推演」必须被探测集覆盖。
        assert!(DESIGN_LEAK_WORDS.contains(&"推演"));
    }

    #[test]
    fn prohibition_marker_detects() {
        assert!(is_prohibition_line("严禁出现「面板」"));
        assert!(!is_prohibition_line("他借面板推演未来"));
    }
}
