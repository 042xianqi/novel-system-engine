//! 角色标定 ETL —— `char_calib_etl.py` 的 Rust 忠实移植（0 LLM，纯确定性）。
//!
//! 事实源：`05_管线框架/engine/char_calib_etl.py`（187 行）
//!
//! **修复的前置数据 bug（2026-10-10 实测确认）**：
//!   `data/emotion_calib_map.json` 原文件**不是合法 JSON**，两处语法错误：
//!     1. rule id=3 的 `effect.base` 对象缺右花括号 `}`
//!     2. rule id=12 整行被截断：缺 `effect` 闭合并缺 `"note"` 字段
//!   → Python `json.load()` 抛 JSONDecodeError ⇒ **`char_calib_etl.py` 从未成功运行过**。
//!   Rust 侧已修复该文件（原文件备份为 `emotion_calib_map.json.orig`），
//!   并在本模块加载时做**结构化校验**（缺字段即报错，不静默吞掉）。
//!
//! 提取：`### 性格/性格特征/...` 段 + `> 介绍：` 行 → 文学词→数值映射。

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

/// 标定映射表（对应 `emotion_calib_map.json`）。
#[derive(Debug, Clone, Deserialize)]
pub struct CalibMap {
    pub dims: Vec<String>,
    pub default_base: Map<String, Value>,
    pub default_lambda: String,
    pub rules: Vec<CalibRule>,
    #[serde(default)]
    pub default_suppress: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CalibRule {
    pub id: u32,
    #[serde(rename = "词")]
    pub words: String,
    pub effect: RuleEffect,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct RuleEffect {
    #[serde(default)]
    pub base: Map<String, Value>,
    #[serde(default)]
    pub suppress: Option<f64>,
    #[serde(default)]
    pub personality: Map<String, Value>,
}

impl CalibMap {
    /// 解析并做**结构校验**（Python 版没有校验，导致坏文件静默失败）。
    pub fn from_json_text(txt: &str) -> Result<Self> {
        let m: CalibMap = serde_json::from_str(txt)
            .context("emotion_calib_map.json 解析失败（原文件历史上曾语法损坏）")?;
        if m.dims.is_empty() {
            return Err(anyhow!("emotion_calib_map.dims 为空"));
        }
        if m.rules.is_empty() {
            return Err(anyhow!("emotion_calib_map.rules 为空"));
        }
        for r in &m.rules {
            if r.words.is_empty() {
                return Err(anyhow!("rule {} 的「词」字段为空", r.id));
            }
            // 每个 base/personality 的键必须是合法维度
            for k in r.effect.base.keys().chain(r.effect.personality.keys()) {
                if !m.dims.contains(k) {
                    return Err(anyhow!("rule {} 引用了未知维度 {k:?}", r.id));
                }
            }
        }
        Ok(m)
    }
}

/// 卡片解析结果（对应 `extract_sections` 返回的 section_text + intro）。
#[derive(Debug, Clone, Default, Serialize)]
pub struct Sections {
    /// 性格段行（已 strip、去 `<!--` 行）
    pub xingge: Vec<String>,
    /// 基础信息段行
    pub base: Vec<String>,
    /// `> 介绍：` 行（首个）
    pub intro: Option<String>,
}

/// 从卡片 Markdown 抽取性格段 / 基础信息段 / 介绍行（照抄 `extract_sections`）。
pub fn extract_sections(text: &str) -> Sections {
    let heading_xingge = regex::Regex::new(r"^#{3,4}\s*性格.*$").unwrap();
    let h3_base = regex::Regex::new(r"^#{3,4}\s*基础信息").unwrap();
    let h2_any = regex::Regex::new(r"^#{2,4}\s").unwrap();
    let h1_any = regex::Regex::new(r"^#{1,2}\s").unwrap();

    let mut out = Sections::default();
    // cur: None / "xingge" / "base"
    let mut cur: Option<&str> = None;
    let mut in_xingge = false;

    for ln in text.split('\n') {
        if ln.starts_with("# ") {
            cur = None;
            in_xingge = false;
            continue;
        }
        if heading_xingge.is_match(ln) {
            cur = Some("xingge");
            in_xingge = true;
            continue;
        }
        if h2_any.is_match(ln) && !heading_xingge.is_match(ln) {
            if ln.starts_with("### 基础信息") || h3_base.is_match(ln) {
                cur = Some("base");
            } else {
                cur = None;
            }
            in_xingge = false;
            continue;
        }
        if h1_any.is_match(ln) {
            in_xingge = false;
            cur = None;
            continue;
        }
        let stripped = ln.trim();
        if stripped.starts_with("> 介绍：") && out.intro.is_none() {
            out.intro = Some(stripped.to_string());
            continue;
        }
        if stripped.is_empty() || stripped.starts_with("<!--") {
            continue;
        }
        if in_xingge {
            out.xingge.push(stripped.to_string());
        }
        if cur == Some("base") {
            out.base.push(stripped.to_string());
        }
    }
    out
}

/// 命中规则（照抄 `match_rules`：任一 pattern 是子串即命中）。
pub fn match_rules<'a>(map: &'a CalibMap, blob: &str) -> Vec<&'a CalibRule> {
    map.rules
        .iter()
        .filter(|r| r.words.split('|').any(|p| !p.is_empty() && blob.contains(p)))
        .collect()
}

