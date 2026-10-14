//! L5 质检门禁 —— `gate14.py` 的 Rust 忠实移植（**G01–G16 共 16 道**，0 LLM / 0 随机）。
//!
//! 事实源：`05_管线框架/engine/gate14.py`（661 行）
//!
//! ⚠ 文件名虽叫 gate14，实际 `GATE_ORDER` 含 **16 项**（G15 家庭一致性 / G16 指令泄漏词
//! 是 2026-10-03 追加）。本模块如实实现 16 项，不用 stub。
//!
//! 挂载口径（照抄）：`deterministic_check` 之后追加 run_16，取 verdict=reject → 打回不入库。
//!
//! 需外部数据的门禁（G03/G06/G08/G10/G14）走 `GateCtx` trait，模块内零盘 IO / 零 DB。

use crate::ooc_gate::{self, DIM_NAMES};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

// ───────────────────────── 常量（逐字照抄 gate14.py）

/// G03 跨章爽点滚动窗口。
pub const PREV_WINDOW: usize = 5;
/// G10 恐怖章关键词（Y1-Y4 联动）。
pub const HORROR_KEYWORDS: [&str; 10] = [
    "残响", "异常", "诡异", "信息寄生", "认知侵蚀", "叠加", "退相", "残响之河", "折流", "锚点异变",
];
/// G04 能力动用关键词（W4/T2/R3）。
pub const ABILITY_KEYWORDS: [&str; 8] =
    ["修为", "逆流转", "水相", "熵流体", "刻痕", "共鸣", "境界突破", "突破"];
/// G04 代价关键词。
pub const COST_KEYWORDS: [&str; 17] = [
    "代价", "失去", "模糊", "遗忘", "消散", "消失", "消失的", "变淡", "淡去", "疼", "痛", "灼",
    "烧", "裂痕", "碎裂", "代价", "忘",
];
/// G14 来源神话 6 词硬闸。
pub const MYTH_BANNED: [&str; 6] = [
    "存在之基", "存在之核", "高维观测者文明", "高维观测者", "阿莱夫·零", "金色立方体",
];
/// G16 指令泄漏词（阻断级）。
pub const LEAK_WORDS: [&str; 8] =
    ["不解释", "不展开", "不说明", "不交代", "不给出", "不复述", "不得杜撰", "不硬凑"];
/// G15 家庭 canon 称谓映射。
pub const FAMILY_CANON: [(&str, &str); 3] =
    [("父亲", "陈大山"), ("母亲", "林氏"), ("弟弟", "陈默")];
const CANON_NAMES: [&str; 4] = ["陈大山", "林氏", "陈默", "陈老头"];

/// 门禁顺序（照抄 GATE_ORDER）。
pub const GATE_ORDER: [&str; 16] = [
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

// ───────────────────────── 配置（解析 dynamics_config.json）

/// 从 `dynamics_config.json` 抽出的门禁所需字段。
#[derive(Debug, Clone, Default)]
pub struct GateCfg {
    pub hook_min_per_chapter: usize,
    pub info_density_max: usize,
    pub chapter_word_max: usize,
    /// banned_by_tier.U0-1.banned_words
    pub banned_u01: Vec<String>,
    pub scoring_excellent: f64,
    pub scoring_superior: f64,
    /// 书专属档案（书名/主角/禁词/家庭 canon…）。
    /// 默认 = 《残响之河》历史常量，行为不变；新书由调用方注入。
    pub book_profile: crate::config::BookProfile,
}

impl GateCfg {
    /// 从 JSON 解析；缺字段用 Python 默认值。
    pub fn from_json(v: &Value) -> Self {
        let g = |path: &[&str]| -> Option<&Value> {
            let mut cur = v;
            for k in path {
                cur = cur.get(*k)?;
            }
            Some(cur)
        };
        GateCfg {
            hook_min_per_chapter: g(&["rhythm", "hook_density", "min_per_chapter"])
                .and_then(|x| x.as_u64())
                .unwrap_or(1) as usize,
            info_density_max: g(&["rhythm", "info_density_max"])
                .and_then(|x| x.as_u64())
                .unwrap_or(1000) as usize,
            chapter_word_max: g(&["gates", "chapter_word_max"])
                .and_then(|x| x.as_u64())
                .unwrap_or(4000) as usize,
            banned_u01: g(&["banned_by_tier", "U0-1", "banned_words"])
                .and_then(|x| x.as_array())
                .map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect())
                .unwrap_or_default(),
            scoring_excellent: g(&["gates", "scoring_lines", "excellent"])
                .and_then(|x| x.as_f64())
                .unwrap_or(95.0),
            scoring_superior: g(&["gates", "scoring_lines", "superior"])
                .and_then(|x| x.as_f64())
                .unwrap_or(98.0),
            book_profile: v
                .get("book_profile")
                .and_then(|bp| serde_json::from_value::<crate::config::BookProfile>(bp.clone()).ok())
                .unwrap_or_default(),
        }
    }
}

