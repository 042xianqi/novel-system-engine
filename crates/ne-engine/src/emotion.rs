//! 情绪引擎 —— `emotion_engine.py` 的 Rust 忠实移植（P0 五公式 + P1 sigmoid/分支）。
//!
//! 事实源：`05_管线框架/engine/emotion_engine.py`（671 行）
//!
//! 移植要点：
//!   * `step()` 五阶段（S1读态→S3递推ΔE→S4惯性→S5耦合→S7表现/S8统计）**逐字对齐**
//!   * `_round4` 全程 4 位小数，保证与 Python 逐字一致
//!   * `branch_probs(seed)` 用 `pyrandom::PyRandom` 复刻 CPython MT19937（已对拍）
//!   * `ooc_check` 是 **k5 原生口径**（判据3 为 `sign(delta)*cur < -30`），
//!     与 `ooc_gate.py` 的口径（`|delta|>20 且 cur<10`）**不同**——两者都保留。
//!
//! 落库：`to_char_emotion_rows` / `to_char_memory_rows` 只构造行，不连 DB（对齐 Python 设计）。

use anyhow::{anyhow, Result};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

use crate::pyrandom::PyRandom;

/// 10 情绪维（照抄 EMO_DIMS 的英文键）。
pub const DIM_NAMES: [&str; 10] = [
    "anger", "fear", "grief", "joy", "anxiety", "calm", "shame", "awe", "compassion", "hope",
];

/// 常量（照抄）。
pub const MAX_STEP: f64 = 10.0;
pub const MEMORY_DROP: f64 = 5.0;
pub const OOC_CONFLICT_THRESHOLD: f64 = 30.0;
pub const COUPLE_EPS: f64 = 1e-9;
pub const SIGMOID_THRESHOLD: f64 = 50.0;

/// 境界序（照抄 REALM_ORDER）。
pub const REALM_ORDER: [&str; 9] = [
    "炼气期", "逆流境", "水相熵流体悟", "凝响境", "共鸣境", "裂潮境", "渊核境", "逆熵九境",
    "残响至尊",
];

fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    v.max(lo).min(hi)
}

/// 对齐 Python `_round4`（round(float, 4)）。
/// 注意：Rust 的 `f64::round` 是 round-half-away-from-zero，Python 3 的 `round` 是
/// banker's rounding（round-half-to-even）。为逐字一致，这里实现 half-to-even。
pub fn round4(x: f64) -> f64 {
    round_half_even(x, 4)
}

fn round_half_even(x: f64, digits: u32) -> f64 {
    let m = 10f64.powi(digits as i32);
    let scaled = x * m;
    let floor = scaled.floor();
    let diff = scaled - floor;
    let r = if (diff - 0.5).abs() < 1e-9 {
        // 半值 → 取偶
        if (floor as i64) % 2 == 0 {
            floor
        } else {
            floor + 1.0
        }
    } else {
        scaled.round()
    };
    r / m
}

/// 默认标定（照抄 DEFAULT_CALIB）。
fn default_personality() -> BTreeMap<String, f64> {
    DIM_NAMES.iter().map(|d| (d.to_string(), 1.0)).collect()
}

/// 默认耦合规则（照抄 DEFAULT_COUPLING：fear > 70 → anger × 0.4）。
fn default_coupling() -> Value {
    json!([{ "when": {"dim":"fear","op":">","value":70.0}, "target":"anger", "factor":0.4 }])
}

/// 补全标定（照抄 `_calib_get`）。返回 (personality, suppress, lambda_mem, coupling)。
fn calib_get(calib: Option<&Value>) -> (BTreeMap<String, f64>, f64, f64, Value) {
    let c = calib.unwrap_or(&Value::Null);
    let mut p = default_personality();
    if let Some(pp) = c.get("personality").and_then(|v| v.as_object()) {
        for (k, v) in pp {
            if let Some(f) = v.as_f64() {
                p.insert(k.clone(), f);
            }
        }
    }
    let suppress = c.get("suppress").and_then(|v| v.as_f64()).unwrap_or(1.0);
    let lm = c.get("lambda_mem").and_then(|v| v.as_f64()).unwrap_or(0.3);
    let coupling = match c.get("coupling") {
        Some(v) if !v.is_null() => v.clone(),
        _ => default_coupling(),
    };
    (p, suppress, lm, coupling)
}

