//! 16 门禁判定引擎 —— `engine/gate14.py` 的 Rust 忠实移植。
//!
//! 移植原则（**逐条对齐，不"改进"**）：
//!   1. 关键词表照抄（Python 原文是什么就抄什么，不扩不缩）
//!   2. 判定条件照抄（阈值、比较符、触发章节范围）
//!   3. 关闭条件照抄（门禁不适用时 pass + detail 说明）
//!   4. 每门的 p0/p1 分级照抄
//!
//! 唯一差异：Python 用 `in text`（子串命中），Rust 用 `text.contains()`，语义一致。
//! 0 LLM / 0 随机 / 可重放 —— 与 Python 版同口径，可直接对拍。

use crate::config::Config;
use serde::Serialize;

// ───────────────────────────── 关键词表（照抄 gate14.py，勿改）
/// 恐怖章诡异要素词（gate14.py HORROR_KEYWORDS）
pub const HORROR_KEYWORDS: &[&str] = &[
    "残响", "异常", "诡异", "信息寄生", "认知侵蚀", "叠加", "退相", "残响之河", "折流", "锚点异变",
];
/// 能力动用词（ABILITY_KEYWORDS）
pub const ABILITY_KEYWORDS: &[&str] = &[
    "修为", "逆流转", "水相", "熵流体", "刻痕", "共鸣", "境界突破", "突破",
];
/// 代价词（COST_KEYWORDS）
pub const COST_KEYWORDS: &[&str] = &[
    "代价", "失去", "模糊", "遗忘", "消散", "消失", "消失的", "变淡", "淡去", "疼", "痛", "灼",
    "烧", "裂痕", "碎裂",
];
/// 章末钩子标记（g02）
pub const HOOK_MARKS: &[&str] = &[
    "？", "？。", "…", "……", "到底", "会不会", "难道", "如果", "可是", "却", "但是",
];
/// 小爽点（g03 mini）
pub const MINI_SHUANG: &[&str] = &["顿挫", "转折", "转机", "突破"];
/// 大爽点（g03 major）
pub const MAJOR_SHUANG: &[&str] = &["高潮", "逆转", "绝地", "翻盘", "碾压"];
/// 恐怖手法 Y1-Y4（g10）
pub const HORROR_TECHNIQUES: &[&str] = &[
    "未知恐惧", "日常异化", "感官代入", "规则类恐怖", "恐惧", "异化", "感官", "规则",
];
/// 卷九反转词（g11）
pub const VOL9_REVERSAL: &[&str] = &["反转", "逆转", "揭露", "真相大白", "身份揭露"];
/// 来源神话 6 词硬闸（g14）
pub const MYTH_BANNED: &[&str] = &[
    "存在之基", "存在之核", "高维观测者文明", "高维观测者", "阿莱夫·零", "金色立方体",
];
/// 指令泄漏词（g16）
pub const LEAK_WORDS: &[&str] = &[
    "不解释", "不展开", "不说明", "不交代", "不给出", "不复述", "不得杜撰", "不硬凑",
];
/// G13 确定性 4 维的禁词（det_dims「禁词」）
pub const G13_STOP_WORDS: &[&str] = &["归墟", "熵兽", "热寂前夜"];
/// G13 AI 词（det_dims「AI词」）
pub const G13_AI_WORDS: &[&str] = &[
    "不禁", "竟然", "不可思议", "值得一提", "事实上", "众所周知", "缓缓", "微微",
];
/// 开篇断言 B：金手指（g01）
pub const G01_B: &[&str] = &["推演", "锚点", "刻痕", "面板"];
/// 开篇断言 C：核心矛盾（g01）
pub const G01_C: &[&str] = &["残响", "熵", "代价", "遗忘", "债", "欠", "还不上"];
/// 开篇断言 D：身份交代（g01，2026-10-02 身份归一口径）
pub const G01_D: &[&str] = &[
    "穿越", "上辈子", "前世", "两具身体", "另一个地方", "这具身体", "身体不是他的", "不是他的",
];
/// 家庭 canon（g15）
pub const CANON_FAMILY: &[(&str, &str)] =
    &[("父亲", "陈大山"), ("母亲", "林氏"), ("弟弟", "陈默")];