/// 到期伏笔（G08 用）。
#[derive(Debug, Clone)]
pub struct DueForeshadow {
    pub id: String,
    pub content: String,
}

/// 外部数据源。
pub trait GateCtx {
    fn cfg(&self) -> &GateCfg;
    /// G03：前序 1..PREV_WINDOW 章正文拼接（无 → 空串 ⇒ skip）
    fn prev_chapters_text(&self, volume: &str, chapter: u32) -> String;
    /// G06：角色卡的 口头禅 字段（(code, catchphrase)）；空 ⇒ skip
    fn role_catchphrases(&self) -> Vec<(String, String)>;
    /// G08：本章到期未回收伏笔；空 ⇒ skip
    fn due_foreshadows(&self, volume: &str, chapter: u32) -> Vec<DueForeshadow>;
    /// G10：本章场景（infer_scene 结果）
    fn scene_of(&self, volume: &str, chapter: u32) -> String;
    /// G07/G14：kb_world_consistency.json 内容
    fn kb(&self) -> Option<Value>;
    /// G14 兜底：从 kb_knowledge 表提取的 drift 词
    fn kb_drift_terms_from_db(&self) -> Vec<String>;
    /// G13：scoring_dimensions 表行数（0 表示不可用）
    fn scoring_dim_count(&self) -> usize;
}

