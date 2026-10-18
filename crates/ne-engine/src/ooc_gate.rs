//! OOC 校验门禁 —— `ooc_gate.py` 的 Rust 忠实移植（5 判据，0 LLM，可重放）。
//!
//! 事实源：`05_管线框架/engine/ooc_gate.py`（441 行）
//! 依赖常量来自 `emotion_engine.py`：DIM_NAMES（10 情绪维）、REALM_ORDER（9 境界序）。
//!
//! 判据：
//!   1 DEAD_ROLE_APPEARS      死角色出场（alive=False）
//!   2 REALM_REGRESSION       境界倒退（realm < _realm_max）
//!   3 EMOTION_EVENT_CONFLICT |delta|>thr 且当前值<FLAT_FLOOR
//!   4 SUPPRESS_OUT_OF_RANGE  suppress ∉ [0,1]
//!   5 WORLD_CONSISTENCY      drift 命中 P0 / canon 违反 P1 / whitelist 豁免
//!
//! 判据 5 的**词表数据由调用方提供**（`&Value`），本模块不读盘——保持纯函数可测。

use anyhow::Result;
use serde::Serialize;
use serde_json::{json, Value};

/// 判据 3 阈值（照抄 CONFLICT_THR / FLAT_FLOOR）。
pub const CONFLICT_THR: f64 = 20.0;
pub const FLAT_FLOOR: f64 = 10.0;

/// 10 情绪维（照抄 emotion_engine.DIM_NAMES）。
pub const DIM_NAMES: [&str; 10] = [
    "anger", "fear", "grief", "joy", "anxiety", "calm", "shame", "awe", "compassion", "hope",
];

/// 境界序（照抄 emotion_engine.REALM_ORDER）。
pub const REALM_ORDER: [&str; 9] = [
    "炼气期",
    "逆流境",
    "水相熵流体悟",
    "凝响境",
    "共鸣境",
    "裂潮境",
    "渊核境",
    "逆熵九境",
    "残响至尊",
];

/// 输出三元组（+可选 severity）。字段名照抄 Python 的 dict 键。
#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub code: String,
    pub role: String,
    pub evidence: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
}

impl Finding {
    fn new(code: &str, role: &str, evidence: String) -> Self {
        Finding { code: code.into(), role: role.into(), evidence, severity: None }
    }
    fn with_sev(code: &str, role: &str, evidence: String, sev: &str) -> Self {
        Finding {
            code: code.into(),
            role: role.into(),
            evidence,
            severity: Some(sev.into()),
        }
    }
    pub fn to_json(&self) -> Value {
        let mut v = json!({
            "code": self.code,
            "role": self.role,
            "evidence": self.evidence,
        });
        if let Some(s) = &self.severity {
            v["severity"] = json!(s);
        }
        v
    }
}