pub const CANON_NAMES: &[&str] = &["陈大山", "林氏", "陈默", "陈老头"];

// ───────────────────────────── 结果类型

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub gate: String,
    pub issue: String,
    pub action: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GateResult {
    pub pass: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub p0: Vec<Finding>,
    pub p1: Vec<Finding>,
}

impl GateResult {
    fn ok(detail: impl Into<String>) -> Self {
        GateResult { pass: true, detail: Some(detail.into()), p0: vec![], p1: vec![] }
    }
    fn p0(&mut self, gate: &str, issue: impl Into<String>, action: impl Into<String>) {
        self.pass = false;
        self.p0.push(Finding { gate: gate.into(), issue: issue.into(), action: action.into() });
    }
    fn p1(&mut self, gate: &str, issue: impl Into<String>, action: impl Into<String>) {
        self.p1.push(Finding { gate: gate.into(), issue: issue.into(), action: action.into() });
    }
    fn detail(&mut self, d: impl Into<String>) {
        self.detail = Some(d.into());
    }
}

/// 门禁上下文的可注入依赖（用 trait 隔离 DB/文件，便于纯函数测试）。
pub trait GateCtx {
    /// 本章场景粗判（"心理"/"战斗"/…），供 g10；无则 "未知"
    fn scene_of(&self, volume: &str, chapter: u32) -> String;
    /// 前序已入库正文字本（供 g03 跨章）
    fn prev_chapters_text(&self, volume: &str, chapter: u32, window: u32) -> String;
    /// 到期未回收伏笔（供 g08）：(id, content)
    fn due_foreshadows(&self, volume: &str, chapter: u32) -> Vec<(String, String)>;
    /// 角色性格卡口头禅列表（供 g06）
    fn catchphrases(&self) -> Vec<String>;
    /// kb_knowledge drift 黑名单词（供 g14）
    fn drift_terms(&self) -> Vec<String>;
    /// 67 维总数（供 g13；无 DB 返回 0）
    fn scoring_dims_total(&self) -> i64;
}

// ───────────────────────────── 工具

/// 字数（去空白）—— 对齐 Python `_words`
pub fn words(text: &str) -> usize {
    text.chars().filter(|c| !c.is_whitespace()).count()
}

/// 统计关键词命中数（对齐 Python `sum(1 for kw in list if kw in text)`）
fn count_hits(text: &str, kws: &[&str]) -> usize {
    kws.iter().filter(|k| text.contains(**k)).count()
}

/// 找出现过的词（对齐 `{w: text.count(w) for w in kws if w in text}`）
fn hits(text: &str, kws: &[&str]) -> Vec<(String, usize)> {
    kws.iter()
        .filter(|k| text.contains(**k))
        .map(|k| (k.to_string(), text.matches(*k).count()))
        .collect()
}

// ───────────────────────────── G01
pub fn g01_opening4(text: &str, chapter: u32, _cfg: &Config) -> GateResult {
    let mut out = GateResult::ok("");
    if chapter > 3 {
        out.detail("非开篇章（ch>3），G01 不适用，pass");
        return out;
    }
    let a = text.contains("陈寂");
    let b = G01_B.iter().any(|k| text.contains(*k));
    let c = G01_C.iter().any(|k| text.contains(*k));
    let d = G01_D.iter().any(|k| text.contains(*k));
    let hard_missing: Vec<&str> = [("A主角出场", a), ("B金手指亮相", b), ("D身份交代", d)]
        .iter()
        .filter(|(_, ok)| !ok)
        .map(|(n, _)| *n)
        .collect();
    let c_missing = !c;
    let mut detail = format!("开篇断言 A命中={a} B命中={b} C命中={c} D命中={d}");
    if !hard_missing.is_empty() || c_missing {
        let miss: Vec<&str> = [
            ("A主角出场", a),
            ("B金手指亮相", b),
            ("C核心矛盾", c),
            ("D身份交代", d),
        ]
        .iter()
        .filter(|(_, ok)| !ok)
        .map(|(n, _)| *n)
        .collect();
        detail.push_str(&format!("；缺{miss:?}"));
    }
    out.detail(detail);
    if !hard_missing.is_empty() {
        out.p0(
            "G01_opening4",
            format!("开篇硬性断言缺 {hard_missing:?}"),
            "补对应要素进前500字（黄金三章铁律）",
        );
    }
    if c_missing && chapter <= 5 {
        out.p1(
            "G01_opening4",
            "C核心矛盾（黄金五章观察项，逐章不强制）",
            "4~5 章内补 C 命中（残响/代价/遗忘 任一）",
        );
    }
    out
}