// ───────────────────────── 输出类型

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateFinding {
    pub gate: String,
    pub issue: String,
    pub action: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateOut {
    pub pass: bool,
    #[serde(default)]
    pub p0: Vec<GateFinding>,
    #[serde(default)]
    pub p1: Vec<GateFinding>,
    pub detail: String,
}

impl GateOut {
    fn pass(detail: impl Into<String>) -> Self {
        GateOut { pass: true, p0: vec![], p1: vec![], detail: detail.into() }
    }
    fn fail_p0(&mut self) {
        self.pass = false;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateRun {
    pub verdict: String,
    pub p0: Vec<GateFinding>,
    pub p1: Vec<GateFinding>,
    pub gates: BTreeMap<String, GateBrief>,
    pub stats: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateBrief {
    pub pass: bool,
    pub detail: String,
}

fn words(text: &str) -> usize {
    text.chars().filter(|c| !c.is_whitespace()).count()
}

/// 末 n 个字符（对齐 Python `text[-300:]` 的按字符切片）。
fn tail_chars(text: &str, n: usize) -> String {
    let v: Vec<char> = text.chars().collect();
    let start = v.len().saturating_sub(n);
    v[start..].iter().collect()
}

fn mk(gate: &str, issue: String, action: &str) -> GateFinding {
    GateFinding { gate: gate.into(), issue, action: action.into() }
}

// ───────────────────────── G01 开篇四断言

pub fn g01_opening4(text: &str, chapter_no: u32, cfg: &GateCfg) -> GateOut {
    let mut out = GateOut {
        pass: true,
        p0: vec![],
        p1: vec![],
        detail: String::new(),
    };
    if chapter_no > 3 {
        return GateOut::pass("非开篇章（ch>3），G01 不适用，pass");
    }
    let bp = &cfg.book_profile;
    let g01_b = bp.g01_b();
    let g01_c = bp.g01_c();
    let g01_d = bp.g01_d();
    let a = text.contains(bp.protagonist());
    let b = g01_b.iter().any(|k| text.contains(k.as_str()));
    let c = g01_c.iter().any(|k| text.contains(k.as_str()));
    let d = g01_d.iter().any(|k| text.contains(k.as_str()));

    let hard_missing: Vec<&str> = [("A主角出场", a), ("B金手指亮相", b), ("D身份交代", d)]
        .iter()
        .filter(|(_, ok)| !*ok)
        .map(|(n, _)| *n)
        .collect();
    let c_missing = !c;

    out.detail = format!("开篇断言 A命中={a} B命中={b} C命中={c} D命中={d}");
    if hard_missing.iter().any(|_| true) || c_missing {
        let miss: Vec<&str> = [("A主角出场", a), ("B金手指亮相", b), ("C核心矛盾", c), ("D身份交代", d)]
            .iter()
            .filter(|(_, ok)| !*ok)
            .map(|(n, _)| *n)
            .collect();
        out.detail.push_str(&format!("；缺{miss:?}"));
    }
    if !hard_missing.is_empty() {
        out.fail_p0();
        out.p0.push(mk(
            "G01_opening4",
            format!("开篇硬性断言缺 {hard_missing:?}"),
            "补对应要素进前500字（黄金三章铁律）",
        ));
    }
    // C 核心矛盾：黄金五章内须命中，降 P1 观察项
    if c_missing && chapter_no <= 5 {
        out.p1.push(mk(
            "G01_opening4",
            "C核心矛盾（黄金五章观察项，逐章不强制）".into(),
            "4~5 章内补 C 命中（残响/代价/遗忘 任一）",
        ));
    }
    out
}

// ───────────────────────── G02 章末钩密度

pub fn g02_hook_density(text: &str, cfg: &GateCfg) -> GateOut {
    let mut out = GateOut { pass: true, p0: vec![], p1: vec![], detail: String::new() };
    let min_n = cfg.hook_min_per_chapter;
    let tail = tail_chars(text, 300);
    let hooks = ["？", "？。", "…", "……", "到底", "会不会", "难道", "如果", "可是", "却", "但是"]
        .iter()
        .filter(|kw| tail.contains(**kw))
        .count();
    out.detail = format!("章末300字钩子标记命中={hooks}（min_per_chapter={min_n}）");
    if hooks < min_n {
        out.fail_p0();
        out.p1.push(mk(
            "G02_hook_density",
            format!("章末钩密度不足（命中{hooks}<{min_n}）"),
            "章末200字内加≥1未闭合钩子（疑点/悬念词）",
        ));
    }
    out
}

// ───────────────────────── G03 爽点间隔（跨章）

pub fn g03_shuang_interval<C: GateCtx>(ctx: &C, text: &str, chapter_no: u32, volume: &str) -> GateOut {
    let mut out = GateOut { pass: true, p0: vec![], p1: vec![], detail: String::new() };
    let prev = ctx.prev_chapters_text(volume, chapter_no);
    if prev.is_empty() {
        return GateOut::pass("无已入库前序正文（04_正文），G03 跨章爽点间隔 skip_no_input");
    }
    let all = format!("{prev}{text}");
    let mini: usize = ["顿挫", "转折", "转机", "突破"].iter().map(|k| all.matches(*k).count()).sum();
    let major: usize = ["高潮", "逆转", "绝地", "翻盘", "碾压"].iter().map(|k| all.matches(*k).count()).sum();
    out.detail = format!("滚动{PREV_WINDOW}章小爽点={mini} 大爽点={major}（R4 滚动5章0大爽点→p0）");
    if major == 0 {
        out.fail_p0();
        out.p0.push(mk(
            "G03_shuang_interval",
            format!("滚动{PREV_WINDOW}章0大爽点"),
            "本章补≥1大爽点（高潮/逆转类节拍）",
        ));
    } else if mini < PREV_WINDOW {
        out.p1.push(mk(
            "G03_shuang_interval",
            format!("小爽点密度偏低（{mini}）"),
            "补小爽点节拍（顿挫/转机）",
        ));
    }
    out
}

// ───────────────────────── G04 能力代价

pub fn g04_ability_cost(text: &str, cfg: &GateCfg) -> GateOut {
    let mut out = GateOut { pass: true, p0: vec![], p1: vec![], detail: String::new() };
    let ability = cfg.book_profile.ability_keywords();
    let cost = cfg.book_profile.cost_keywords();
    let ability_hit = ability.iter().filter(|k| text.contains(k.as_str())).count();
    let cost_hit = cost.iter().filter(|k| text.contains(k.as_str())).count();
    out.detail = format!("能力动用词={ability_hit} 代价词={cost_hit}");
    if ability_hit > 0 && cost_hit == 0 {
        out.fail_p0();
        out.p1.push(mk(
            "G04_ability_cost",
            "能力动用章无代价描写（W4/T2/R3）".into(),
            "补代价闭环（失去/模糊/消散类描写，R3 代价闭环）",
        ));
    }
    out
}

// ───────────────────────── G05 视角一致

pub fn g05_pov_consistent(text: &str, cfg: &GateCfg) -> GateOut {
    let prot = cfg.book_profile.protagonist();
    let mut out = GateOut { pass: true, p0: vec![], p1: vec![], detail: String::new() };
    if text.contains(prot) {
        return GateOut::pass(format!("主角{prot}视角在场"));
    }
    out.fail_p0();
    out.p0.push(mk(
        "G05_pov_consistent",
        format!("正文无主角{prot}（技06 视角锁定：非{prot}视角章打回）"),
        &format!("视角回{prot}，或删除本章/改非正文场景"),
    ));
    out.detail = format!("{prot}0命中，视角锁定失守");
    out
}

// ───────────────────────── G06 人设一致（需 DB）

pub fn g06_character_consistency<C: GateCtx>(ctx: &C, text: &str) -> GateOut {
    let mut out = GateOut { pass: true, p0: vec![], p1: vec![], detail: String::new() };
    let rows = ctx.role_catchphrases();
    let with_catch: Vec<&(String, String)> =
        rows.iter().filter(|(_, c)| !c.is_empty()).collect();
    if with_catch.is_empty() {
        return GateOut::pass("角色性格卡0口头禅字段（P4/P6 无判定输入），skip_no_input");
    }
    for (code, catch) in with_catch {
        if text.contains(catch.as_str()) {
            return GateOut::pass(format!("角色{code} 口头禅命中"));
        }
    }
    out.detail = format!("检查{}张角色卡口头禅，均未命中", rows.len());
    out
}

// ───────────────────────── G07 OOC

pub fn g07_ooc<C: GateCtx>(ctx: &C, text: &str, chapter_no: u32) -> GateOut {
    let mut out = GateOut { pass: true, p0: vec![], p1: vec![], detail: String::new() };
    // 照抄 Python：char_state 用固定默认值构造（0 编造）
    let emotions: Map<String, Value> =
        DIM_NAMES.iter().map(|d| (d.to_string(), json!(20.0))).collect();
    let char_state = json!({
        "char_code": "CHAR-99-001",
        "char_name": "陈寂",
        "chapter": chapter_no,
        "alive": true,
        "realm": "共鸣境",
        "_realm_max": "共鸣境",
        "suppress": 0.6,
        "emotions": Value::Object(emotions),
    });
    let action = format!("第{chapter_no}章正文生成");
    let warnings = ooc_gate::ooc_gate_json(
        &action,
        &char_state,
        None,
        20.0,
        Some(text),
        ctx.kb().as_ref(),
    );
    let warns = match warnings {
        Ok(Value::Array(a)) => a,
        _ => return GateOut::pass("ooc_gate 调用失败，skip_no_input"),
    };
    if warns.is_empty() {
        return GateOut::pass("OOC 判据全过");
    }
    out.fail_p0();
    let codes: Vec<String> = warns
        .iter()
        .map(|w| {
            let code = w.get("code").and_then(|v| v.as_str()).unwrap_or("");
            let ev = w.get("evidence").and_then(|v| v.as_str()).unwrap_or("");
            out.p0.push(mk(
                "G07_OOC",
                format!("{code}（{ev}）"),
                "按 OOC 判据修复（死角色/境界倒退/情绪矛盾/压抑越界/第5判据）",
            ));
            code.to_string()
        })
        .collect();
    out.detail = format!("OOC 命中 {} 条：{codes:?}", warns.len());
    out
}

// ───────────────────────── G08 伏笔回收率

pub fn g08_foreshadow_recover<C: GateCtx>(ctx: &C, text: &str, chapter_no: u32, volume: &str) -> GateOut {
    let mut out = GateOut { pass: true, p0: vec![], p1: vec![], detail: String::new() };
    let rows = ctx.due_foreshadows(volume, chapter_no);
    if rows.is_empty() {
        return GateOut::pass("本章无到期未回收伏笔，skip_no_input");
    }
    let (p2, p1_line) = (0.6_f64, 0.5_f64);
    let mut flagged = 0usize;
    for r in &rows {
        // 证据判定：去编号/去括号后取前 6 字，正文提及则 evidence=0.5
        let re_num = regex::Regex::new(r"\b[A-Z]+-\d+\b").unwrap();
        let core = re_num.replace_all(&r.content, "");
        let re_par = regex::Regex::new(r"[（(][^）)]*[）)]").unwrap();
        let core = re_par.replace_all(&core, "");
        let core: String = core.trim().chars().take(6).collect();
        let evidence = if !core.is_empty() && text.contains(&core) { 0.5 } else { 0.0 };
        let bm = crate::dynamics::bayes_mood(&json!({
            "prior": 0.5, "evidence": evidence,
            "deadline": chapter_no, "foreshadow_id": r.id,
        }));
        let post: f64 = bm
            .ok()
            .and_then(|v| v.get("bayes.posterior").and_then(|x| x.as_str()).and_then(|s| s.parse().ok()))
            .unwrap_or(0.0);
        if post < p1_line {
            flagged += 1;
            out.fail_p0();
            out.p0.push(mk(
                "G08_foreshadow_recover",
                format!("伏笔{} 后验={post:.3}<P1线{p1_line}（空壳）", r.id),
                "本章兑现该伏笔或明确延期（gates.bayes 双档）",
            ));
        } else if post < p2 {
            flagged += 1;
            out.p1.push(mk(
                "G08_foreshadow_recover",
                format!("伏笔{} 后验={post:.3}<P2线{p2}", r.id),
                "补回收证据（正文提及要素词）",
            ));
        }
    }
    out.detail = format!("到期未回收伏笔={} 命中降级={flagged}（P2={p2}/P1={p1_line}）", rows.len());
    out
}

// ───────────────────────── G09 信息密度

pub fn g09_info_density(text: &str, cfg: &GateCfg) -> GateOut {
    let mut out = GateOut { pass: true, p0: vec![], p1: vec![], detail: String::new() };
    let cap = cfg.info_density_max;
    let w = words(text);
    let expl: usize = ["即", "又称", "所谓", "设定", "规则"].iter().map(|k| text.matches(*k).count()).sum();
    let ratio = expl as f64 / (1.max(w) as f64);
    out.detail = format!("字数={w} cap={cap} 说明词密度={ratio:.4}");
    let word_max = cfg.chapter_word_max;
    if w > word_max {
        out.fail_p0();
        out.p0.push(mk(
            "G09_info_density",
            format!("字数{w}>硬上限{word_max}"),
            "砍字/分章（gates.chapter_word_max）",
        ));
    } else if w > (cap as f64 * 1.5) as usize {
        out.p1.push(mk(
            "G09_info_density",
            format!("字数{w}>cap×1.5（信息密度软提示）"),
            "压缩说明密度（<800优秀口径）",
        ));
    }
    out
}

// ───────────────────────── G10 恐怖章

pub fn g10_horror_chapter<C: GateCtx>(ctx: &C, text: &str, chapter_no: u32, volume: &str) -> GateOut {
    let mut out = GateOut { pass: true, p0: vec![], p1: vec![], detail: String::new() };
    let scene = ctx.scene_of(volume, chapter_no);
    let hk = ctx.cfg().book_profile.horror_keywords();
    let is_horror = scene == "心理" || hk.iter().any(|k| text.contains(k.as_str()));
    if !is_horror {
        return GateOut::pass("非恐怖章（场景≠心理 且 0 诡异要素词），G10 不适用");
    }
    let y_hits = [
        "未知恐惧", "日常异化", "感官代入", "规则类恐怖", "恐惧", "异化", "感官", "规则",
    ]
    .iter()
    .filter(|k| text.contains(**k))
    .count();
    out.detail = format!("恐怖章（场景={scene}），Y1-Y4 并集命中={y_hits}");
    if y_hits == 0 {
        out.fail_p0();
        out.p1.push(mk(
            "G10_horror_chapter",
            "恐怖章未命中 Y1-Y4 任一恐怖手法".into(),
            "补≥1 恐怖手法（Y1未知恐惧/Y2日常异化/Y3感官代入/Y4规则类恐怖）",
        ));
    }
    out
}

// ───────────────────────── G11 卷九反转

pub fn g11_vol9_reversal(text: &str, volume: &str) -> GateOut {
    let mut out = GateOut { pass: true, p0: vec![], p1: vec![], detail: String::new() };
    if volume != "卷09" {
        return GateOut::pass("非卷09，G11 卷九反转不适用");
    }
    let rev: usize = ["反转", "逆转", "揭露", "真相大白", "身份揭露"]
        .iter()
        .map(|k| text.matches(*k).count())
        .sum();
    out.detail = format!("卷09 反转关键词命中={rev}（技34 强制4层）");
    if rev < 4 {
        out.p1.push(mk(
            "G11_vol9_reversal",
            format!("卷09反转不足（命中{rev}<4层）"),
            "补卷九4层反转登记（技34 L3 细纲门禁，L5 软提示）",
        ));
    }
    out
}

// ───────────────────────── G12 命名阶位

pub fn g12_naming_tier(text: &str, volume: &str, cfg: &GateCfg) -> GateOut {
    let mut out = GateOut { pass: true, p0: vec![], p1: vec![], detail: String::new() };
    let vol_no: u32 = volume
        .chars()
        .rev()
        .take(2)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>()
        .parse()
        .unwrap_or(1);
    let tier = if vol_no <= 2 {
        "U0-1"
    } else if vol_no <= 4 {
        "U2-3"
    } else {
        "U4+"
    };
    let banned: Vec<String> = if tier == "U0-1" { cfg.banned_u01.clone() } else { vec![] };
    if banned.is_empty() {
        return GateOut::pass(format!("当前阶位={tier}（{volume}），无禁词词表（原文未载明），pass"));
    }
    let mut hits: BTreeMap<String, usize> = BTreeMap::new();
    for w in &banned {
        let n = text.matches(w.as_str()).count();
        if n > 0 {
            hits.insert(w.clone(), n);
        }
    }
    out.detail = format!("阶位={tier} 禁词命中={hits:?}");
    if !hits.is_empty() {
        out.fail_p0();
        for (w, n) in hits {
            out.p0.push(mk(
                "G12_naming_tier",
                format!("U0-1 朴实档越档命名词「{w}」×{n}"),
                &format!("改朴实命名（banned_by_tier.{tier}.banned_words）"),
            ));
        }
    }
    out
}

// ───────────────────────── G13 67维评分

pub fn g13_scoring_67dim<C: GateCtx>(ctx: &C, text: &str, _cfg: &GateCfg) -> GateOut {
    let mut out = GateOut { pass: true, p0: vec![], p1: vec![], detail: String::new() };
    let dims_total = ctx.scoring_dim_count();
    let w = words(text);
    let det: [(&str, bool); 4] = [
        ("字数", (2500..=4000).contains(&w)),
        ("禁词", !_cfg.book_profile.banned_words().iter().any(|k| text.contains(k.as_str()))),
        (
            "AI词",
            !["不禁", "竟然", "不可思议", "值得一提", "事实上", "众所周知", "缓缓", "微微"]
                .iter()
                .any(|k| text.contains(*k)),
        ),
        ("感叹号", (text.matches('!').count() + text.matches('！').count()) <= 1),
    ];
    let fail_names: Vec<&str> = det.iter().filter(|(_, ok)| !ok).map(|(n, _)| *n).collect();
    // ⚠ Python 原文用 `dims_total - 4`，当 dims_total=0（DB 不可用）会 underflow；
    // Rust 用 saturating_sub 避免 panic，并在 detail 里如实标注。
    out.detail = format!(
        "67维={dims_total}，确定性可量化4维命中不达标={}，其余{}维 needs_llm=True（LLM解释层占位，0编造分数）",
        if fail_names.is_empty() { "0".to_string() } else { format!("{fail_names:?}") },
        dims_total.saturating_sub(4)
    );
    if !fail_names.is_empty() {
        out.fail_p0();
        out.p1.push(mk(
            "G13_scoring_67dim",
            format!("确定性维度不达标：{fail_names:?}"),
            "按对应维度修复（字数/禁词/AI词/感叹号）；LLM 61维占位不阻断",
        ));
    }
    out
}

// ───────────────────────── G14 世界观一致性

pub fn g14_world_consistency<C: GateCtx>(ctx: &C, text: &str) -> GateOut {
    let mut out = GateOut { pass: true, p0: vec![], p1: vec![], detail: String::new() };

    // 来源神话 6 词硬闸 → P1
    let mut myth_hits: BTreeMap<String, usize> = BTreeMap::new();
    for w in ctx.cfg().book_profile.myth_banned() {
        let n = text.matches(w.as_str()).count();
        if n > 0 {
            myth_hits.insert(w, n);
        }
    }
    if !myth_hits.is_empty() {
        out.fail_p0();
        for (w, n) in &myth_hits {
            out.p1.push(mk(
                "G14_world_consistency",
                format!("来源神话词「{w}」×{n}（面板台词白名单只允许「锚点已建立」）"),
                "改写为「锚点已建立」/「未知来源」中性词，不写来源机制",
            ));
        }
    }
    out.detail = format!("；来源神话6词命中={}",
        if myth_hits.is_empty() { "0".to_string() } else { format!("{myth_hits:?}") });

    // 复用 k5 world_consistency（有 kb 时）
    if let Some(kb) = ctx.kb() {
        let warns = ooc_gate::world_consistency(Some(text), Some(&kb), "");
        let drift: Vec<_> = warns.iter().filter(|w| w.code == "WORLD_CONSISTENCY_DRIFT").collect();
        let canon: Vec<_> = warns.iter().filter(|w| w.code == "WORLD_CONSISTENCY_CANON").collect();
        out.detail = format!("kb drift命中={} canon违反={}", drift.len(), canon.len());
        for w in &drift {
            out.fail_p0();
            out.p0.push(mk(
                "G14_world_consistency",
                format!("drift黑名单命中（{}）", w.evidence),
                "按 kb_knowledge drift 勘误账本改词",
            ));
        }
        for w in &canon {
            out.p1.push(mk(
                "G14_world_consistency",
                format!("canon基准违反（{}）", w.evidence),
                "对照 canon 定论修正（0 阻断，告警级）",
            ));
        }
        return out;
    }

    // 兜底：从 DB 提取 drift 词
    let terms = ctx.kb_drift_terms_from_db();
    if terms.is_empty() {
        out.detail.push_str("；DB 缺失，G14 skip_no_input");
        return out;
    }
    let mut hits: BTreeMap<String, usize> = BTreeMap::new();
    for w in &terms {
        let n = text.matches(w.as_str()).count();
        if n > 0 {
            hits.insert(w.clone(), n);
        }
    }
    out.detail.push_str(&format!("；drift词表={}个 命中={}",
        terms.len(), if hits.is_empty() { "0".into() } else { format!("{hits:?}") }));
    if !hits.is_empty() {
        out.fail_p0();
        for (w, n) in hits {
            out.p0.push(mk(
                "G14_world_consistency",
                format!("drift黑名单词「{w}」×{n}"),
                "按勘误账本改词（kb_knowledge verdict=drift）",
            ));
        }
    }
    out
}

// ───────────────────────── G15 家庭 canon 一致性

pub fn g15_family_consistency(text: &str, cfg: &GateCfg) -> GateOut {
    let mut out = GateOut { pass: true, p0: vec![], p1: vec![], detail: String::new() };
    let mut hit = 0usize;
    let family_canon = cfg.book_profile.canon_family();
    let canon_names = cfg.book_profile.canon_names();
    for (title, canon_name) in &family_canon {
        let pat = format!(r"{}[的|叫]?([\u{{4e00}}-\u{{9fa5}}]{{2,4}})", regex::escape(title));
        let Ok(re) = regex::Regex::new(&pat.replace("[的|叫]?", "(?:的|叫)?")) else {
            continue;
        };
        for cap in re.captures_iter(text) {
            let Some(m) = cap.get(1) else { continue };
            let mentioned = m.as_str();
            let ok = canon_names.iter().any(|c| {
                let cl = c.chars().count();
                mentioned.chars().count() >= cl && mentioned.chars().take(cl).collect::<String>() == *c
            });
            if ok {
                continue;
            }
            hit += 1;
            out.p1.push(mk(
                "G15_family_consistency",
                format!("称谓「{title}」提及「{mentioned}」≠canon「{canon_name}」（家庭定版·后续不得改）"),
                "按 canon 改回：父=陈大山/母=林氏/弟=陈默/祖=陈老头",
            ));
        }
    }
    out.detail = format!("家庭canon错配={hit}处（父=陈大山/母=林氏/弟=陈默/祖=陈老头，后续不得改）");
    out.pass = hit == 0;
    out
}

// ───────────────────────── G16 指令泄漏词

pub fn g16_leak_words(text: &str) -> GateOut {
    let mut out = GateOut { pass: true, p0: vec![], p1: vec![], detail: String::new() };
    let mut hits: BTreeMap<String, usize> = BTreeMap::new();
    for w in LEAK_WORDS {
        let n = text.matches(w).count();
        if n > 0 {
            hits.insert(w.to_string(), n);
        }
    }
    if !hits.is_empty() {
        out.fail_p0();
        for (w, n) in &hits {
            out.p1.push(mk(
                "G16_leak_words",
                format!("指令泄漏词「{w}」×{n}（prompt 规则词入正文，非文学表达）"),
                "删除或改写为叙事（规则词只进 system 指令区，不得出现在正文）",
            ));
        }
    }
    out.detail = format!("泄漏词命中={}",
        if hits.is_empty() { "0".to_string() } else { format!("{hits:?}") });
    out
}

// ───────────────────────── 主入口

/// 跑全部 16 门禁（照抄 run_14）。
///
/// 单门禁异常不拖垮全链——Rust 版无异常，改为「Err 结果记为 skip」（语义等价）。
pub fn run_all<C: GateCtx>(ctx: &C, text: &str, chapter_no: u32, volume: &str) -> Result<GateRun> {
    let cfg = ctx.cfg();
    let mut p0: Vec<GateFinding> = Vec::new();
    let mut p1: Vec<GateFinding> = Vec::new();
    let mut gates: BTreeMap<String, GateBrief> = BTreeMap::new();

    let results: Vec<(&str, Result<GateOut>)> = vec![
        ("G01_opening4", Ok(g01_opening4(text, chapter_no, cfg))),
        ("G02_hook_density", Ok(g02_hook_density(text, cfg))),
        ("G03_shuang_interval", Ok(g03_shuang_interval(ctx, text, chapter_no, volume))),
        ("G04_ability_cost", Ok(g04_ability_cost(text, cfg))),
        ("G05_pov_consistent", Ok(g05_pov_consistent(text, cfg))),
        ("G06_character_consistency", Ok(g06_character_consistency(ctx, text))),
        ("G07_OOC", Ok(g07_ooc(ctx, text, chapter_no))),
        ("G08_foreshadow_recover", Ok(g08_foreshadow_recover(ctx, text, chapter_no, volume))),
        ("G09_info_density", Ok(g09_info_density(text, cfg))),
        ("G10_horror_chapter", Ok(g10_horror_chapter(ctx, text, chapter_no, volume))),
        ("G11_vol9_reversal", Ok(g11_vol9_reversal(text, volume))),
        ("G12_naming_tier", Ok(g12_naming_tier(text, volume, cfg))),
        ("G13_scoring_67dim", Ok(g13_scoring_67dim(ctx, text, cfg))),
        ("G14_world_consistency", Ok(g14_world_consistency(ctx, text))),
        ("G15_family_consistency", Ok(g15_family_consistency(text, cfg))),
        ("G16_leak_words", Ok(g16_leak_words(text))),
    ];

    for (name, r) in results {
        let r = match r {
            Ok(r) => r,
            Err(e) => GateOut::pass(format!("异常skip：{e}")),
        };
        gates.insert(
            name.to_string(),
            GateBrief { pass: r.pass, detail: r.detail.clone() },
        );
        p0.extend(r.p0);
        p1.extend(r.p1);
    }

    let verdict = if !p0.is_empty() {
        "reject"
    } else if !p1.is_empty() {
        "fix"
    } else {
        "pass"
    };

    Ok(GateRun {
        verdict: verdict.to_string(),
        p0,
        p1,
        gates,
        stats: json!({ "words": words(text), "gates_run": GATE_ORDER.len() }),
    })
}

// ───────────────────────── 无上下文实现（供 MCP / CLI 等无 DB 场景）

/// **无外部数据的 GateCtx**：所有需 DB/文件的门禁走 skip 路径。
///
/// 用途：`ne-mcp` 的 `check` 工具只传 (text, chapter, volume)，
/// 没有 DB 句柄。用它跑 16 门禁，纯文本门禁（G01/G02/G04/G05/G09/G11/G12/G15/G16）
/// 照常生效；需数据的（G03/G06/G08/G10/G13/G14）如实 skip。
pub struct BareGateCtx {
    pub cfg: GateCfg,
}

impl BareGateCtx {
    /// 从 dynamics_config.json 文本构造（缺则用默认值）。
    pub fn from_config_text(txt: Option<&str>) -> Self {
        let cfg = txt
            .and_then(|t| serde_json::from_str::<Value>(t).ok())
            .map(|v| GateCfg::from_json(&v))
            .unwrap_or_default();
        // GateCfg::default() 的数值字段是 0，需修正为 Python 默认值
        let cfg = if cfg.chapter_word_max == 0 {
            GateCfg {
                hook_min_per_chapter: 1,
                info_density_max: 1000,
                chapter_word_max: 4000,
                banned_u01: vec![
                    "太玄灵石".into(),
                    "共鸣水晶".into(),
                    "传说".into(),
                    "神级".into(),
                    "史诗".into(),
                ],
                scoring_excellent: 95.0,
                scoring_superior: 98.0,
                book_profile: cfg.book_profile.clone(),
            }
        } else {
            cfg
        };
        BareGateCtx { cfg }
    }
}

impl GateCtx for BareGateCtx {
    fn cfg(&self) -> &GateCfg {
        &self.cfg
    }
    fn prev_chapters_text(&self, _v: &str, _c: u32) -> String {
        String::new()
    }
    fn role_catchphrases(&self) -> Vec<(String, String)> {
        vec![]
    }
    fn due_foreshadows(&self, _v: &str, _c: u32) -> Vec<DueForeshadow> {
        vec![]
    }
    fn scene_of(&self, _v: &str, _c: u32) -> String {
        "未知".into()
    }
    fn kb(&self) -> Option<Value> {
        None
    }
    fn kb_drift_terms_from_db(&self) -> Vec<String> {
        vec![]
    }
    fn scoring_dim_count(&self) -> usize {
        0
    }
}