/// 初始状态（照抄 initial_state）。
pub fn initial_state(char_code: &str, char_name: &str) -> Value {
    let zeros: Map<String, Value> =
        DIM_NAMES.iter().map(|d| (d.to_string(), json!(0.0))).collect();
    json!({
        "char_code": char_code,
        "char_name": char_name,
        "chapter": 0,
        "alive": true,
        "realm": "",
        "suppress": 1.0,
        "emotions": Value::Object(zeros.clone()),
        "emotions_show": Value::Object(zeros),
        "memories": [],
        "dominant": Value::Null,
        "strength": 0.0,
    })
}

/// 新记忆条目（照抄 new_memory）。
pub fn new_memory(char_code: &str, event_id: &str, source_chapter: u32, dim: &str, strength: f64) -> Value {
    json!({
        "char_code": char_code,
        "event_id": event_id,
        "source_chapter": source_chapter,
        "dim": dim,
        "e0": strength,
    })
}

/// 公式 4：记忆衰减（照抄 decay_memory）。返回 (e_t, keep)。
pub fn decay_memory(mem: &Value, chapter: u32, lambda_mem: f64) -> (f64, bool) {
    let sc = mem.get("source_chapter").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let t = chapter.saturating_sub(sc) as i64;
    let e0 = mem.get("e0").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let e_t = e0 * (-lambda_mem * t as f64).exp();
    (round4(e_t), e_t >= MEMORY_DROP - COUPLE_EPS)
}

/// 记忆残响叠加（照抄 memory_influence）。
pub fn memory_influence(memories: &[Value], chapter: u32, lambda_mem: f64) -> BTreeMap<String, f64> {
    let mut acc: BTreeMap<String, f64> = DIM_NAMES.iter().map(|d| (d.to_string(), 0.0)).collect();
    for m in memories {
        let (_e_t, keep) = decay_memory(m, chapter, lambda_mem);
        let dim = m.get("dim").and_then(|v| v.as_str()).unwrap_or("");
        if keep && acc.contains_key(dim) {
            let e_t = m.get("e0").and_then(|v| v.as_f64()).unwrap_or(0.0)
                * (-lambda_mem * (chapter.saturating_sub(
                    m.get("source_chapter").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                )) as f64)
                    .exp();
            *acc.get_mut(dim).unwrap() += round4(e_t);
        }
    }
    acc
}

/// 公式 6：耦合修正（照抄 apply_coupling）。
pub fn apply_coupling(emotions: &Map<String, Value>, rules: &Value) -> Map<String, Value> {
    let mut out = emotions.clone();
    let Some(arr) = rules.as_array() else { return out };
    for r in arr {
        let Some(when) = r.get("when") else { continue };
        let dim = when.get("dim").and_then(|v| v.as_str()).unwrap_or("");
        let tgt = r.get("target").and_then(|v| v.as_str()).unwrap_or("");
        let f = r.get("factor").and_then(|v| v.as_f64()).unwrap_or(1.0);
        let cur = out.get(dim).and_then(|v| v.as_f64()).unwrap_or(0.0);
        let v = when.get("value").and_then(|x| x.as_f64()).unwrap_or(0.0);
        let op = when.get("op").and_then(|x| x.as_str()).unwrap_or(">");
        let hit = match op {
            "==" => (cur - v).abs() < f64::EPSILON,
            "!=" => (cur - v).abs() >= f64::EPSILON,
            ">" => cur > v,
            ">=" => cur >= v,
            "<" => cur < v,
            "<=" => cur <= v,
            _ => false,
        };
        if hit {
            if let Some(t) = out.get(tgt).and_then(|x| x.as_f64()) {
                out.insert(tgt.to_string(), json!(round4(t * f)));
            }
        }
    }
    out
}

/// Sigmoid（照抄）。
pub fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

/// S2→S3 爆发增益（照抄 sigmoid_gain）。
pub fn sigmoid_gain(cumstim: f64, threshold: f64) -> f64 {
    if cumstim < threshold {
        return 1.0;
    }
    2.0 * sigmoid((cumstim - threshold) / (threshold / 2.0))
}