fn role_of(char_state: &Value) -> String {
    char_state
        .get("char_code")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// 判据 1：死角色出场。
pub fn dead_role_appears(char_state: &Value, action_desc: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    // 对齐 Python: `char_state.get("alive") is False`
    let alive = char_state.get("alive");
    if alive == Some(&Value::Bool(false)) {
        out.push(Finding::new(
            "DEAD_ROLE_APPEARS",
            &role_of(char_state),
            format!("alive=false; action={action_desc:?}"),
        ));
    }
    out
}

/// 判据 2：境界倒退。
pub fn realm_regression(char_state: &Value, _action_desc: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    let realm = char_state
        .get("realm")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let hi = char_state
        .get("_realm_max")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let ri = REALM_ORDER.iter().position(|r| *r == realm);
    let hii = REALM_ORDER.iter().position(|r| *r == hi);
    if let (Some(ri), Some(hii)) = (ri, hii) {
        if ri < hii {
            out.push(Finding::new(
                "REALM_REGRESSION",
                &role_of(char_state),
                format!("realm={realm:?}; realm_max={hi:?}; order_idx={ri}/{hii}"),
            ));
        }
    }
    out
}

/// 判据 3：情绪-事件矛盾。
pub fn emotion_event_conflict(
    char_state: &Value,
    event_templates: Option<&Value>,
    thr: f64,
) -> Vec<Finding> {
    let mut out = Vec::new();
    let cur = char_state.get("emotions").cloned().unwrap_or_else(|| json!({}));
    let Some(evs) = event_templates.and_then(|v| v.as_object()) else {
        return out;
    };
    for (eid, tpl) in evs {
        let Some(delta) = tpl.get("emotion_delta").and_then(|v| v.as_object()) else {
            continue;
        };
        for (dim, dval) in delta {
            if !DIM_NAMES.contains(&dim.as_str()) {
                continue;
            }
            let Some(dv) = dval.as_f64().map(f64::abs) else {
                continue;
            };
            let cv = cur.get(dim).and_then(|v| v.as_f64()).unwrap_or(0.0);
            if dv > thr && cv < FLAT_FLOOR {
                out.push(Finding::new(
                    "EMOTION_EVENT_CONFLICT",
                    &role_of(char_state),
                    format!(
                        "event={eid}; dim={dim}; |delta|={dv:.1}>thr={thr:?}; cur={cv:.1}<floor={FLAT_FLOOR:?}"
                    ),
                ));
            }
        }
    }
    out
}

/// 判据 4：压抑系数越界。
pub fn suppress_out_of_range(char_state: &Value, action_desc: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    if let Some(sup) = char_state.get("suppress") {
        if !sup.is_null() {
            let sv = sup.as_f64();
            if let Some(sv) = sv {
                if sv < 0.0 || sv > 1.0 {
                    out.push(Finding::new(
                        "SUPPRESS_OUT_OF_RANGE",
                        &role_of(char_state),
                        format!("suppress={sv}; action={action_desc:?}; 合法域=[0,1]"),
                    ));
                }
            }
        }
    }
    out
}

/// 判据 5：世界一致性。
///
/// `kb` 结构照抄 `kb_world_consistency.json`：
///   { "drift_blacklist":[{term,kbid,tag,verdict}], "canon_invariants":[...], "whitelist":[{term}] }
/// 语义照抄 Python：
///   * term ∈ whitelist 或该 term 的所有出现都被白名单词覆盖 → 豁免
/// term 有效性守卫（2026-10-14 修缺陷 `term='in'` 误判 P0）。
///
/// 背景：`kb_world_consistency.json` 的 `drift_blacklist` 里混入了 LLM 精读角色卡时
/// 误抽出的英文单词（`in`/`the`/`but`/`her`/`all` 等 52 条含拉丁字母）。原实现
/// `text.contains(term)` 为纯子串匹配 → `term="in"` 命中任何含 `in` 的草稿
/// （实测：正文加一个 `innocent` 即被 P0 打回）。
///
/// 规则（不改判定口径，只排除明显不是词条的数据）：
///   * 含 CJK 的 term → 照常参与（既有行为不变）；
///   * 纯非 CJK（拉丁/数字/符号）→ 要求长度 ≥ 4，排除 `in`/`the`/`but` 类噪声。
fn is_valid_term(term: &str) -> bool {
    if term
        .chars()
        .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
    {
        return true;
    }
    term.chars().count() >= 4
}

/// 含 CJK 的 term，其出现是否是「独立词」而非更长词的一部分。
///
/// 纯拉丁 term 用词边界（`\b`）匹配，避免 `City` 命中 `Cityscape` 之类；
/// 含 CJK 的 term 保持原 `contains` 语义（中文无词边界，且既有测试依赖该行为）。
fn term_hit(term: &str, text: &str) -> bool {
    let has_cjk = term
        .chars()
        .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c));
    if has_cjk {
        return text.contains(term);
    }
    // 纯拉丁：要求两侧为非词字符（或串首/尾）
    let re = match regex::Regex::new(&format!(r"(?i)(?:^|[^A-Za-z0-9_]){}(?:$|[^A-Za-z0-9_])", regex::escape(term))) {
        Ok(r) => r,
        Err(_) => return text.contains(term),
    };
    re.is_match(text)
}

/// 极高频通用中文词表（2026-10-14 修同类缺陷 `term='组织'` 等）。
///
/// 背景：`drift_blacklist` 的 38 个 2 字中文 term 里混入了**日常常用词**
/// （`组织`/`标题`/`科技`/`文明`/`场景`/`描写`/`结构`/`战斗`/`生活`/`对话`/
/// `特殊`/`重复`/`换算`/`自报`/`同名`/`神话`/`禁词`）。这些词一旦出现在正文，
/// 原 `contains` 逻辑会把整章判 `P0 drift` 打回——与 `term='in'` 是同一类问题。
///
/// **处理口径（不改判定语义）**：这类词命中时**仍报，但降级为 P1 告警**，
/// 保留人工复核入口，不直接 P0 阻断生成。真正的专有名词（`萧炎`/`慧能`/`鸿钧`）
/// 不在表内，P0 强度不变。
const HIGH_FREQ_COMMON_TERMS: &[&str] = &[
    "组织", "标题", "科技", "文明", "场景", "描写", "结构", "战斗", "生活",
    "对话", "特殊", "重复", "换算", "自报", "同名", "神话", "禁词", "标题",
    "形式", "内容", "设定", "规则", "资料", "信息", "状态", "数据", "字段",
    // 2026-10-16 补：canon_invariants 里实测混入的**非专名日常/工程词**
    // （出现在正文属正常，不应告警）。注意：境界名（炼气/筑基/金丹/元婴/
    // 化神/炼虚/合体/大乘/渡劫）与正典专名（天启/归墟/九阶/四路）**不在此表**，
    // 它们是真实 canon 约束，必须保留告警强度。
    "回写", "主纪", "不强统", "单独保留", "标注待补", "远期预留", "合法地名",
];

