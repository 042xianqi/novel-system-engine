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
                if whitelisted(term) {
                    continue;
                }
                if text.contains(term) {
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
                    out.push(Finding::with_sev(
                        code,
                        role,
                        format!("kbid={kbid}; term='{term}'; tag={tag}{tail}"),
                        sev,
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
    fn replay_deterministic() {
        let mut s = base_state();
        s["suppress"] = json!(1.5);
        let a = serde_json::to_string(&ooc_gate_json("重放", &s, None, CONFLICT_THR, None, None).unwrap()).unwrap();
        let b = serde_json::to_string(&ooc_gate_json("重放", &s, None, CONFLICT_THR, None, None).unwrap()).unwrap();
        assert_eq!(a, b);
    }
}