/// 分支概率（照抄 branch_probs；seed=None → 恒 None）。
pub fn branch_probs(emotions: &Value, seed: Option<i64>, threshold: f64) -> Option<Value> {
    let seed = seed?;
    let em = emotions.get("emotions").unwrap_or(emotions);
    let dom = emotions.get("dominant").and_then(|v| v.as_str());
    let strength = emotions.get("strength").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let dom_val = match dom {
        Some(d) => em.get(d).and_then(|v| v.as_f64()).unwrap_or(0.0),
        None => 0.0,
    };
    let p_base = if strength > COUPLE_EPS { dom_val / strength } else { 0.5 };
    let cumstim = strength;
    let prob = p_base * sigmoid_gain(cumstim, threshold);
    let mut rng = PyRandom::from_int(seed);
    let r = rng.random();
    if r < prob {
        let branch = match dom {
            Some(d) => format!("BRANCH-{d}"),
            None => "BRANCH-neutral".to_string(),
        };
        return Some(json!({
            "branch": branch,
            "dominant": dom,
            "strength": round4(strength),
            "cumstim": round4(cumstim),
            "prob": round4(prob),
            "r": round4(r),
            "seed": seed,
        }));
    }
    None
}

/// 核心：一步情绪演算（照抄 step，五公式）。
pub fn step(chapter: u32, triggered_events: &Value, char_state: &Value, calib: Option<&Value>) -> Result<Value> {
    let (p, suppress, lm, coupling) = calib_get(calib);
    let char_code = char_state.get("char_code").and_then(|v| v.as_str()).unwrap_or("").to_string();

    // S1 读状态
    let prev_chapter = char_state.get("chapter").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let em_old_obj = char_state.get("emotions").cloned().unwrap_or_else(|| json!({}));
    let mut em_old: BTreeMap<String, f64> = BTreeMap::new();
    for d in DIM_NAMES {
        em_old.insert(d.to_string(), em_old_obj.get(d).and_then(|v| v.as_f64()).unwrap_or(0.0));
    }
    let memories: Vec<Value> = char_state
        .get("memories")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut mem_list = memories.clone();

    // S2 sigmoid 增益
    let strength0 = char_state.get("strength").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let s2_gain = sigmoid_gain(strength0, SIGMOID_THRESHOLD);

    // S3 递推
    let influence = memory_influence(&memories, chapter, lm);
    let mut em_new = em_old.clone();
    let events_arr = triggered_events.as_array().cloned().unwrap_or_default();
    for ev in &events_arr {
        if let Some(roles) = ev.get("affected_roles").and_then(|v| v.as_array()) {
            if !roles.iter().any(|r| r.as_str() == Some(char_code.as_str())) {
                continue;
            }
        }
        let i = clamp(ev.get("i").and_then(|v| v.as_f64()).unwrap_or(0.5), 0.0, 1.0);
        let delta_tpl = ev.get("emotion_delta").cloned().unwrap_or_else(|| json!({}));
        if let Some(obj) = delta_tpl.as_object() {
            for (dim, dval) in obj {
                if !DIM_NAMES.contains(&dim.as_str()) {
                    return Err(anyhow!("step: 未知情绪维度 {dim:?}（合法：{DIM_NAMES:?}）"));
                }
                let p_mult = p.get(dim).copied().unwrap_or(1.0);
                let cur = em_new.get(dim).copied().unwrap_or(em_old[dim]);
                let dvv = dval.as_f64().unwrap_or(0.0);
                em_new.insert(dim.clone(), cur + s2_gain * i * p_mult * dvv);
            }
        }
        // 记忆生成
        let mut md = ev.get("memory_dim").and_then(|v| v.as_str()).map(|s| s.to_string());
        if md.is_none() {
            if let Some(obj) = delta_tpl.as_object() {
                if !obj.is_empty() {
                    md = obj
                        .iter()
                        .max_by(|a, b| {
                            a.1.as_f64().unwrap_or(0.0).abs().partial_cmp(&b.1.as_f64().unwrap_or(0.0).abs()).unwrap()
                        })
                        .map(|(k, _)| k.clone());
                }
            }
        }
        if let Some(md) = md {
            let ms = match ev.get("memory_strength").and_then(|v| v.as_f64()) {
                Some(m) => m,
                None => {
                    let maxv = delta_tpl
                        .as_object()
                        .map(|o| o.values().filter_map(|v| v.as_f64()).fold(f64::NEG_INFINITY, f64::max))
                        .unwrap_or(50.0);
                    maxv.abs().max(50.0)
                }
            };
            let sc = if prev_chapter > 0 { prev_chapter } else { chapter };
            mem_list.push(new_memory(
                &char_code,
                ev.get("event_id").and_then(|v| v.as_str()).unwrap_or(""),
                sc,
                &md,
                ms,
            ));
        }
    }

    // 旧章残响叠加（t≥1）
    for (dim, val) in &influence {
        let t0: f64 = mem_list
            .iter()
            .filter(|m| {
                m.get("dim").and_then(|v| v.as_str()) == Some(dim.as_str())
                    && m.get("source_chapter").and_then(|v| v.as_u64()).unwrap_or(0) == chapter as u64
            })
            .map(|m| m.get("e0").and_then(|v| v.as_f64()).unwrap_or(0.0))
            .sum();
        let residual = val - t0;
        if residual > COUPLE_EPS {
            em_new.insert(dim.clone(), em_old[dim] + residual);
        }
    }

    // 裁剪 0-100
    let em_new: BTreeMap<String, f64> =
        em_new.iter().map(|(k, v)| (k.clone(), clamp(*v, 0.0, 100.0))).collect();

    // S4 惯性
    let em_final: BTreeMap<String, f64> = DIM_NAMES
        .iter()
        .map(|d| {
            let o = em_old[*d];
            let n = em_new.get(*d).copied().unwrap_or(o);
            (d.to_string(), round4(o + clamp(n - o, -MAX_STEP, MAX_STEP)))
        })
        .collect();

    // S5 耦合
    let em_final_obj: Map<String, Value> =
        em_final.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
    let em_coupled = apply_coupling(&em_final_obj, &coupling);

    // S6 记忆清理
    let mut live_memories = Vec::new();
    for m in &mem_list {
        let (e_t, keep) = decay_memory(m, chapter, lm);
        if keep {
            let mut m2 = m.clone();
            m2["e_t"] = json!(e_t);
            live_memories.push(m2);
        }
    }

    // S7 表现
    let em_show: Map<String, Value> = em_coupled
        .iter()
        .map(|(k, v)| (k.clone(), json!(round4(v.as_f64().unwrap_or(0.0) * suppress))))
        .collect();

    // S8 统计
    let strength: f64 = em_coupled
        .values()
        .map(|v| v.as_f64().unwrap_or(0.0).powi(2))
        .sum::<f64>()
        .sqrt();
    // dominant：最大项，并列取 DIM_NAMES 先出现者
    let mut dominant: Option<String> = None;
    let mut best = f64::NEG_INFINITY;
    for d in DIM_NAMES {
        let v = em_coupled.get(d).and_then(|x| x.as_f64()).unwrap_or(0.0);
        if v > best {
            best = v;
            dominant = Some(d.to_string());
        }
    }
    let all_zero = em_coupled
        .values()
        .all(|v| v.as_f64().unwrap_or(0.0) <= COUPLE_EPS);
    if all_zero {
        dominant = None;
    }

    let mut new_state = char_state.clone();
    new_state["chapter"] = json!(chapter);
    new_state["emotions"] = Value::Object(em_coupled);
    new_state["emotions_show"] = Value::Object(em_show);
    new_state["memories"] = json!(live_memories);
    new_state["dominant"] = match dominant {
        Some(d) => json!(d),
        None => Value::Null,
    };
    new_state["strength"] = json!(round4(strength));
    new_state["suppress"] = json!(suppress);
    Ok(new_state)
}