/// 标定结果。
#[derive(Debug, Clone, Serialize)]
pub struct Calibrated {
    pub emotion_base: Map<String, Value>,
    pub suppress: Option<f64>,
    pub personality: Map<String, Value>,
    pub lambda: String,
    pub rule_hits: Vec<u32>,
}

/// 执行标定（照抄 `calibrate`）。未命中 → `None`（待标定）。
pub fn calibrate(map: &CalibMap, xingge_text: &str, intro: Option<&str>) -> Option<Calibrated> {
    let blob = format!("{xingge_text}\n{}", intro.unwrap_or(""));
    let hits = match_rules(map, &blob);
    if hits.is_empty() {
        return None;
    }
    let mut base = map.default_base.clone();
    let mut suppress: Option<f64> = None;
    let mut personality: Map<String, Value> = Map::new();
    let mut ids = Vec::new();

    for r in &hits {
        for (k, v) in &r.effect.base {
            base.insert(k.clone(), v.clone());
        }
        if let Some(s) = r.effect.suppress {
            suppress = Some(match suppress {
                None => s,
                Some(cur) => cur.max(s), // 压抑叠加取最大
            });
        }
        for (k, v) in &r.effect.personality {
            personality.insert(k.clone(), v.clone());
        }
        ids.push(r.id);
    }
    // 压抑词未命中：有「沉默/寡言」提示则 0.5
    if suppress.is_none() && (blob.contains("沉默") || blob.contains("寡言")) {
        suppress = Some(0.5);
    }
    // lambda 分档（照抄）
    let lam = if ["执念", "遗憾", "悔恨", "临终", "背负", "冤魂", "仇", "誓", "复仇", "丧", "孤儿", "孤"]
        .iter()
        .any(|p| blob.contains(p))
    {
        "0.95".to_string()
    } else if ["热血", "嬉皮", "玩世不恭", "轻佻", "洒脱", "天真"]
        .iter()
        .any(|p| blob.contains(p))
    {
        "0.7".to_string()
    } else {
        map.default_lambda.clone()
    };

    Some(Calibrated {
        emotion_base: base,
        suppress,
        personality,
        lambda: lam,
        rule_hits: ids,
    })
}

