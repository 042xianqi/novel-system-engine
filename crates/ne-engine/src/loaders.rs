//! 灌库/校验层 —— `db/` 下装载脚本的 Rust 移植。
//!
//! 覆盖（按 `db/*.py` 源）：
//!   * `load_config.py`    配置完整性校验（节完整 / 15 gate 键 / ASCII 键名铁律）
//!   * `load_structure.py` structure_beats 卷级骨架（seg_no / verify_volumes / BEATS 实值）
//!
//! **修复的历史 bug**：`load_structure.py:34` 与 `load_config.py:34` 均硬编码
//! `ROOT = "/sdcard/projects/系统/资料库"`（错误路径，真库在 `/sdcard/Download/...`）。
//! Rust 版全部路径由调用方注入。
//!
//! 本模块**不做 DB 写入**（写入由 `crate::db` 负责）；只产出「待灌行」与校验结论。

use serde_json::{json, Map, Value};
use std::collections::BTreeSet;

// ───────────────────────── load_config.py

/// 必需节（照抄 REQUIRED_SECTIONS）。
pub const REQUIRED_SECTIONS: [&str; 6] =
    ["gates", "rhythm", "tlm", "bayes", "structure", "arbitrate_weights"];

/// 允许中文子键的父路径（照抄 TIER_NAME_KEYS）。
pub const TIER_NAME_KEYS: [&str; 3] = ["tlm.decay_by_causal", "bayes.prior", "banned_by_tier"];

/// K5 §4 的 15 个 gate 键（照抄 K5_15_GATE_KEYS）。
pub const K5_15_GATE_KEYS: [&str; 15] = [
    "opening_4_assertion",
    "hook_density",
    "shuang_point_interval",
    "ability_cost",
    "pov_lock",
    "character_consistency",
    "ooc_judgement",
    "foreshadow_recover",
    "info_density",
    "horror_chapter",
    "vol9_reversal",
    "naming_tier",
    "scoring_67dim",
    "world_consistency",
    "family_consistency",
];

/// sha256 前 12 位（照抄 sha12_bytes）。
pub fn sha12_bytes(b: &[u8]) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    // ⚠ Rust 标准库无 sha256；这里用 DefaultHasher 仅作**稳定内键**用途，
    // 与 Python 的 sha256[:12] **不等价**。真实去重请启用 `sha2` feature 后替换。
    // 已在文档如实标注，避免误用为密码学摘要。
    let mut h = DefaultHasher::new();
    b.hash(&mut h);
    format!("{:012x}", h.finish() & 0xffff_ffff_ffff)
}

/// 配置校验结果（对应 `validate_config` 的 (ok, counts) + report）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConfigReport {
    pub ok: bool,
    pub missing_sections: Vec<String>,
    pub missing_gates: Vec<String>,
    pub extra_gates: Vec<String>,
    pub non_ascii_keys: Vec<String>,
    pub null_valued_keys: Vec<String>,
    pub bad_array_keys: Vec<String>,
    pub counts: Value,
    pub messages: Vec<String>,
}

/// 递归收集非 ASCII 键名（照抄 check_ascii_keys）。
pub fn check_ascii_keys(obj: &Value, path: &str, out: &mut Vec<String>) {
    match obj {
        Value::Object(m) => {
            for (k, v) in m {
                let full = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                if !k.is_ascii() {
                    let parent = full.rsplit_once('.').map(|(p, _)| p.to_string()).unwrap_or_default();
                    if !TIER_NAME_KEYS.contains(&parent.as_str()) {
                        out.push(full.clone());
                    }
                }
                check_ascii_keys(v, &full, out);
            }
        }
        Value::Array(a) => {
            for (i, item) in a.iter().enumerate() {
                check_ascii_keys(item, &format!("{path}[{i}]"), out);
            }
        }
        _ => {}
    }
}