/// 定案签名别名（照抄 emotion_step）。
pub fn emotion_step(chapter: u32, triggered: &Value, state: &Value, calib: Option<&Value>) -> Result<Value> {
    step(chapter, triggered, state, calib)
}

/// k5 原生口径 OOC 校验（照抄 emotion_engine.ooc_check，**4 判据**）。
///
/// ⚠ 与 `ooc_gate.rs::ooc_gate` 的判据 3 **口径不同**（这里 sign*cur < -30）。
/// 两者都保留：本函数是 k5 原生，ooc_gate.rs 是 k7 门禁层。
pub fn ooc_check(action_desc: &str, char_state: &Value, event_templates: Option<&Value>) -> Vec<Value> {
    let mut warnings = Vec::new();
    let mut codes: Vec<String> = Vec::new();
    let code_of = |cs: &Value| cs.get("char_code").and_then(|v| v.as_str()).unwrap_or("?").to_string();

    // 判据 1
    if char_state.get("alive") == Some(&Value::Bool(false)) {
        warnings.push(json!({
            "code":"DEAD_ROLE_APPEARS","severity":"P0",
            "message": format!("{} 已死亡（alive=false）却出场", code_of(char_state)),
            "evidence": format!("action={action_desc:?}; alive=false"),
        }));
        codes.push("DEAD_ROLE_APPEARS".into());
    }

    // 判据 2
    let realm = char_state.get("realm").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if let Some(ri) = REALM_ORDER.iter().position(|r| *r == realm) {
        if let Some(hi) = char_state.get("_realm_max").and_then(|v| v.as_str()) {
            if let Some(hii) = REALM_ORDER.iter().position(|r| *r == hi) {
                if ri < hii {
                    warnings.push(json!({
                        "code":"REALM_REGRESSION","severity":"P0",
                        "message": format!("境界倒退：{realm} 低于历史最高 {hi}"),
                        "evidence": format!("realm={realm:?}; realm_max={hi:?}"),
                    }));
                    codes.push("REALM_REGRESSION".into());
                }
            }
        }
    }

    // 判据 3（k5 口径：sign(delta)*cur < -30）
    if let Some(evs) = event_templates.and_then(|v| v.as_object()) {
        'outer: for (eid, tpl) in evs {
            let Some(delta) = tpl.get("emotion_delta").and_then(|v| v.as_object()) else {
                continue;
            };
            for (dim, dval) in delta {
                if !DIM_NAMES.contains(&dim.as_str()) {
                    continue;
                }
                let cur = char_state
                    .get("emotions")
                    .and_then(|e| e.get(dim))
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let dv = dval.as_f64().unwrap_or(0.0);
                let sign = if dv != 0.0 { dv / dv.abs() } else { 1.0 };
                let score = sign * cur;
                if score < -OOC_CONFLICT_THRESHOLD {
                    warnings.push(json!({
                        "code":"EMOTION_EVENT_CONFLICT","severity":"P1",
                        "message": format!("事件 {eid} 期望 {dim} 升 {dv}，但角色当前 {dim}={cur:.1}"),
                        "evidence": format!("event={eid}; dim={dim}; delta={dv}; cur={cur:.1}"),
                    }));
                    codes.push("EMOTION_EVENT_CONFLICT".into());
                    break 'outer;
                }
            }
        }
    }

    // 判据 4
    if let Some(sup) = char_state.get("suppress").and_then(|v| v.as_f64()) {
        if !(0.0..=1.0).contains(&sup) {
            warnings.push(json!({
                "code":"SUPPRESS_OUT_OF_RANGE","severity":"P0",
                "message": format!("压抑系数 suppress={sup} 越界（合法 0-1）"),
                "evidence": format!("suppress={sup}"),
            }));
        }
    }

    warnings
}