// ───────────────────────────── G02
pub fn g02_hook_density(text: &str, _chapter: u32, cfg: &Config) -> GateResult {
    let min_n = cfg.hook_min() as usize;
    let tail: String = text.chars().rev().take(300).collect::<Vec<_>>().into_iter().rev().collect();
    let hook = HOOK_MARKS.iter().filter(|k| tail.contains(**k)).count();
    let mut out = GateResult::ok(format!("章末300字钩子标记命中={hook}（min_per_chapter={min_n}）"));
    if hook < min_n {
        out.p1(
            "G02_hook_density",
            format!("章末钩密度不足（命中{hook}<{min_n}）"),
            "章末200字内加≥1未闭合钩子（疑点/悬念词）",
        );
    }
    out
}

// ───────────────────────────── G03
pub fn g03_shuang_interval(text: &str, chapter: u32, volume: &str, ctx: &dyn GateCtx) -> GateResult {
    let prev = ctx.prev_chapters_text(volume, chapter, 5);
    if prev.is_empty() {
        return GateResult::ok("无已入库前序正文（04_正文），G03 跨章爽点间隔 skip_no_input");
    }
    let all = format!("{prev}{text}");
    let mini = MINI_SHUANG.iter().map(|k| all.matches(k).count()).sum::<usize>();
    let major = MAJOR_SHUANG.iter().map(|k| all.matches(k).count()).sum::<usize>();
    let mut out = GateResult::ok(format!(
        "滚动5章小爽点={mini} 大爽点={major}（R4 滚动5章0大爽点→p0）"
    ));
    if major == 0 {
        out.p0(
            "G03_shuang_interval",
            "滚动5章0大爽点",
            "本章补≥1大爽点（高潮/逆转类节拍）",
        );
    } else if mini < 5 {
        out.p1(
            "G03_shuang_interval",
            format!("小爽点密度偏低（{mini}）"),
            "补小爽点节拍（顿挫/转机）",
        );
    }
    out
}

// ───────────────────────────── G04
pub fn g04_ability_cost(text: &str, _chapter: u32, _cfg: &Config) -> GateResult {
    let ability = count_hits(text, ABILITY_KEYWORDS);
    let cost = count_hits(text, COST_KEYWORDS);
    let mut out = GateResult::ok(format!("能力动用词={ability} 代价词={cost}"));
    if ability > 0 && cost == 0 {
        out.p1(
            "G04_ability_cost",
            "能力动用章无代价描写（W4/T2/R3）",
            "补代价闭环（失去/模糊/消散类描写，R3 代价闭环）",
        );
    }
    out
}

// ───────────────────────────── G05
pub fn g05_pov_consistent(text: &str, _chapter: u32) -> GateResult {
    if text.contains("陈寂") {
        return GateResult::ok("主角陈寂视角在场");
    }
    let mut out = GateResult::ok("陈寂0命中，视角锁定失守");
    out.p0(
        "G05_pov_consistent",
        "正文无主角陈寂（技06 视角锁定：非陈寂视角章打回）",
        "视角回陈寂，或删除本章/改非正文场景",
    );
    out
}

// ───────────────────────────── G06
pub fn g06_character_consistency(text: &str, ctx: &dyn GateCtx) -> GateResult {
    let catches = ctx.catchphrases();
    if catches.is_empty() {
        return GateResult::ok("角色性格卡0口头禅字段（P4/P6 无判定输入），skip_no_input");
    }
    for c in &catches {
        if !c.is_empty() && text.contains(c) {
            return GateResult::ok(format!("角色口头禅命中「{c}」"));
        }
    }
    GateResult::ok(format!("查了 {} 条口头禅，0 命中（P4/P6 软判定）", catches.len()))
}