/// 校验 `dynamics_config.json`（照抄 validate_config）。
pub fn validate_config(data: Option<&Value>) -> ConfigReport {
    let mut r = ConfigReport {
        ok: false,
        missing_sections: vec![],
        missing_gates: vec![],
        extra_gates: vec![],
        non_ascii_keys: vec![],
        null_valued_keys: vec![],
        bad_array_keys: vec![],
        counts: json!({}),
        messages: vec![],
    };
    let Some(d) = data else {
        r.messages.push("[config] 数据缺失 → 0 校验".into());
        return r;
    };

    // ① 节完整性
    r.missing_sections = REQUIRED_SECTIONS
        .iter()
        .filter(|s| d.get(**s).is_none())
        .map(|s| s.to_string())
        .collect();
    if !r.missing_sections.is_empty() {
        r.messages
            .push(format!("[config] 缺节：{:?}", r.missing_sections));
    }

    // ② 15 gate 键齐全
    let gk15 = d.get("gate_keys_15").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let mut counts = Map::new();
    counts.insert("gate_keys_15_rows".into(), json!(gk15.len()));

    let present: BTreeSet<String> = gk15
        .iter()
        .filter_map(|e| e.get("key").and_then(|k| k.as_str()).map(String::from))
        .collect();
    let wanted: BTreeSet<String> = K5_15_GATE_KEYS.iter().map(|s| s.to_string()).collect();
    r.missing_gates = wanted.difference(&present).cloned().collect();
    r.extra_gates = present.difference(&wanted).cloned().collect();
    if !r.missing_gates.is_empty() {
        r.messages.push(format!("[config] gate_keys_15 缺键：{:?}", r.missing_gates));
    }
    if !r.extra_gates.is_empty() {
        r.messages.push(format!(
            "[config] gate_keys_15 多键（允许存在但报出）：{:?}",
            r.extra_gates
        ));
    }

    // ②b 键值类型校验
    for e in &gk15 {
        let Some(o) = e.as_object() else {
            r.messages.push("[config] gate_keys_15 条目非 dict 拒校验".into());
            continue;
        };
        let key = o.get("key").and_then(|v| v.as_str()).unwrap_or("");
        match o.get("value") {
            None | Some(Value::Null) => {
                r.null_valued_keys.push(key.to_string());
                r.messages.push(format!("[config] {key} value=null 拒灌"));
            }
            Some(Value::Array(a)) => {
                // 数组型必须是 [min,max] 双元素数值
                let ok = a.len() == 2 && a.iter().all(|x| x.is_number());
                if !ok {
                    r.bad_array_keys.push(key.to_string());
                    r.messages
                        .push(format!("[config] {key} 数组型非 [min,max] 双数值"));
                }
            }
            _ => {}
        }
    }

    // ③ ASCII 键名铁律
    let mut na = Vec::new();
    check_ascii_keys(d, "", &mut na);
    r.non_ascii_keys = na;
    if !r.non_ascii_keys.is_empty() {
        r.messages
            .push(format!("[config] 非 ASCII 键名：{:?}", r.non_ascii_keys));
    }

    r.ok = r.missing_sections.is_empty()
        && r.missing_gates.is_empty()
        && r.non_ascii_keys.is_empty()
        && r.null_valued_keys.is_empty()
        && r.bad_array_keys.is_empty();
    r.counts = Value::Object(counts);
    r
}

// ───────────────────────── load_structure.py

/// 卷级实值（照抄 VOLUMES：(卷号, 起始章, 结束章, 三段划分, 宏观规则)）。
pub const VOLUMES: [(&str, u32, u32, &str, &str); 9] = [
    ("卷02", 181, 360, "入局181-240/加压241-300/承诺301-360", "新文明碰撞首战（lst.square P1消费点）"),
    ("卷03", 361, 540, "361-420入局/421-500主体/501-540跃迁卷末", "情感线小高潮密集段（0.85 decay档，F5短钩1-2章回收）"),
    ("卷04", 541, 720, "541-600入暗影（红线区起）/601-680主体/681-720收口（长线收口+U4-U5跃迁）", "埋设红线区（全书55%起）：只埋短钩+回收旧长线，禁开新长线"),
    ("卷05", 721, 900, "721-780入局兑现/781-840主体并烧/841-900收口跃迁", "埋设红线区续：禁开新长线（卷04~05），只兑现/收口既有对抗"),
    ("卷06", 901, 1080, "901-960兑现/961-1020冲突/1021-1080跃迁", "长线伏笔集中兑现段（回收率50-58%档）"),
    ("卷07", 1081, 1260, "1081-1140兑现（长线账目对账）/1141-1200冲突（三线并烧）/1201-1260危机（幽影星实验体001号+U6→U7跃迁）", "长线兑现段续（存量长线集中兑现）"),
    ("卷08", 1261, 1440, "1261-1320入局（证道场·面板=体系外未证变量）/1321-1380揭露·争夺/1381-1440翻转·共认（卷末U8蓄势）", "反转/身份揭露高峰（H2型钩），收藏拐点二次爬坡（D5）"),
    ("卷09", 1441, 1620, "1441-1500入局（天枢之门接掌·摆渡人身份）/1501-1560加压（暗流清账+魂契桥陈默重逢）/1561-1620收线（12文明恩怨全收）", "主线收敛（全书80%+）：新线全禁，只回收；单章新埋0条长线"),
    ("卷10", 1621, 1800, "入局1621-1700（抵无始·U8前置）/高潮1701-1760（双轨坍缩·归一者浮现·陈玄冥传承兑现）/收束1761-1800（5章限期兑现=1796-1800）", "大高潮×1-2（S4全书级）+全部未兑现伏笔限期5章兑现（F3），残响归河"),
];