/// new_state → char_emotion 表行（照抄 to_char_emotion_rows）。
pub fn to_char_emotion_rows(new_state: &Value) -> Vec<Value> {
    let mut row = Map::new();
    row.insert("char_code".into(), new_state.get("char_code").cloned().unwrap_or(json!("")));
    row.insert("chapter".into(), new_state.get("chapter").cloned().unwrap_or(json!(0)));
    for d in DIM_NAMES {
        row.insert(
            d.to_string(),
            json!(round4(new_state.get("emotions").and_then(|e| e.get(d)).and_then(|v| v.as_f64()).unwrap_or(0.0))),
        );
    }
    row.insert("dominant".into(), new_state.get("dominant").cloned().unwrap_or(Value::Null));
    row.insert("strength".into(), new_state.get("strength").cloned().unwrap_or(json!(0.0)));
    vec![Value::Object(row)]
}

/// new_state.memories → char_memory 表行（照抄 to_char_memory_rows）。
pub fn to_char_memory_rows(new_state: &Value) -> Vec<Value> {
    let mut rows = Vec::new();
    if let Some(arr) = new_state.get("memories").and_then(|v| v.as_array()) {
        for m in arr {
            rows.push(json!({
                "char_code": m.get("char_code").and_then(|v| v.as_str()).unwrap_or_else(|| new_state.get("char_code").and_then(|v| v.as_str()).unwrap_or("")),
                "event_id": m.get("event_id").and_then(|v| v.as_str()).unwrap_or(""),
                "source_chapter": m.get("source_chapter").cloned().unwrap_or(json!(0)),
                "dim": m.get("dim").and_then(|v| v.as_str()).unwrap_or(""),
                "e0": round4(m.get("e0").and_then(|v| v.as_f64()).unwrap_or(0.0)),
                "e_t": m.get("e_t").cloned().unwrap_or(Value::Null),
            }));
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn initial_state_shape() {
        let s = initial_state("CHAR-99-001", "陈寂");
        assert_eq!(s["char_code"], "CHAR-99-001");
        assert_eq!(s["alive"], json!(true));
        assert_eq!(s["emotions"]["fear"], json!(0.0));
        assert_eq!(s["dominant"], Value::Null);
    }

    #[test]
    fn sigmoid_known_values() {
        assert!((sigmoid(0.0) - 0.5).abs() < 1e-12);
        assert!(sigmoid(10.0) > 0.9999);
        // 未达阈值 → 增益 1.0
        assert_eq!(sigmoid_gain(10.0, 50.0), 1.0);
    }

    #[test]
    fn step_basic_delta_applies() {
        // ⚠ 对齐 Python 实测：惯性上限 ±10 意味着单章 fear 最多 +10。
        // python3 -c "...step(1,[{'i':1.0,'emotion_delta':{'fear':40}}],st)" → fear=10.0
        let st = initial_state("CHAR-99-001", "陈寂");
        let evs = json!([{"event_id":"E1","i":1.0,"emotion_delta":{"fear":40.0}}]);
        let ns = step(1, &evs, &st, None).unwrap();
        assert_eq!(ns["emotions"]["fear"], json!(10.0), "惯性上限 ±10");
        assert_eq!(ns["chapter"], json!(1));
        assert_eq!(ns["dominant"], json!("fear"));
        assert_eq!(ns["strength"], json!(10.0));
    }

    #[test]
    fn step_inertia_caps_at_10() {
        // 惯性上限 ±10：old=0，delta 巨大 → 实际步进 ≤10
        let st = initial_state("C", "");
        let evs = json!([{"event_id":"E","i":1.0,"emotion_delta":{"fear":200.0}}]);
        let ns = step(1, &evs, &st, None).unwrap();
        // em_new=200 → clamp 100 → 惯性 min(100-0,10)=10 → fear=10
        assert_eq!(ns["emotions"]["fear"], json!(10.0));
    }

    #[test]
    fn coupling_fear_over_70_dampens_anger() {
        let mut st = initial_state("C", "");
        st["emotions"]["anger"] = json!(50.0);
        st["emotions"]["fear"] = json!(80.0);
        // 无事件，靠耦合规则触发（fear>70 → anger*0.4）
        let ns = step(2, &json!([]), &st, None).unwrap();
        // 惯性后 anger=50, fear=80；耦合 → anger=20
        assert_eq!(ns["emotions"]["anger"], json!(20.0));
    }

    #[test]
    fn decay_memory_drops_below_5() {
        let m = new_memory("C", "E", 0, "fear", 60.0);
        let (e_t, keep) = decay_memory(&m, 0, 0.3);
        assert!(keep);
        assert!((e_t - 60.0).abs() < 1e-9);
        // 衰减到 <5
        let (e_t2, keep2) = decay_memory(&m, 30, 0.3);
        assert!(!keep2, "e_t={e_t2}");
    }

    #[test]
    fn branch_probs_none_seed_is_none() {
        let st = initial_state("C", "");
        assert!(branch_probs(&st, None, SIGMOID_THRESHOLD).is_none());
    }

    #[test]
    fn ooc_check_predicate3_is_k5_flavor() {
        // k5 口径：sign(delta)*cur < -30
        // delta=+40, cur=-... 不可能；用 cur 正、delta 负：sign=-1, cur=50 → -50 < -30 → 触发
        let mut st = initial_state("C", "");
        st["emotions"]["joy"] = json!(50.0);
        let evs = json!({"E":{"emotion_delta":{"joy":-40.0}}});
        let w = ooc_check("动作", &st, Some(&evs));
        assert!(w.iter().any(|x| x["code"] == "EMOTION_EVENT_CONFLICT"), "{w:?}");
        // 对照 ooc_gate 口径（|delta|>20 且 cur<10）此处 cur=50 不触发
        let w2 = crate::ooc_gate::ooc_gate("动作", &st, Some(&evs), 20.0, None, None);
        assert!(!w2.iter().any(|f| f.code == "EMOTION_EVENT_CONFLICT"));
    }

    #[test]
    fn replay_deterministic_with_seed() {
        let mut st = initial_state("C", "");
        st["dominant"] = json!("fear");
        st["strength"] = json!(90.0);
        st["emotions"]["fear"] = json!(90.0);
        let a = branch_probs(&st, Some(7), SIGMOID_THRESHOLD);
        let b = branch_probs(&st, Some(7), SIGMOID_THRESHOLD);
        assert_eq!(serde_json::to_string(&a).unwrap(), serde_json::to_string(&b).unwrap());
    }
}