// ───────────────────────────── G09
pub fn g09_info_density(text: &str, _chapter: u32, cfg: &Config) -> GateResult {
    let cap = cfg.info_density_max() as usize;
    let w = words(text);
    let expl = ["即", "又称", "所谓", "设定", "规则"]
        .iter()
        .map(|k| text.matches(k).count())
        .sum::<usize>();
    let ratio = expl as f64 / w.max(1) as f64;
    let mut out = GateResult::ok(format!("字数={w} cap={cap} 说明词密度={ratio:.4}"));
    let word_max = cfg.word_max() as usize;
    if w > word_max {
        out.p0(
            "G09_info_density",
            format!("字数{w}>硬上限{word_max}"),
            "砍字/分章（gates.chapter_word_max）",
        );
    } else if w > cap * 3 / 2 {
        out.p1(
            "G09_info_density",
            format!("字数{w}>cap×1.5（信息密度软提示）"),
            "压缩说明密度（<800优秀口径）",
        );
    }
    out
}

// ───────────────────────────── G10
pub fn g10_horror_chapter(
    text: &str,
    chapter: u32,
    volume: &str,
    ctx: &dyn GateCtx,
) -> GateResult {
    let scene = ctx.scene_of(volume, chapter);
    let is_horror = scene == "心理" || HORROR_KEYWORDS.iter().any(|k| text.contains(*k));
    if !is_horror {
        return GateResult::ok("非恐怖章（场景≠心理 且 0 诡异要素词），G10 不适用");
    }
    let y = count_hits(text, HORROR_TECHNIQUES);
    let mut out = GateResult::ok(format!("恐怖章（场景={scene}），Y1-Y4 并集命中={y}"));
    if y == 0 {
        out.p1(
            "G10_horror_chapter",
            "恐怖章未命中 Y1-Y4 任一恐怖手法",
            "补≥1 恐怖手法（Y1未知恐惧/Y2日常异化/Y3感官代入/Y4规则类恐怖）",
        );
    }
    out
}

// ───────────────────────────── G11
pub fn g11_vol9_reversal(text: &str, volume: &str) -> GateResult {
    if volume != "卷09" {
        return GateResult::ok("非卷09，G11 卷九反转不适用");
    }
    let rev = VOL9_REVERSAL.iter().map(|k| text.matches(k).count()).sum::<usize>();
    let mut out = GateResult::ok(format!("卷09 反转关键词命中={rev}（技34 强制4层）"));
    if rev < 4 {
        out.p1(
            "G11_vol9_reversal",
            format!("卷09反转不足（命中{rev}<4层）"),
            "补卷九4层反转登记（技34 L3 细纲门禁，L5 软提示）",
        );
    }
    out
}

// ───────────────────────────── G12
pub fn g12_naming_tier(text: &str, volume: &str, cfg: &Config) -> GateResult {
    // 卷号取自 volume 末两位（对齐 Python `int(volume[-2:])`）
    let vnum: u32 = volume
        .chars()
        .rev()
        .take(2)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>()
        .parse()
        .unwrap_or(0);
    let tier = if vnum <= 2 {
        "U0-1"
    } else if vnum <= 4 {
        "U2-3"
    } else {
        "U4+"
    };
    let banned = cfg.banned_words(tier);
    if banned.is_empty() {
        return GateResult::ok(format!("当前阶位={tier}（卷{vnum:02}），无禁词词表（原文未载明），pass"));
    }
    let b: Vec<&str> = banned.iter().map(|s| s.as_str()).collect();
    let h = hits(text, &b);
    let mut out = GateResult::ok(format!("阶位={tier} 禁词命中={}", fmt_hits(&h)));
    if !h.is_empty() {
        for (w, n) in h {
            out.p0(
                "G12_naming_tier",
                format!("U0-1 朴实档越档命名词「{w}」×{n}"),
                format!("改朴实命名（banned_by_tier.{tier}.banned_words）"),
            );
        }
    }
    out
}