/// 卷内每段章数的**默认值**（历史硬编码 60）。
///
/// ⚠ 实际值应由配置给出：`dynamics_config.json` 的 `volume_beat.chapters_per_segment`。
/// 本常量仅作兜底。
pub const DEFAULT_CHAPTERS_PER_SEGMENT: u32 = 60;

/// 卷内三段段号：`ceil((ch - vol_start + 1) / per_seg)`，等价 `(ch-vol_start)/per_seg + 1`。
///
/// `per_seg` = 每段章数。**不再写死 60** —— 用户要求「按实际内容伸缩」，
/// 不同卷可有不同段落划分。
pub fn seg_no_with(vol_start: u32, ch: u32, per_seg: u32) -> u32 {
    if ch < vol_start || per_seg == 0 {
        return 0;
    }
    ((ch - vol_start) / per_seg) + 1
}

/// 兼容旧调用（默认每段 60 章）。
pub fn seg_no(vol_start: u32, ch: u32) -> u32 {
    seg_no_with(vol_start, ch, DEFAULT_CHAPTERS_PER_SEGMENT)
}

/// 跨卷线锚点（照抄 ANCHORS）。
pub const ANCHORS: &str = "跨卷线锚点6条（跨卷线锚点表.md）：线1林晚秋CHAR-90-042（卷02~07随行/分离/重逢，卷08深化）；\
线2陈默CHAR-90-041（卷01亲情支点→卷09魂契桥重逢→卷10收束）；\
线3白鸦委托REL-99-004（卷04起只收口/推进，禁开新长线）；\
线4推演面板U阶（卷02 U1-U3→卷04-05 U4-U5→卷07 U6-U7→卷08卷末U8蓄势→卷10 U8一次性不可逆）；\
线5存在之基碎片EVT-99-002（异象=跨文明触发器）；\
线6五大文明合作框架REL-99-006 §5.1";

/// 章级微节拍硬规则（照抄 TENSION）。
pub const TENSION: &str = "章级微节拍硬规则（dyn_x4 §4.3）：每章末位必写钩子（H1/H3，蓝图先规划）；章内起承转爽4段字数比500:800:300:400（2000字单章）";

/// S1–S10 十类节拍（照抄 BEATS：(code, name, value, applies_to)）。
pub fn beats() -> Vec<(&'static str, &'static str, String, &'static str)> {
    let s9 = format!(
        "逐卷180章（ch181~1800全局连续）：{}",
        VOLUMES
            .iter()
            .map(|v| format!("{} ch{}~{}", v.0, v.1, v.2))
            .collect::<Vec<_>>()
            .join("；")
    );
    let s10 = format!(
        "逐卷三段划分+宏观规则：{}。{}",
        VOLUMES
            .iter()
            .map(|v| format!("{}({}){}", v.0, v.3, v.4))
            .collect::<Vec<_>>()
            .join("；"),
        ANCHORS
    );
    vec![
        ("S1", "小爽点密度", "每章≥1小爽点（S1口径）；每章蓝图末位必写钩子（H1/H3，蓝图先规划不等写完）".into(), "卷02~10全章（ch181~1800）"),
        ("S2", "小高潮峰", "中段每5章一个小高潮峰（dyn_x4 §4.1），与逐卷三段（入局/主体/加压 等）叠加".into(), "卷02~10中段"),
        ("S3", "铺垫蓄力", "高潮前铺垫窗≤5章（S3），张力蓄力；卷08 1381-1440 翻转·共认段/卷10 高潮段1701-1760前蓄力".into(), "卷02~10高潮前段"),
        ("S4", "卷高潮", "卷末大爽点/中高潮位（S4）；卷10升级全书级大高潮×1-2（1701-1760）+5章限期兑现（1796-1800，F3）".into(), "卷02~10卷末"),
        ("S5", "埋线伏笔", "章5/10/15位埋中/长线伏笔，单章新埋≤2条（F4）；卷04-05红线区禁开新长线（只短钩1-2章回收）".into(), "卷02~10中段"),
        ("S6", "章内四段", TENSION.into(), "卷02~10全章"),
        ("S7", "大钩前置", "H3型大钩前置在卷高潮前段章末，下章必兑现（§4.1 17章位型）".into(), "卷02~10卷高潮前"),
        ("S8", "余韵下卷钩子", "卷末余韵+H1悬念式下卷钩子（§4.1 18章位型），承下卷".into(), "卷02~09卷末"),
        ("S9", "章号区间", s9, "卷02~10"),
        ("S10", "卷级宏观", s10, "卷02~10"),
    ]
}