/// 单卡 ETL（对应 `main()` 里的单卡循环体，去掉 IO）。
pub fn etl_card(map: &CalibMap, card_text: &str, code: &str, name: &str, subdir: &str) -> Value {
    let sec = extract_sections(card_text);
    let xingge_blob = sec.xingge.join("\n");
    let res = calibrate(map, &xingge_blob, sec.intro.as_deref());

    let mut entry = json!({
        "code": code,
        "name": name,
        "subdir": subdir,
        "xingge_present": !sec.xingge.is_empty(),
    });
    // 【决策二·E级修复】entry 由 json!({...}) 构造，当前必为对象；但一旦上游改成
    // 其它结构，.unwrap() 会 panic。降级：非对象则原样返回（调用方拿到不完整 entry，
    // 好过整条 ETL 崩溃）。
    let Some(o) = entry.as_object_mut() else {
        return entry;
    };
    match res {
        None => {
            o.insert("emotion_base".into(), Value::Null);
            o.insert("personality".into(), Value::Null);
            o.insert("suppress".into(), Value::Null);
            o.insert("lambda".into(), Value::Null);
            o.insert("status".into(), json!("待标定"));
            o.insert(
                "note".into(),
                json!(if sec.xingge.is_empty() {
                    "性格段缺失或未命中映射规则"
                } else {
                    "性格段未命中映射规则"
                }),
            );
        }
        Some(c) => {
            o.insert("emotion_base".into(), Value::Object(c.emotion_base));
            o.insert(
                "personality".into(),
                if c.personality.is_empty() {
                    Value::Null
                } else {
                    Value::Object(c.personality.clone())
                },
            );
            o.insert("suppress".into(), json!(c.suppress));
            o.insert("lambda".into(), json!(c.lambda));
            o.insert("status".into(), json!("实值"));
            o.insert("rule_hits".into(), json!(c.rule_hits));
            o.insert(
                "field_status".into(),
                json!({
                    "emotion_base": "实值",
                    "personality": if c.personality.is_empty() { "待标定" } else { "实值" },
                    "suppress": if c.suppress.is_some() { "实值" } else { "待标定" },
                    "lambda": "实值",
                }),
            );
        }
    }
    entry
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_map() -> CalibMap {
        // 测试夹具（随 crate 一起入库，见 tests/fixtures/）
        let p = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/emotion_calib_map.json"
        ));
        let txt = std::fs::read_to_string(p).expect("读不到 tests/fixtures/emotion_calib_map.json");
        CalibMap::from_json_text(&txt).expect("解析失败")
    }

    #[test]
    fn map_parses_and_validates() {
        let m = load_map();
        assert_eq!(m.dims.len(), 10);
        assert_eq!(m.rules.len(), 12);
        assert_eq!(m.default_lambda, "0.85");
    }

    /// 回归：原文件损坏（rule3 缺 `}`、rule12 被截断）必须能被检出。
    #[test]
    fn detects_the_historical_corruption() {
        // 复现原始 bug 形态①：`effect.base` 对象缺右花括号 → 花括号不配平（语法非法）
        let broken_brace = r#"{"dims":["anger"],"default_base":{"anger":30},"default_lambda":"0.85",
            "rules":[{"id":3,"词":"温婉","effect":{"base":{"anger":15,"note":"x"}}}]"#;
        assert!(
            CalibMap::from_json_text(broken_brace).is_err(),
            "花括号不配平的 JSON 必须报错"
        );

        // 复现原始 bug 形态②：rule12 行被截断（缺 effect 闭合 + 缺 note + 尾逗号）
        let broken_trunc = r#"{"dims":["anger"],"default_base":{"anger":30},"default_lambda":"0.85",
            "rules":[
              {"id":11,"词":"共情","effect":{"base":{"anger":15}}},
              {"id":12,"词":"腹黑","effect":{"base":{"anger":50},"suppress":0.75,"personality":{"anger":1.3}
            ]}"#;
        assert!(
            CalibMap::from_json_text(broken_trunc).is_err(),
            "被截断（花括号不配平）的 JSON 必须报错"
        );
    }

    /// 结构校验：引用了 dims 之外的维度 → 报错（Python 版无此校验）。
    #[test]
    fn rejects_unknown_dimension_in_rule() {
        let bad = r#"{"dims":["anger"],"default_base":{"anger":30},"default_lambda":"0.85",
            "rules":[{"id":1,"词":"x","effect":{"base":{"fear":15}}}]}"#;
        let r = CalibMap::from_json_text(bad);
        assert!(r.is_err(), "rule 引用未知维度 fear 应报错");
        assert!(r.unwrap_err().to_string().contains("未知维度"));
    }

    #[test]
    fn extract_sections_finds_xingge_and_intro() {
        let card = "# CHAR-99-001｜陈寂\n> 介绍：一个穿越者\n### 性格特征\n- 隐忍坚韧\n- 沉默寡言\n### 基础信息\n**姓名**：陈寂\n";
        let s = extract_sections(card);
        assert!(s.xingge.iter().any(|l| l.contains("隐忍坚韧")), "{:?}", s.xingge);
        assert_eq!(s.intro.as_deref(), Some("> 介绍：一个穿越者"));
        assert!(s.base.iter().any(|l| l.contains("姓名")), "{:?}", s.base);
    }

    #[test]
    fn calibrate_hits_rule5_yinren() {
        let m = load_map();
        // 「隐忍|坚韧|克制|坚定|沉默寡言|寡言」→ rule5（suppress 0.6）
        let c = calibrate(&m, "隐忍坚韧，沉默寡言", None).unwrap();
        assert!(c.rule_hits.contains(&5), "命中 rule5；实际 {:?}", c.rule_hits);
        assert_eq!(c.suppress, Some(0.6));
        assert_eq!(c.lambda, "0.85");
    }

    #[test]
    fn calibrate_no_hit_returns_none() {
        let m = load_map();
        assert!(calibrate(&m, "完全无关的文本", None).is_none());
    }

    #[test]
    fn calibrate_lambda_tier_95_for_obsession() {
        let m = load_map();
        // 命中规则 + 含「执念」→ lambda 0.95
        let c = calibrate(&m, "冷峻 执念", None).unwrap();
        assert_eq!(c.lambda, "0.95");
    }

    #[test]
    fn calibrate_lambda_tier_70_for_frivolous() {
        let m = load_map();
        let c = calibrate(&m, "冷峻 玩世不恭", None).unwrap();
        assert_eq!(c.lambda, "0.7");
    }

    #[test]
    fn suppress_takes_max_across_rules() {
        let m = load_map();
        // rule1 喜怒不露(0.8) + rule5 隐忍(0.6) → max = 0.8
        let c = calibrate(&m, "喜怒不露 隐忍", None).unwrap();
        assert_eq!(c.suppress, Some(0.8), "压抑叠加取最大");
    }

    #[test]
    fn suppress_fallback_on_silence() {
        let m = load_map();
        // 命中 rule7（无 suppress），但含「沉默」→ 兜底 0.5
        let c = calibrate(&m, "玩世不恭 沉默", None).unwrap();
        assert_eq!(c.suppress, Some(0.5));
    }

    #[test]
    fn etl_card_shape() {
        let m = load_map();
        let card = "# CHAR-1｜甲\n### 性格\n- 隐忍\n";
        let e = etl_card(&m, card, "CHAR-1", "甲", "主角");
        assert_eq!(e["code"], "CHAR-1");
        assert_eq!(e["xingge_present"], json!(true));
        assert_eq!(e["status"], json!("实值"));
        assert_eq!(e["suppress"], json!(0.6));
    }
}