// ───────────────────────────── G13
pub fn g13_scoring_67dim(text: &str, cfg: &Config, ctx: &dyn GateCtx) -> GateResult {
    let dims_total = ctx.scoring_dims_total();
    let w = words(text);
    let det: Vec<(&str, bool)> = vec![
        ("字数", cfg.word_min() as usize <= w && w <= cfg.word_max() as usize),
        ("禁词", !G13_STOP_WORDS.iter().any(|k| text.contains(*k))),
        ("AI词", !G13_AI_WORDS.iter().any(|k| text.contains(*k))),
        (
            "感叹号",
            (text.matches('!').count() + text.matches('！').count()) <= 1,
        ),
    ];
    let bad: Vec<&str> = det.iter().filter(|(_, ok)| !ok).map(|(n, _)| *n).collect();
    let mut out = GateResult::ok(format!(
        "67维={dims_total}，确定性可量化4维命中不达标={}，其余{}维 needs_llm=True（LLM解释层占位，0编造分数）",
        if bad.is_empty() { "0".to_string() } else { format!("{bad:?}") },
        dims_total.saturating_sub(4)
    ));
    if !bad.is_empty() {
        out.p1(
            "G13_scoring_67dim",
            format!("确定性维度不达标：{bad:?}"),
            "按对应维度修复（字数/禁词/AI词/感叹号）；LLM 61维占位不阻断",
        );
    }
    out
}

// ───────────────────────────── G14
pub fn g14_world_consistency(text: &str, ctx: &dyn GateCtx) -> GateResult {
    let mut out = GateResult { pass: true, detail: Some(String::new()), p0: vec![], p1: vec![] };
    let myth = hits(text, MYTH_BANNED);
    if !myth.is_empty() {
        for (w, n) in &myth {
            out.p1(
                "G14_world_consistency",
                format!("来源神话词「{w}」×{n}（面板台词白名单只允许「锚点已建立」）"),
                "改写为「锚点已建立」/「未知来源」中性词，不写来源机制",
            );
        }
    }
    let terms = ctx.drift_terms();
    let terms_ref: Vec<&str> = terms.iter().map(|s| s.as_str()).collect();
    let h = hits(text, &terms_ref);
    out.detail(format!(
        "；来源神话6词命中={}；drift词表={}个 命中={}",
        fmt_hits(&myth),
        terms.len(),
        fmt_hits(&h)
    ));
    if !h.is_empty() {
        for (w, n) in h {
            out.p0(
                "G14_world_consistency",
                format!("drift黑名单词「{w}」×{n}"),
                "按勘误账本改词（kb_knowledge verdict=drift）",
            );
        }
    }
    out
}

// ───────────────────────────── G15
pub fn g15_family_consistency(text: &str) -> GateResult {
    let mut out = GateResult::ok("");
    let mut hit = 0usize;
    let re_str = CANON_FAMILY
        .iter()
        .map(|(t, _)| regex::escape(t))
        .collect::<Vec<_>>()
        .join("|");
    let re = regex::Regex::new(&format!(r"(?:{re_str})(?:的|叫)?([\u{{4e00}}-\u{{9fa5}}]{{2,4}})")).unwrap();
    for caps in re.captures_iter(text) {
        let whole = caps.get(0).unwrap().as_str();
        let mentioned = caps.get(1).unwrap().as_str();
        // 找出命中的称谓 → 期望 canon 名
        let title = CANON_FAMILY
            .iter()
            .find(|(t, _)| whole.starts_with(t))
            .map(|(t, c)| (*t, *c));
        let Some((title, canon_name)) = title else { continue };
        // 合法：mentioned 的 2/3/4 字前缀 ∈ CANON_NAMES
        let matched = (2..=4).any(|n| {
            if mentioned.chars().count() < n {
                return false;
            }
            let prefix: String = mentioned.chars().take(n).collect();
            CANON_NAMES.contains(&prefix.as_str())
        });
        if matched {
            continue;
        }
        hit += 1;
        out.p1(
            "G15_family_consistency",
            format!("称谓「{title}」提及「{mentioned}」≠canon「{canon_name}」（家庭定版·后续不得改）"),
            "按 canon 改回：父=陈大山/母=林氏/弟=陈默/祖=陈老头",
        );
    }
    out.pass = hit == 0;
    out.detail(format!(
        "家庭canon错配={hit}处（父=陈大山/母=林氏/弟=陈默/祖=陈老头，后续不得改）"
    ));
    out
}