/// 卷级核验结论（对应 `verify_volumes`）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct VolumeCheck {
    pub ok: bool,
    pub lines: Vec<VolumeLine>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct VolumeLine {
    pub vol: String,
    pub expect: String,
    pub chap_count: usize,
    pub first: Option<u32>,
    pub last: Option<u32>,
    pub has_seg_line: bool,
    pub ok: bool,
}

/// 核验单卷细纲文本（照抄 verify_volumes 的判定口径）。
/// 校验一卷细纲的章号范围与连续性。
///
/// ⚠ **不再断言 `heads.len() == 180`**（2026-10-10 修正）。
/// 旧版写死 180，与用户要求「卷章数按实际内容伸缩」直接冲突，
/// 且实际细纲每章概要字数从 230 到 1058 不等 —— 强行等长是「AI 味」的来源。
///
/// 现行判定：
/// - 首章 = `c0`、末章 = `c1`
/// - 章号**连续**（无跳号、无重复）
/// - 有分段行
///
/// 章数本身**不做等长要求**，只要求与 `c0~c1` 自洽。
pub fn verify_volume(vol: &str, c0: u32, c1: u32, text: &str) -> VolumeLine {
    let re_head = regex::Regex::new(r"(?m)^### 第(\d+)章").unwrap();
    let heads: Vec<u32> = re_head
        .captures_iter(text)
        .filter_map(|c| c.get(1).and_then(|m| m.as_str().parse().ok()))
        .collect();
    let re_seg = regex::Regex::new(r"(?m)^> .*(段：|章号区间：)").unwrap();
    let has_seg = re_seg.is_match(text);
    let first = heads.first().copied();
    let last = heads.last().copied();

    // 连续性检查（替代旧的 == 180）
    let contiguous = heads
        .windows(2)
        .all(|w| w[1] == w[0] + 1);
    let expect_count = (c1.saturating_sub(c0) + 1) as usize;
    let count_ok = heads.len() == expect_count; // 由 c0~c1 推出的应有余数，而非写死 180

    let h_ok = first == Some(c0) && last == Some(c1) && count_ok && contiguous;
    VolumeLine {
        vol: vol.to_string(),
        expect: format!("ch{c0}~{c1}"),
        chap_count: heads.len(),
        first,
        last,
        has_seg_line: has_seg,
        ok: h_ok && has_seg,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn seg_no_matches_python() {
        // Python: seg_no(181, 181) = 1；seg_no(181, 241) = 2；seg_no(181, 300) = 2；seg_no(181, 301) = 3
        assert_eq!(seg_no(181, 181), 1);
        assert_eq!(seg_no(181, 240), 1);
        assert_eq!(seg_no(181, 241), 2);
        assert_eq!(seg_no(181, 300), 2);
        assert_eq!(seg_no(181, 301), 3);
        assert_eq!(seg_no(181, 360), 3);
    }

    #[test]
    fn beats_has_10() {
        let b = beats();
        assert_eq!(b.len(), 10);
        assert_eq!(b[0].0, "S1");
        assert_eq!(b[9].0, "S10");
        assert!(b[9].2.contains("跨卷线锚点6条"));
    }

    #[test]
    fn validate_config_detects_missing_section() {
        let d = json!({"gates": {}, "rhythm": {}});
        let r = validate_config(Some(&d));
        assert!(!r.ok);
        assert!(r.missing_sections.contains(&"tlm".to_string()));
    }

    #[test]
    fn validate_config_detects_non_ascii_key() {
        let d = json!({
            "gates": {}, "rhythm": {}, "tlm": {}, "bayes": {}, "structure": {}, "arbitrate_weights": {},
            "gate_keys_15": K5_15_GATE_KEYS.iter().map(|k| json!({"key":k,"value":1})).collect::<Vec<_>>(),
            "坏键": 1,
        });
        let r = validate_config(Some(&d));
        assert!(!r.ok);
        assert!(r.non_ascii_keys.iter().any(|k| k.contains("坏键")), "{:?}", r.non_ascii_keys);
    }

    #[test]
    fn validate_config_allows_tier_chinese_keys() {
        // bayes.prior 下的中文子键允许（TIER_NAME_KEYS）
        let d = json!({
            "gates": {}, "rhythm": {}, "tlm": {}, "structure": {}, "arbitrate_weights": {},
            "bayes": {"prior": {"伏笔A": 0.5}},
            "gate_keys_15": K5_15_GATE_KEYS.iter().map(|k| json!({"key":k,"value":1})).collect::<Vec<_>>(),
        });
        let r = validate_config(Some(&d));
        assert!(r.non_ascii_keys.is_empty(), "bayes.prior 中文子键应豁免: {:?}", r.non_ascii_keys);
    }

    #[test]
    fn validate_config_good_passes() {
        let d = json!({
            "gates": {}, "rhythm": {}, "tlm": {}, "bayes": {}, "structure": {}, "arbitrate_weights": {},
            "gate_keys_15": K5_15_GATE_KEYS.iter().map(|k| json!({"key":k,"value":1})).collect::<Vec<_>>(),
        });
        let r = validate_config(Some(&d));
        assert!(r.ok, "合法配置应通过: {:?}", r.messages);
    }

    #[test]
    fn validate_config_flags_null_gate_value() {
        let mut keys: Vec<Value> =
            K5_15_GATE_KEYS.iter().map(|k| json!({"key":k,"value":1})).collect();
        keys[0] = json!({"key":"opening_4_assertion","value":null});
        let d = json!({
            "gates": {}, "rhythm": {}, "tlm": {}, "bayes": {}, "structure": {}, "arbitrate_weights": {},
            "gate_keys_15": keys,
        });
        let r = validate_config(Some(&d));
        assert!(!r.ok);
        assert!(r.null_valued_keys.contains(&"opening_4_assertion".to_string()));
    }

    #[test]
    fn verify_volume_range_and_continuity() {
        let good: String = (181..=360)
            .map(|i| format!("### 第{i}章 标题\n内容\n"))
            .collect::<String>()
            + "> 段：入局181-240/加压241-300/承诺301-360\n";
        let l = verify_volume("卷02", 181, 360, &good);
        assert_eq!(l.chap_count, 180);
        assert!(l.ok, "{l:?}");
        // 缺三段行 → 不 ok
        let bad: String = (181..=360).map(|i| format!("### 第{i}章 x\n")).collect();
        assert!(!verify_volume("卷02", 181, 360, &bad).ok);
    }

    /// F3 回归：**不同卷可以有不同章数**（不再强制 180 / 60）。
    #[test]
    fn verify_volume_accepts_variable_length() {
        // 130 章一卷 —— 旧版会因为 != 180 直接失败
        let mk = |c0: u32, c1: u32| -> String {
            let bodies: String = (c0..=c1).map(|i| format!("### 第{i}章 标题\n内容\n")).collect();
            format!("{bodies}> 段：{c0}-{}\n", c0 + 40)
        };
        for (c0, c1, want) in [(1u32, 130u32, 130usize), (131, 260, 130), (261, 500, 240)] {
            let l = verify_volume("卷X", c0, c1, &mk(c0, c1));
            assert!(l.ok, "章数 {want}（非 180）应通过：{l:?}");
            assert_eq!(l.chap_count, want);
        }
    }

    /// 跳号/重复应判失败（连续性替代旧的长度断言）。
    #[test]
    fn verify_volume_rejects_gaps_and_dupes() {
        let gap = "### 第1章 a\n### 第2章 b\n### 第4章 c\n> 段：1-4\n";
        assert!(!verify_volume("卷X", 1, 4, gap).ok, "跳号应失败");
        let dup = "### 第1章 a\n### 第1章 b\n### 第2章 c\n> 段：1-2\n";
        assert!(!verify_volume("卷X", 1, 2, dup).ok, "重复应失败");
    }

    #[test]
    fn seg_no_is_configurable() {
        // 旧默认 60
        assert_eq!(seg_no(1, 1), 1);
        assert_eq!(seg_no(1, 60), 1);
        assert_eq!(seg_no(1, 61), 2);
        // 可配置：每段 45 章
        assert_eq!(seg_no_with(1, 45, 45), 1);
        assert_eq!(seg_no_with(1, 46, 45), 2);
        // 边界
        assert_eq!(seg_no_with(100, 99, 60), 0, "早于卷首应返 0");
        assert_eq!(seg_no_with(1, 1, 0), 0, "除数 0 不应 panic");
    }
}