/// 该 term 是否属于「高频通用词」（命中降级为 P1 而非 P0）。
fn is_high_freq_common(term: &str) -> bool {
    HIGH_FREQ_COMMON_TERMS.contains(&term)
}

///   * drift 命中 → P0（code=WORLD_CONSISTENCY_DRIFT）
///   * canon 命中 → P1（code=WORLD_CONSISTENCY_CANON）
pub fn world_consistency(draft_text: Option<&str>, kb: Option<&Value>, role: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    let Some(text) = draft_text else { return out };
    if text.is_empty() {
        return out;
    }
    let Some(kb) = kb else { return out };

    let wl: Vec<String> = kb
        .get("whitelist")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.get("term").and_then(|t| t.as_str()))
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();

    // 白名单词在正文中的所有跨度
    let mut wl_spans: Vec<(usize, usize)> = Vec::new();
    for w in &wl {
        for m in regex::Regex::new(&regex::escape(w))
            .unwrap()
            .find_iter(text)
        {
            wl_spans.push((m.start(), m.end()));
        }
    }

    // term 的所有出现是否都被白名单覆盖
    let whitelisted = |term: &str| -> bool {
        let re = match regex::Regex::new(&regex::escape(term)) {
            Ok(r) => r,
            Err(_) => return false,
        };
        let mut any = false;
        for m in re.find_iter(text) {
            any = true;
            let pos = m.start();
            let covered = wl_spans.iter().any(|(s, e)| *s <= pos && pos < *e);
            if !covered {
                return false;
            }
        }
        any
    };

    let scan = |key: &str, code: &str, sev: &str, out: &mut Vec<Finding>| {
        if let Some(arr) = kb.get(key).and_then(|v| v.as_array()) {
            for e in arr {
                let Some(term) = e.get("term").and_then(|t| t.as_str()) else {
                    continue;
                };
                if term.is_empty() || wl.iter().any(|w| w == term) {
                    continue;
                }
                if !is_valid_term(term) {
                    continue;
                }
                if whitelisted(term) {
                    continue;
                }
                if term_hit(term, text) {
                    let kbid = e.get("kbid").map(|v| v.to_string()).unwrap_or_default();
                    let tag = e.get("tag").and_then(|t| t.as_str()).unwrap_or("");
                    // 对齐 Python：drift 条目带 verdict + "命中草稿=P0打回"；
                    // canon 条目带 "违反canon基准=P1告警"（Python 原文即如此）。
                    let tail = if code == "WORLD_CONSISTENCY_DRIFT" {
                        let verdict = e.get("verdict").and_then(|v| v.as_str()).unwrap_or("");
                        format!("; verdict={verdict}; 命中草稿=P0打回")
                    } else {
                        "; 违反canon基准=P1告警".to_string()
                    };
                    // 高频通用词命中：仍报，但降级 P1（避免 `组织`/`文明` 这类
                    // 日常词把整章 P0 打回，同 `term='in'` 一类问题）。
                    let (sev_eff, tag_note) =
                        if code == "WORLD_CONSISTENCY_DRIFT" && is_high_freq_common(term) {
                            ("P1", "；高频通用词→降级P1（待人工复核）")
                        } else {
                            (sev, "")
                        };
                    out.push(Finding::with_sev(
                        code,
                        role,
                        format!("kbid={kbid}; term='{term}'; tag={tag}{tail}{tag_note}"),
                        sev_eff,
                    ));
                }
            }
        }
    };

    scan("drift_blacklist", "WORLD_CONSISTENCY_DRIFT", "P0", &mut out);
    scan("canon_invariants", "WORLD_CONSISTENCY_CANON", "P1", &mut out);
    out
}