// ───────────────────────────── G16
pub fn g16_leak_words(text: &str) -> GateResult {
    let h = hits(text, LEAK_WORDS);
    let mut out = GateResult::ok(format!("泄漏词命中={}", fmt_hits(&h)));
    if !h.is_empty() {
        for (w, n) in h {
            out.p1(
                "G16_leak_words",
                format!("指令泄漏词「{w}」×{n}（prompt 规则词入正文，非文学表达）"),
                "删除或改写为叙事（规则词只进 system 指令区，不得出现在正文）",
            );
        }
    }
    out
}

// ───────────────────────────── G07 / G08（需 DB 输入）
/// G07 OOC：简化口径——角色已死/境界倒退/情绪矛盾/压抑越界（4 判据）。
/// 无角色状态数据 → skip_no_input（不阻断），与 Python 同口径。
pub fn g07_ooc(_text: &str, _ctx: &dyn GateCtx) -> GateResult {
    // 角色状态数据源（char_default_fill.json + char_state 表）尚未接入 Rust 侧，
    // 按 Python 的降级口径 skip_no_input，不臆造判定。
    GateResult::ok("角色状态数据未接入（对齐 Python skip_no_input 口径），G07 不阻断")
}

/// G08 伏笔回收率：读 foreshadow 到期项，用贝叶斯后验双档（P2=0.6 / P1=0.5）判定。
pub fn g08_foreshadow_recover(
    text: &str,
    chapter: u32,
    volume: &str,
    ctx: &dyn GateCtx,
) -> GateResult {
    let rows = ctx.due_foreshadows(volume, chapter);
    if rows.is_empty() {
        return GateResult::ok("本章无到期未回收伏笔，skip_no_input");
    }
    let (p2, p1_line) = (0.6_f64, 0.5_f64);
    let mut flagged = 0usize;
    let mut out = GateResult::ok("");
    for (fid, content) in &rows {
        // 证据判定：去编号/去括号后取首段文字（>=2字），若出现在正文则 evidence=0.5
        let core = strip_ref_and_paren(content);
        let core6: String = core.chars().take(6).collect();
        let evidence = if !core6.is_empty() && text.contains(&core6) { 0.5 } else { 0.0 };
        let post = bayes_posterior(0.5, evidence);
        if post < p1_line {
            flagged += 1;
            out.p0(
                "G08_foreshadow_recover",
                format!("伏笔{fid} 后验={post:.3} < P1线{p1_line}（空壳）"),
                "本章兑现该伏笔或明确延期（gates.bayes 双档）",
            );
        } else if post < p2 {
            flagged += 1;
            out.p1(
                "G08_foreshadow_recover",
                format!("伏笔{fid} 后验={post:.3} < P2线{p2}"),
                "补回收证据（正文提及要素词）",
            );
        }
    }
    out.pass = flagged == 0;
    out.detail(format!(
        "到期未回收伏笔={} 命中降级={flagged}（P2={p2}/P1={p1_line}）",
        rows.len()
    ));
    out
}

/// 贝叶斯后验 —— **逐字移植 `dynamics.bayes_mood`**：
/// ```text
/// eff_lr    = 1 + evidence*(lik_ratio-1)     # lik_ratio 默认 2.0
/// posterior = (prior*eff_lr) / (prior*eff_lr + (1-prior))
/// ```
/// evidence=0.5 → 有效似然比 1.5；evidence=0.0 → 1.0（后验=先验 0.5）。
fn bayes_posterior(prior: f64, evidence: f64) -> f64 {
    let prior = if prior > 0.0 && prior < 1.0 { prior } else { 0.5 };
    let evidence = evidence.clamp(0.0, 1.0);
    let lik_ratio = 2.0_f64;
    let eff_lr = 1.0 + evidence * (lik_ratio - 1.0);
    (prior * eff_lr) / (prior * eff_lr + (1.0 - prior))
}

