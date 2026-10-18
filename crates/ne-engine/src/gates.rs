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
use serde_json::Value;
// 词表统一取自 SSOT（ne_core::lexicon）；此处以**切片视图**导出，保持下游 `&[&str]` 类型不变。
use ne_core::lexicon as lex;
pub const MYTH_BANNED: &[&str] = &lex::MYTH_BANNED;
pub const LEAK_WORDS: &[&str] = &lex::LEAK_WORDS;

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
/// G13 确定性 4 维的禁词（＝ SSOT 正典禁词）。
pub const G13_STOP_WORDS: &[&str] = &lex::BANNED_WORDS;

/// G17 设计层/工程层词泄漏（＝ SSOT 探测集；卷01-03 生效）。
/// 仅作**字面**检测；语义层（如"算的哪门子账"）无法被覆盖。
pub const G17_DESIGN_LEAK_WORDS: &[&str] = &lex::DESIGN_LEAK_WORDS;
/// G13 AI 词（＝ SSOT AI 痕词）。
pub const G13_AI_WORDS: &[&str] = &lex::AI_TRACE_WORDS;
/// 开篇断言 B：金手指亮相（g01）。
///
/// ⚠ 只允许用**体感词**作判据——「金手指亮相」= 那股说不清的异样体感出现。
/// 严禁再用「推演/面板/锚点/刻痕」等设计层工程词：那等于命令首章把工程词写进正文
/// （与 STYLE_CONSTRAINTS §0 禁词表正面冲突）。2026-10-15 修正。
pub const G01_B: &[&str] = &["体感", "一下", "劲儿", "说不清", "抓不住", "异样", "脑子"];
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
// ───────────────────────────── 书档案默认值（通用化回退）
/// 默认书名（《残响之河》；新书在 `book_profile.title` 覆盖）。
pub const BOOK_TITLE_DEFAULT: &str = "残响之河";
/// 默认主角名（陈寂；新书在 `book_profile.protagonist` 覆盖）。
pub const PROTAGONIST_DEFAULT: &str = "陈寂";

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
    /// 本章登场角色状态（供 G07 OOC 真跑）：[{char_code,char_name,alive,realm,_realm_max,suppress,emotions{}}]
    /// 默认空：未接线的实现按 skip_no_input 降级。
    fn char_states_for_ooc(&self, _volume: &str, _chapter: u32) -> Vec<Value> {
        Vec::new()
    }
    /// event_template 全表（供 G07 判据3）：{EVT-ID: {emotion_delta:{}, ...}}
    fn event_templates_map(&self) -> Option<Value> {
        None
    }
    /// kb_world_consistency（供 G07 判据5）：{drift_blacklist:[], canon_invariants:[]}
    fn kb_consistency(&self) -> Option<Value> {
        None
    }
    /// 库规模统计（供 G13 离线维度 D19~D36）：{类别: 数量}
    fn library_counts(&self) -> std::collections::BTreeMap<String, i64> {
        Default::default()
    }
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
pub fn g01_opening4(text: &str, chapter: u32, cfg: &Config) -> GateResult {
    let mut out = GateResult::ok("");
    if chapter > 3 {
        out.detail("非开篇章（ch>3），G01 不适用，pass");
        return out;
    }
    let bp = &cfg.book_profile;
    let a = text.contains(bp.protagonist());
    let g01_b = bp.g01_b();
    let g01_c = bp.g01_c();
    let g01_d = bp.g01_d();
    let b = g01_b.iter().any(|k| text.contains(k.as_str()));
    let c = g01_c.iter().any(|k| text.contains(k.as_str()));
    let d = g01_d.iter().any(|k| text.contains(k.as_str()));
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
            format!("4~5 章内补 C 命中（{} 任一）", g01_c.join("/")),
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
pub fn g04_ability_cost(text: &str, _chapter: u32, cfg: &Config) -> GateResult {
    let bp = &cfg.book_profile;
    let ability_kw = bp.ability_keywords();
    let cost_kw = bp.cost_keywords();
    let ability = count_hits(text, &ability_kw.iter().map(|s| s.as_str()).collect::<Vec<_>>());
    let cost = count_hits(text, &cost_kw.iter().map(|s| s.as_str()).collect::<Vec<_>>());
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
pub fn g05_pov_consistent(text: &str, _chapter: u32, cfg: &Config) -> GateResult {
    let prot = cfg.book_profile.protagonist();
    if text.contains(prot) {
        return GateResult::ok(format!("主角{prot}视角在场"));
    }
    let mut out = GateResult::ok(format!("{prot}0命中，视角锁定失守"));
    out.p0(
        "G05_pov_consistent",
        format!("正文无主角{prot}（技06 视角锁定：非{prot}视角章打回）"),
        format!("视角回{prot}，或删除本章/改非正文场景"),
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
/// G09 信息密度：**只测「说明词密度」与「章长硬上限」**，不以 cap 封顶总字数。
///
/// 语义修正（2026-10-11）：旧实现把 `rhythm.info_density_max`(=1000) 当作**总字数**上限，
/// `字数 > cap×1.5` 即报 P1（≈ 总字数封顶 1500）。这与该值本义（「每实质剧情点字数」的
/// 代理刻度，见 `dynamics_config.json` 注释）不符，且与 G13 的 `chapter_word_min`(=2500)
/// **直接互斥**——1500 < 2500，任何章节都无法同时过 G09 与 G13（实测：1492 字过 G09 挂
/// G13，2528 字过 G13 挂 G09）。
/// 现改为：`cap` 仅作说明词密度的参考刻度上报；字数只受 `gates.chapter_word_max`(4000)
/// 硬上限约束，超出 → P0。信息密度（说明词占比 ratio）继续测量并上报，P1 阈值待 L4 正文
/// 回读标定后再接（当前不触发）。
pub fn g09_info_density(text: &str, _chapter: u32, cfg: &Config) -> GateResult {
    let cap = cfg.info_density_max() as usize;
    let w = words(text);
    let expl = ["即", "又称", "所谓", "设定", "规则"]
        .iter()
        .map(|k| text.matches(k).count())
        .sum::<usize>();
    let ratio = expl as f64 / w.max(1) as f64;
    let mut out = GateResult::ok(format!(
        "字数={w} 说明词密度={:.4}（cap={cap} 为密度参考刻度，非字数上限）",
        ratio
    ));
    let word_max = cfg.word_max() as usize;
    if w > word_max {
        out.p0(
            "G09_info_density",
            format!("字数{w}>硬上限{word_max}"),
            "砍字/分章（gates.chapter_word_max）",
        );
    }
    out
}

// ───────────────────────────── G10
pub fn g10_horror_chapter(
    text: &str,
    chapter: u32,
    volume: &str,
    cfg: &Config,
    ctx: &dyn GateCtx,
) -> GateResult {
    let scene = ctx.scene_of(volume, chapter);
    let hk = cfg.book_profile.horror_keywords();
    let is_horror = scene == "心理" || hk.iter().any(|k| text.contains(k.as_str()));
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
pub fn g13_scoring_67dim(text: &str, volume: &str, cfg: &Config, ctx: &dyn GateCtx) -> GateResult {
    let dims_total = ctx.scoring_dims_total();
    let w = words(text);
    let det: Vec<(&str, bool)> = vec![
        (
            "字数",
            cfg.word_min_for_volume(volume) as usize <= w && w <= cfg.word_max() as usize,
        ),
        ("禁词", !cfg.book_profile.banned_words().iter().any(|k| text.contains(k.as_str()))),
        ("AI词", !G13_AI_WORDS.iter().any(|k| text.contains(*k))),
        (
            "感叹号",
            (text.matches('!').count() + text.matches('！').count()) <= 1,
        ),
    ];
    let bad: Vec<&str> = det.iter().filter(|(_, ok)| !ok).map(|(n, _)| *n).collect();
    // D19~D28 库规模维度（离线可算）
    let counts = ctx.library_counts();
    let targets: [(&str, &str, i64); 7] = [
        ("文明数量达标", "文明", 30),
        ("人物数量达标", "角色", 249),
        ("地点数量达标", "地点", 146),
        ("事件数量达标", "事件", 156),
        ("功法数量达标", "功法", 1),
        ("科技数量达标", "科技", 1),
        ("物品数量达标", "物品", 1),
    ];
    let mut scale_ok = 0usize;
    let mut scale_miss: Vec<String> = Vec::new();
    for (label, key, target) in targets {
        let n = *counts.get(key).unwrap_or(&0);
        if n >= target { scale_ok += 1; } else { scale_miss.push(format!("{label}({n}/{target})")); }
    }
    let total_cards = *counts.get("_总卡").unwrap_or(&0);
    let used_cards = *counts.get("_已引用").unwrap_or(&0);
    let coverage = if total_cards > 0 { used_cards * 100 / total_cards } else { 0 };
    let relations_ok = coverage >= 50;
    let offline_scored = 4 + targets.len() + 2;
    let mut out = GateResult::ok(format!(
        "67维：离线 {offline_scored} 维（4确定性 + {}规模 + 关系覆盖{coverage}% {}），剩{}维需LLM",
        scale_ok,
        if relations_ok {"ok"} else {"miss"},
        dims_total.saturating_sub(offline_scored as i64)
    ));
    if !bad.is_empty() {
        out.p1("G13_scoring_67dim", format!("确定性维度不达标：{bad:?}"), "按对应维度修复");
    }
    if !scale_miss.is_empty() {
        out.p1("G13_scoring_67dim", format!("库规模未达标：{}", scale_miss.join("，")), "补齐资料卡；不阻断正文");
    }
    out
}

// ───────────────────────────── G14
pub fn g14_world_consistency(text: &str, cfg: &Config, ctx: &dyn GateCtx) -> GateResult {
    let mut out = GateResult { pass: true, detail: Some(String::new()), p0: vec![], p1: vec![] };
    let myth_kw = cfg.book_profile.myth_banned();
    let myth_ref: Vec<&str> = myth_kw.iter().map(|s| s.as_str()).collect();
    let myth = hits(text, &myth_ref);
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
pub fn g15_family_consistency(text: &str, cfg: &Config) -> GateResult {
    let mut out = GateResult::ok("");
    let mut hit = 0usize;
    let canon_family = cfg.book_profile.canon_family();
    let canon_names = cfg.book_profile.canon_names();
    let canon_desc = canon_family
        .iter()
        .map(|(t, c)| format!("{t}={c}"))
        .collect::<Vec<_>>()
        .join("/");
    let re_str = canon_family
        .iter()
        .map(|(t, _)| regex::escape(t))
        .collect::<Vec<_>>()
        .join("|");
    let re = regex::Regex::new(&format!(r"(?:{re_str})(?:的|叫)?([\u{{4e00}}-\u{{9fa5}}]{{2,4}})")).unwrap();
    for caps in re.captures_iter(text) {
        let whole = caps.get(0).unwrap().as_str();
        let mentioned = caps.get(1).unwrap().as_str();
        // 找出命中的称谓 → 期望 canon 名
        let title = canon_family
            .iter()
            .find(|(t, _)| whole.starts_with(t.as_str()))
            .map(|(t, c)| (t.as_str(), c.as_str()));
        let Some((title, canon_name)) = title else { continue };
        // 合法：mentioned 的 2/3/4 字前缀 ∈ CANON_NAMES
        let matched = (2..=4).any(|n| {
            if mentioned.chars().count() < n {
                return false;
            }
            let prefix: String = mentioned.chars().take(n).collect();
            canon_names.iter().any(|cn| cn == &prefix)
        });
        if matched {
            continue;
        }
        hit += 1;
        out.p1(
            "G15_family_consistency",
            format!("称谓「{title}」提及「{mentioned}」≠canon「{canon_name}」（家庭定版·后续不得改）"),
            format!("按 canon 改回：{canon_desc}"),
        );
    }
    out.pass = hit == 0;
    out.detail(format!(
        "家庭canon错配={hit}处（{canon_desc}，后续不得改）"
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

// ───────────────────────────── G17
/// G17 设计层/工程层词泄漏：卷01-03 正文严禁出现漏斗工程代号。
///
/// 卷01-03 = 「严禁输出该文字」阶段（见全局铁则 & STYLE_CONSTRAINTS §0），
/// 故命中即 **P0 打回**；卷04 起降级为 P1（此时认知曲线可逐步显性）。
pub fn g17_design_leak(text: &str, volume: &str) -> GateResult {
    // 2026-10-16：改用 lexicon::design_leak_hits（扣除正典专名豁免，避免误伤推演院/未来推演）。
    let h = lex::design_leak_hits(text);
    let mut out = GateResult::ok(format!("设计层词命中={}", fmt_hits(&h)));
    if h.is_empty() {
        return out;
    }
    // 卷号解析：卷01-03 → P0
    let vnum: u32 = volume
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .unwrap_or(0);
    let early = vnum >= 1 && vnum <= 3;
    for (w, n) in h {
        let (sev_issue, action) = if early {
            (
                format!("设计层词「{w}」×{n} 泄漏进正文（卷01-03 严禁：上帝视角下渗）"),
                "删除；改写成纯体感（流/噪/沙沙/时隐时现/隔一层膜/抓不住），角色此刻不许命名或类比其本质",
            )
        } else {
            (
                format!("设计层词「{w}」×{n} 出现在正文（卷04+ 允许渐显，请确认符合认知曲线）"),
                "复核：该词是否为角色经推导后的自主归纳，而非上帝视角预设",
            )
        };
        if early {
            out.p0("G17_design_leak", sev_issue, action);
        } else {
            out.p1("G17_design_leak", sev_issue, action);
        }
    }
    out
}

// ───────────────────────────── G07 / G08（需 DB 输入）
/// G07 OOC：简化口径——角色已死/境界倒退/情绪矛盾/压抑越界（4 判据）。
/// 无角色状态数据 → skip_no_input（不阻断），与 Python 同口径。
pub fn g07_ooc(text: &str, volume: &str, chapter: u32, ctx: &dyn GateCtx) -> GateResult {
    let states = ctx.char_states_for_ooc(volume, chapter);
    if states.is_empty() {
        return GateResult::ok("本章无角色状态数据，G07 skip_no_input 不阻断");
    }
    let ev_tpl = ctx.event_templates_map();
    let kb = ctx.kb_consistency();
    let ev_ref = ev_tpl.as_ref();
    // action_desc：取正文首 200 字作“本章情节摘要”（判据1/2 仅用它做证据文本）
    let action: String = text.chars().take(200).collect();
    let mut out = GateResult::ok("");
    let mut checked = 0usize;
    for st in &states {
        let findings = crate::ooc_gate::ooc_gate(
            &action,
            st,
            ev_ref,
            crate::ooc_gate::CONFLICT_THR,
            Some(text),
            kb.as_ref(),
        );
        checked += 1;
        for f in findings {
            let sev = f.severity.as_deref().unwrap_or("P1");
            let msg = format!("{} [{}] {}", f.code, f.role, f.evidence);
            if sev == "P0" {
                out.p0("G07_OOC", msg, "修正角色行为/状态，与情绪引擎/世界设定对齐");
            } else {
                out.p1("G07_OOC", msg, "复核角色行为是否与既定设定冲突");
            }
        }
    }
    if out.p0.is_empty() && out.p1.is_empty() {
        out = GateResult::ok(format!("OOC 5 判据真跑，{checked} 角色全部通过"));
    }
    out
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
    "G17_design_leak",
    "P1_PARAGRAPH",
];

#[derive(Debug, Clone, Serialize)]
pub struct GateReport {
    pub verdict: String,
    pub p0: Vec<Finding>,
    pub p1: Vec<Finding>,
    pub gates: std::collections::BTreeMap<String, GateBrief>,
    pub stats: GateStats,
    /// P1-PARAGRAPH 修复后的全文（仅排版变化；无改动时为 None）。
    /// 调用方（novel 落盘后）据此回写文件 —— 只改分段，一字不改。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixed_text: Option<String>,
}

// ───────────────────────────── P1-PARAGRAPH（修复型，永不 P0）
/// 段落格式门禁：段 40~80 汉字，超长按句末标点切分、过短与邻段合并。
/// 逗:句 比值仅统计告警。**只调排版，一字不改**；绝不返回 P0。
pub fn p1_paragraph(text: &str) -> (GateResult, Option<String>) {
    let fx = crate::paragraph_gate::fix_paragraphs(text);
    let mut out = GateResult::ok(format!(
        "段落修复={}处；[逗号:{} / 句号:{}，比值:{:.2}]",
        fx.fixed_count, fx.comma, fx.period, fx.ratio
    ));
    if fx.fixed_count > 0 {
        out.p1(
            "P1_PARAGRAPH",
            format!("P1-PARAGRAPH_FIX 修改段落数：{}", fx.fixed_count),
            "段落 40~80 汉字自动合并/切分（仅排版，文字未改）",
        );
    }
    if !(1.7..=2.8).contains(&fx.ratio) {
        out.p1(
            "P1_PARAGRAPH",
            "P1-PARAGRAPH_WARN 逗号句号比值超出参考区间，建议人工复核文笔节奏",
            "仅告警：不在程序层改写文字",
        );
    }
    let fixed = if fx.fixed_count > 0 { Some(fx.fixed) } else { None };
    (out, fixed)
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
    run!("G05_pov_consistent", g05_pov_consistent(text, chapter, cfg));
    run!("G06_character_consistency", g06_character_consistency(text, ctx));
    run!("G07_OOC", g07_ooc(text, volume, chapter, ctx));
    run!("G08_foreshadow_recover", g08_foreshadow_recover(text, chapter, volume, ctx));
    run!("G09_info_density", g09_info_density(text, chapter, cfg));
    run!("G10_horror_chapter", g10_horror_chapter(text, chapter, volume, cfg, ctx));
    run!("G11_vol9_reversal", g11_vol9_reversal(text, volume));
    run!("G12_naming_tier", g12_naming_tier(text, volume, cfg));
    run!("G13_scoring_67dim", g13_scoring_67dim(text, volume, cfg, ctx));
    run!("G14_world_consistency", g14_world_consistency(text, cfg, ctx));
    run!("G15_family_consistency", g15_family_consistency(text, cfg));
    run!("G16_leak_words", g16_leak_words(text));
    run!("G17_design_leak", g17_design_leak(text, volume));

    // P1-PARAGRAPH：段落格式修复型门禁（只产 P1，永不 P0）
    let (para_res, fixed_text) = p1_paragraph(text);
    run!("P1_PARAGRAPH", para_res);

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
        stats: GateStats { words: words(text), gates_run: 17 },
        fixed_text,
    }
}

#[cfg(test)]
mod tests_g09_semantics {
    use super::*;

    /// 回归：G09 不得把 `info_density_max`(cap) 当总字数上限。
    /// 语义修正 2026-10-11：G09=信息密度 ≠ 总字数封顶；字数只受 word_max 硬上限约束。
    /// 否则 G09(≤cap×1.5=1500) 与 G13(≥word_min=2500) 互斥，任何章节无法 pass。
    #[test]
    fn g09_does_not_cap_total_words_at_cap() {
        let cfg = Config::default(); // cap=1000, word_max=4000
        // 2000 字（>cap×1.5=1500，<word_max=4000）：旧实现报 P1，现应 pass
        let text: String = std::iter::repeat('字').take(2000).collect();
        let r = g09_info_density(&text, 1, &cfg);
        assert!(r.pass, "2000 字应过 G09（cap 不再当字数上限）: {:?}", r.p1);
        assert!(r.p1.is_empty(), "不应再有 cap×1.5 字数 P1: {:?}", r.p1);
    }

    /// 超过 word_max(4000) 仍须 P0 拦截。
    #[test]
    fn g09_still_p0_over_hard_max() {
        let cfg = Config::default();
        let text: String = std::iter::repeat('字').take(4001).collect();
        let r = g09_info_density(&text, 1, &cfg);
        assert!(!r.pass, ">word_max 必须 P0");
        assert_eq!(r.p0.len(), 1);
    }

    // ───────────────────────── 通用化回归：新书不得被《残响之河》硬编码打回

    fn cfg_new_book() -> Config {
        let mut c = Config::default();
        c.book_profile = serde_json::from_value(serde_json::json!({
            "title": "星尘渡口",
            "protagonist": "林知远",
            "myth_banned": ["星神本源"],
            "canon_family": [["父亲", "林崇山"]],
            "canon_names": ["林崇山"],
            "banned_words": ["灵气"]
        }))
        .unwrap();
        c
    }

    /// 新书主角名出现即过 G05；旧库默认仍认陈寂。
    #[test]
    fn g05_uses_configured_protagonist() {
        let nbc = cfg_new_book();
        let r = g05_pov_consistent("林知远走进渡口。", 1, &nbc);
        assert!(r.pass, "新书主角应过 G05: {:?}", r.p0);
        // 同一段文本在默认（残响之河）配置下应被打回
        let old = Config::default();
        let r2 = g05_pov_consistent("林知远走进渡口。", 1, &old);
        assert!(!r2.pass, "默认配置下非陈寂主角应 P0");
    }

    /// G05 提示文案含本书主角名，不泄漏「陈寂」。
    #[test]
    fn g05_message_has_no_canxiang_leak() {
        let nbc = cfg_new_book();
        let r = g05_pov_consistent("无关正文。", 1, &nbc);
        let blob = format!("{:?}", r.p0) + &r.detail.clone().unwrap_or_default();
        assert!(blob.contains("林知远"));
        assert!(!blob.contains("陈寂"), "不得泄漏旧书主角名: {blob}");
    }

    /// G15 家庭 canon 用本书设定；提示文案不泄漏旧书家族名。
    #[test]
    fn g15_uses_configured_family() {
        let nbc = cfg_new_book();
        let r = g15_family_consistency("父亲的姓名是王大力。", &nbc);
        let blob = format!("{:?}", r.p1) + &r.detail.clone().unwrap_or_default();
        assert!(!blob.contains("陈大山"), "不得泄漏旧书家族: {blob}");
        assert!(blob.contains("林崇山"), "应给出本书 canon: {blob}");
    }

    /// G01 金手指/矛盾/身份断言用本书词表。
    #[test]
    fn g01_uses_configured_assertion_words() {
        let mut c = cfg_new_book();
        c.book_profile.g01_b = Some(vec!["星纹".into()]);
        c.book_profile.g01_c = Some(vec!["星蚀".into()]);
        c.book_profile.g01_d = Some(vec!["前世".into()]);
        let text = "林知远掌心浮现星纹，他想起前世的星蚀。";
        let r = g01_opening4(text, 1, &c);
        assert!(r.pass, "四断言齐备应过 G01: {:?}", r.p0);
    }

    /// G14 神话禁词用本书词表：本书禁词命中应报，旧书词不误伤。
    #[test]
    fn g14_uses_configured_myth_banned() {
        struct Bare;
        impl GateCtx for Bare {
            fn scene_of(&self, _v: &str, _c: u32) -> String { "未知".into() }
            fn prev_chapters_text(&self, _v: &str, _c: u32, _w: u32) -> String { String::new() }
            fn due_foreshadows(&self, _v: &str, _c: u32) -> Vec<(String, String)> { vec![] }
            fn catchphrases(&self) -> Vec<String> { vec![] }
            fn drift_terms(&self) -> Vec<String> { vec![] }
            fn scoring_dims_total(&self) -> i64 { 0 }
        }
        let nbc = cfg_new_book();
        let r = g14_world_consistency("提到星神本源这个词。", &nbc, &Bare);
        let blob = format!("{:?}", r.p1);
        assert!(blob.contains("星神本源"), "本书禁词应命中: {blob}");
    }

    /// G17：卷01-03 设计层词命中 → P0（上帝视角下渗防线的字面闸门）。
    #[test]
    fn g17_design_leak_p0_in_early_volumes() {
        let r = g17_design_leak("那股味儿有点像推演算账时候的模糊感觉。", "卷01");
        assert!(!r.pass, "卷01 出现工程词必须打回");
        assert!(!r.p0.is_empty(), "应有 P0");
        assert!(format!("{:?}", r.p0).contains("推演"));
    }

    /// G17：干净体感文本不误伤。
    #[test]
    fn g17_clean_sensory_passes() {
        let r = g17_design_leak("颅内一阵说不清的体感，像细碎的东西在淌，抓不住，一闪就散了。", "卷01");
        assert!(r.pass, "纯体感应过 G17: {:?}", r.p0);
    }

    /// G17：卷04+ 降级为 P1（认知曲线允许渐显）。
    #[test]
    fn g17_downgraded_after_volume3() {
        let r = g17_design_leak("他终于确信，这东西在推演。", "卷04");
        assert!(r.pass, "卷04 不再 P0");
        assert_eq!(r.p1.len(), 1);
    }

    /// G01_B 不得再用工程词（否则首章会被门禁"鼓励"写入禁词）。
    #[test]
    fn g01_b_has_no_design_words() {
        for w in G01_B {
            assert!(
                !G17_DESIGN_LEAK_WORDS.contains(w),
                "G01_B 含设计层词「{w}」，与 STYLE_CONSTRAINTS §0 冲突"
            );
        }
    }
}