/// 统一入口：5 判据并集（顺序 1→2→3→4→5，同 code 去重保留首条）。
pub fn ooc_gate(
    action_desc: &str,
    char_state: &Value,
    event_templates: Option<&Value>,
    thr: f64,
    draft_text: Option<&str>,
    kb: Option<&Value>,
) -> Vec<Finding> {
    let mut preds: Vec<Vec<Finding>> = vec![
        dead_role_appears(char_state, action_desc),
        realm_regression(char_state, action_desc),
        emotion_event_conflict(char_state, event_templates, thr),
        suppress_out_of_range(char_state, action_desc),
    ];
    if draft_text.is_some() {
        preds.push(world_consistency(draft_text, kb, &role_of(char_state)));
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for g in preds {
        for w in g {
            if seen.insert(w.code.clone()) {
                out.push(w);
            }
        }
    }
    out
}

/// 便捷：返回 JSON 数组（供 CLI / MCP）。
pub fn ooc_gate_json(
    action_desc: &str,
    char_state: &Value,
    event_templates: Option<&Value>,
    thr: f64,
    draft_text: Option<&str>,
    kb: Option<&Value>,
) -> Result<Value> {
    let v: Vec<Value> = ooc_gate(action_desc, char_state, event_templates, thr, draft_text, kb)
        .iter()
        .map(|f| f.to_json())
        .collect();
    Ok(Value::Array(v))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn base_state() -> Value {
        let emo: serde_json::Map<String, Value> =
            DIM_NAMES.iter().map(|d| (d.to_string(), json!(20.0))).collect();
        json!({
            "char_code": "CHAR-99-001",
            "char_name": "陈寂",
            "alive": true,
            "realm": "共鸣境",
            "_realm_max": "共鸣境",
            "suppress": 0.6,
            "emotions": Value::Object(emo),
        })
    }

    #[test]
    fn all_pass_state_is_empty() {
        let ev = json!({"EVT-X": {"emotion_delta": {"fear": 30.0}}});
        let w = ooc_gate("常态", &base_state(), Some(&ev), CONFLICT_THR, None, None);
        assert!(w.is_empty(), "{w:?}");
    }

    #[test]
    fn dead_role_detected() {
        let mut s = base_state();
        s["alive"] = json!(false);
        let w = ooc_gate("死者复活", &s, None, CONFLICT_THR, None, None);
        assert!(w.iter().any(|f| f.code == "DEAD_ROLE_APPEARS"));
        assert_eq!(w[0].role, "CHAR-99-001");
    }

    #[test]
    fn realm_regression_detected() {
        let mut s = base_state();
        s["realm"] = json!("凝响境");
        s["_realm_max"] = json!("共鸣境");
        let w = ooc_gate("境界回退", &s, None, CONFLICT_THR, None, None);
        assert!(w.iter().any(|f| f.code == "REALM_REGRESSION"));
    }

    #[test]
    fn emotion_conflict_needs_flat_floor() {
        // fear=5 < 10 且 |30|>20 → 触发
        let mut s = base_state();
        s["emotions"]["fear"] = json!(5.0);
        let ev = json!({"EVT-Y": {"emotion_delta": {"fear": 30.0}}});
        let w = ooc_gate("平惧骤惊", &s, Some(&ev), CONFLICT_THR, None, None);
        assert!(w.iter().any(|f| f.code == "EMOTION_EVENT_CONFLICT"));
        // 对照：fear=20 ≥ 10 → 不触发
        let w2 = ooc_gate("常态", &base_state(), Some(&ev), CONFLICT_THR, None, None);
        assert!(!w2.iter().any(|f| f.code == "EMOTION_EVENT_CONFLICT"));
    }

    #[test]
    fn suppress_out_of_range_detected() {
        let mut s = base_state();
        s["suppress"] = json!(1.5);
        let w = ooc_gate("压抑越界", &s, None, CONFLICT_THR, None, None);
        assert!(w.iter().any(|f| f.code == "SUPPRESS_OUT_OF_RANGE"));
    }

    #[test]
    fn world_consistency_drift_blocks_canon_warns() {
        let kb = json!({
            "drift_blacklist": [{"term":"霍去病","kbid":12,"tag":"现实混入","verdict":"drift"}],
            "canon_invariants": [{"term":"腐朽世界已删","kbid":7,"tag":"世界观"}],
            "whitelist": [{"term":"热力学"}],
        });
        let w = world_consistency(Some("霍去病在归墟海修炼"), Some(&kb), "CHAR-99-001");
        assert!(w.iter().any(|f| f.code == "WORLD_CONSISTENCY_DRIFT" && f.severity.as_deref() == Some("P0")));
        let w2 = world_consistency(Some("腐朽世界已删 浊界历4127年"), Some(&kb), "C");
        assert!(w2.iter().any(|f| f.code == "WORLD_CONSISTENCY_CANON" && f.severity.as_deref() == Some("P1")));
        let w3 = world_consistency(Some("太玄历247年，陈寂在青牛镇修炼"), Some(&kb), "C");
        assert!(w3.is_empty(), "{w3:?}");
    }

    #[test]
    fn whitelist_covers_term_exempts() {
        // 白名单词「热力学」覆盖「力学」→ 不判 drift
        let kb = json!({
            "drift_blacklist": [{"term":"力学","kbid":1,"tag":"x"}],
            "canon_invariants": [],
            "whitelist": [{"term":"热力学"}],
        });
        let w = world_consistency(Some("热力学的熵增"), Some(&kb), "C");
        assert!(w.is_empty(), "被白名单覆盖应豁免: {w:?}");
    }

    #[test]
    fn draft_text_none_disables_predicate5() {
        let kb = json!({"drift_blacklist":[{"term":"霍去病"}],"canon_invariants":[],"whitelist":[]});
        let w = ooc_gate("试水", &base_state(), None, CONFLICT_THR, None, Some(&kb));
        assert!(!w.iter().any(|f| f.code.starts_with("WORLD_CONSISTENCY")));
    }

    #[test]
    fn short_latin_term_not_matched() {
        // 缺陷复现：term="in" 曾让任何含 in 的草稿 P0 打回
        let kb = json!({
            "drift_blacklist": [{"term":"in","kbid":92,"tag":"形式"}],
            "canon_invariants": [],
            "whitelist": [],
        });
        let w = world_consistency(Some("他走进一间innocent的房子"), Some(&kb), "C");
        assert!(w.is_empty(), "单字母拉丁词不应命中: {w:?}");
    }
    #[test]
    fn latin_term_uses_word_boundary() {
        let kb = json!({
            "drift_blacklist": [{"term":"City","kbid":87,"tag":"现实混入"}],
            "canon_invariants": [],
            "whitelist": [],
        });
        // 独立词命中
        let w = hits("他和 City 签了约", &kb);
        assert!(w, "独立词应命中");
        // 词内子串不命中
        let w2 = hits("Cityscape 一片", &kb);
        assert!(!w2, "更长词的一部分不应命中");
        fn hits(t: &str, kb: &serde_json::Value) -> bool {
            world_consistency(Some(t), Some(kb), "C")
                .iter()
                .any(|f| f.code == "WORLD_CONSISTENCY_DRIFT")
        }
    }
    #[test]
    fn cjk_term_still_substring() {
        // 中文 term 行为不变（无词边界，既有测试依赖）
        let kb = json!({
            "drift_blacklist": [{"term":"太玄教","kbid":48,"tag":"自造文明"}],
            "canon_invariants": [],
            "whitelist": [],
        });
        let w = world_consistency(Some("他加入了太玄教的旧部"), Some(&kb), "C");
        assert!(w.iter().any(|f| f.code == "WORLD_CONSISTENCY_DRIFT"));
    }
    #[test]
fn high_freq_common_term_downgraded_to_p1() {
    // `组织` 是日常常用词，命中不应把整章 P0 打回（同 term='in' 一类问题）
    let kb = json!({
        "drift_blacklist": [{"term":"组织","kbid":38,"tag":"死引用"}],
        "canon_invariants": [],
        "whitelist": [],
    });
    let w = world_consistency(Some("他组织了一次行动"), Some(&kb), "C");
    assert!(!w.is_empty(), "仍应报（保留复核入口）");
    assert!(
        w.iter().all(|f| f.severity.as_deref() == Some("P1")),
        "高频通用词应降级 P1: {w:?}"
    );
}
#[test]
fn proper_noun_still_p0() {
    // 专有名词（现实混入）P0 强度不变
    let kb = json!({
        "drift_blacklist": [{"term":"萧炎","kbid":12,"tag":"现实混入"}],
        "canon_invariants": [],
        "whitelist": [],
    });
    let w = world_consistency(Some("萧炎出现了"), Some(&kb), "C");
    assert!(w.iter().any(|f| f.severity.as_deref() == Some("P0")), "{w:?}");
}
#[test]
fn replay_deterministic() {
        let mut s = base_state();
        s["suppress"] = json!(1.5);
        let a = serde_json::to_string(&ooc_gate_json("重放", &s, None, CONFLICT_THR, None, None).unwrap()).unwrap();
        let b = serde_json::to_string(&ooc_gate_json("重放", &s, None, CONFLICT_THR, None, None).unwrap()).unwrap();
        assert_eq!(a, b);
    }
}