/// 去掉卡编号（如 REL-99-004）与括号内容，返回核心文本（对齐 Python g08 的 core 提取）。
fn strip_ref_and_paren(s: &str) -> String {
    let re_ref = regex::Regex::new(r"\b[A-Z]+-\d+(?:-\d+)?\b").unwrap();
    let a = re_ref.replace_all(s, "");
    let re_paren = regex::Regex::new(r"[（(][^）)]*[）)]").unwrap();
    re_paren.replace_all(&a, "").trim().to_string()
}

fn fmt_hits(h: &[(String, usize)]) -> String {
    if h.is_empty() {
        "0".to_string()
    } else {
        h.iter()
            .map(|(w, n)| format!("{w}×{n}"))
            .collect::<Vec<_>>()
            .join(",")
    }
}

// ───────────────────────────── 主入口

/// 门禁执行顺序（供 CLI 展示；与 run_all 的调用序一致）。
pub const GATE_COUNT_ORDER: &[&str] = &[
    "G01_opening4",
    "G02_hook_density",
    "G03_shuang_interval",
    "G04_ability_cost",
    "G05_pov_consistent",
    "G06_character_consistency",
    "G07_OOC",
    "G08_foreshadow_recover",
    "G09_info_density",
    "G10_horror_chapter",
    "G11_vol9_reversal",
    "G12_naming_tier",
    "G13_scoring_67dim",
    "G14_world_consistency",
    "G15_family_consistency",
    "G16_leak_words",
];

#[derive(Debug, Clone, Serialize)]
pub struct GateReport {
    pub verdict: String,
    pub p0: Vec<Finding>,
    pub p1: Vec<Finding>,
    pub gates: std::collections::BTreeMap<String, GateBrief>,
    pub stats: GateStats,
}

#[derive(Debug, Clone, Serialize)]
pub struct GateBrief {
    pub pass: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GateStats {
    pub words: usize,
    pub gates_run: usize,
}

/// 跑全 16 门禁（顺序与 gate14.GATE_ORDER 一致）。
pub fn run_all(text: &str, chapter: u32, volume: &str, cfg: &Config, ctx: &dyn GateCtx) -> GateReport {
    let mut p0 = Vec::new();
    let mut p1 = Vec::new();
    let mut gates = std::collections::BTreeMap::new();

    macro_rules! run {
        ($name:expr, $r:expr) => {{
            let r: GateResult = $r;
            gates.insert(
                $name.to_string(),
                GateBrief { pass: r.pass, detail: r.detail.clone().unwrap_or_default() },
            );
            p0.extend(r.p0);
            p1.extend(r.p1);
        }};
    }

    run!("G01_opening4", g01_opening4(text, chapter, cfg));
    run!("G02_hook_density", g02_hook_density(text, chapter, cfg));
    run!("G03_shuang_interval", g03_shuang_interval(text, chapter, volume, ctx));
    run!("G04_ability_cost", g04_ability_cost(text, chapter, cfg));
    run!("G05_pov_consistent", g05_pov_consistent(text, chapter));
    run!("G06_character_consistency", g06_character_consistency(text, ctx));
    run!("G07_OOC", g07_ooc(text, ctx));
    run!("G08_foreshadow_recover", g08_foreshadow_recover(text, chapter, volume, ctx));
    run!("G09_info_density", g09_info_density(text, chapter, cfg));
    run!("G10_horror_chapter", g10_horror_chapter(text, chapter, volume, ctx));
    run!("G11_vol9_reversal", g11_vol9_reversal(text, volume));
    run!("G12_naming_tier", g12_naming_tier(text, volume, cfg));
    run!("G13_scoring_67dim", g13_scoring_67dim(text, cfg, ctx));
    run!("G14_world_consistency", g14_world_consistency(text, ctx));
    run!("G15_family_consistency", g15_family_consistency(text));
    run!("G16_leak_words", g16_leak_words(text));

    let verdict = if !p0.is_empty() {
        "reject"
    } else if !p1.is_empty() {
        "fix"
    } else {
        "pass"
    }
    .to_string();

    GateReport {
        verdict,
        p0,
        p1,
        gates,
        stats: GateStats { words: words(text), gates_run: 16 },
    }
}
