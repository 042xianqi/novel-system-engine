//! 动力学演算模型 —— `dynamics.py` 的 Rust 忠实移植（8 公式，纯数学，0 LLM）。
//!
//! 事实源：`05_管线框架/engine/dynamics.py`（354 行）
//! 移植原则：公式、clamp 范围、默认值、输出键名**逐字照抄**。
//! 唯一差异：Python 抛 `ValueError` 的地方，Rust 返回 `anyhow::Err`（语义一致）。
//!
//! 输出统一为 `serde_json::Value`（object），浮点用 4 位小数字符串（对齐 `_fmt(v,4)`）。

use anyhow::{anyhow, Result};
use serde_json::{json, Map, Value};

/// 跨文明胜率矩阵（ART-99-008 标定，照抄）。
fn lanchester_matrix(atk: &str) -> Option<&'static [(&'static str, f64)]> {
    match atk {
        "太玄" => Some(&[("星耀", 0.48), ("天启", 0.45), ("异化", 0.58)]),
        "星耀" => Some(&[("太玄", 0.52), ("天启", 0.61), ("异化", 0.54)]),
        "天启" => Some(&[("太玄", 0.55), ("星耀", 0.39), ("异化", 0.47)]),
        "异化" => Some(&[("太玄", 0.42), ("星耀", 0.46), ("天启", 0.53)]),
        _ => None,
    }
}

/// 10 维战力权重（照抄 DMG_WEIGHTS）。
const DMG_WEIGHTS: [f64; 10] = [0.15, 0.15, 0.10, 0.15, 0.15, 0.08, 0.07, 0.05, 0.04, 0.06];
const DMG_KEYS: [&str; 10] = [
    "realm", "gene", "mod", "offense", "defense", "shield", "armor", "res", "synergy", "luck",
];

fn clamp01(v: f64) -> f64 {
    v.clamp(0.0, 1.0)
}

/// 对齐 Python `_fmt(v, prec=4)` → 4 位小数字符串。
fn fmt4(v: f64) -> String {
    format!("{v:.4}")
}

fn fval(p: &Value, k: &str) -> Option<f64> {
    p.get(k).and_then(|x| x.as_f64())
}

fn ival(p: &Value, k: &str) -> Option<i64> {
    p.get(k).and_then(|x| x.as_i64())
}

fn sval(p: &Value, k: &str) -> String {
    p.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

// ───────────────────────────── 1. lst.square
pub fn lst_square(payload: &Value) -> Result<Value> {
    let atk_num = ival(payload, "atk_num").unwrap_or(0);
    let def_num = ival(payload, "def_num").unwrap_or(0);
    if atk_num <= 0 || def_num <= 0 {
        return Err(anyhow!("lst.square: atk_num/def_num must be > 0 (got {atk_num}/{def_num})"));
    }
    let atk = sval(payload, "atk");
    let def_ = sval(payload, "def");
    let (mut alpha, mut beta) = (1.0_f64, 1.0_f64);
    if !atk.is_empty() && !def_.is_empty() && atk != def_ {
        if let Some(m) = lanchester_matrix(&atk) {
            if let Some((_, wr)) = m.iter().find(|(n, _)| *n == def_) {
                if *wr > 0.0 && *wr < 1.0 {
                    alpha = wr / (1.0 - wr);
                    beta = 1.0;
                }
            }
        }
    }
    // 【决策二·E级修复】原先"先 unwrap_or(0.0) 判空、再 unwrap() 取值"是两次独立
    // 调用：当前实现下第二次必定 Some（安全），但一旦 fval 改为非确定性实现就 panic。
    // 改为一次取值 + if let，panic 点彻底消失，语义等价。
    if let Some(a) = fval(payload, "atk_alpha") {
        if a > 0.0 {
            alpha = a;
        }
    }
    if let Some(b) = fval(payload, "def_beta") {
        if b > 0.0 {
            beta = b;
        }
    }

    let ratio = alpha / beta;
    let a = atk_num as f64;
    let d = def_num as f64;
    let diff = a * a - ratio * d * d;

    let (atk_surv, def_surv, draw) = if diff > 0.0 {
        ((diff / ratio).sqrt(), 0.0, false)
    } else if diff < 0.0 {
        (0.0, (-diff).sqrt(), false)
    } else {
        (0.0, 0.0, true)
    };
    let win = if draw {
        0.5
    } else if atk_surv > 0.0 {
        1.0
    } else if def_surv > 0.0 {
        0.0
    } else {
        0.5
    };

    Ok(json!({
        "lst.atk_survivors": fmt4(atk_surv),
        "lst.def_survivors": fmt4(def_surv),
        "lst.atk_win_rate": fmt4(win),
        "lst.draw": if draw {"true"} else {"false"},
        "lst.alpha": fmt4(alpha),
        "lst.beta": fmt4(beta),
    }))
}

// ───────────────────────────── 2. bayes.mood
pub fn bayes_mood(payload: &Value) -> Result<Value> {
    let mut prior = fval(payload, "prior").unwrap_or(0.5);
    if !(prior > 0.0 && prior < 1.0) {
        prior = 0.5;
    }
    let evidence = fval(payload, "evidence").unwrap_or(0.0);
    if !(0.0..=1.0).contains(&evidence) {
        return Err(anyhow!("bayes.mood: evidence must be in [0,1] (got {evidence})"));
    }
    let mut lik_ratio = fval(payload, "likelihood_ratio").unwrap_or(2.0);
    if lik_ratio <= 0.0 {
        lik_ratio = 2.0;
    }
    let deadline = ival(payload, "deadline").unwrap_or(0);

    let eff_lr = 1.0 + evidence * (lik_ratio - 1.0);
    let posterior = (prior * eff_lr) / (prior * eff_lr + (1.0 - prior));
    let warn_hollow = deadline > 0 && posterior < 0.8;

    Ok(json!({
        "bayes.prior": fmt4(prior),
        "bayes.evidence": fmt4(evidence),
        "bayes.lik_ratio": fmt4(eff_lr),
        "bayes.posterior": fmt4(posterior),
        "bayes.id": sval(payload, "foreshadow_id"),
        "bayes.deadline": deadline.to_string(),
        "bayes.warn_hollow": if warn_hollow {"true"} else {"false"},
    }))
}

// ───────────────────────────── 3. mkc.transition
pub fn mkc_transition(payload: &Value) -> Result<Value> {
    let forces: Vec<String> = payload
        .get("forces")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().map(|x| x.as_str().unwrap_or("").to_string()).collect())
        .unwrap_or_default();
    let current: Vec<f64> = payload
        .get("current")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().map(|x| x.as_f64().unwrap_or(0.0)).collect())
        .unwrap_or_default();
    let n = forces.len();
    if n == 0 || current.len() != n {
        return Err(anyhow!(
            "mkc.transition: forces({n}) != current({})",
            current.len()
        ));
    }

    // transition 矩阵：缺省则 0.8 对角 + 0.2/(n-1) 他项
    let trans: Vec<Vec<f64>> = match payload.get("transition").and_then(|v| v.as_array()) {
        Some(rows) if !rows.is_empty() => rows
            .iter()
            .map(|r| {
                r.as_array()
                    .map(|rr| rr.iter().map(|x| x.as_f64().unwrap_or(0.0)).collect())
                    .unwrap_or_default()
            })
            .collect(),
        _ => {
            let other_share = 0.2 / (n.saturating_sub(1)).max(1) as f64;
            (0..n)
                .map(|i| {
                    (0..n)
                        .map(|j| if i == j { 0.8 } else { other_share })
                        .collect()
                })
                .collect()
        }
    };

    let mut next = vec![0.0_f64; n];
    for i in 0..n {
        for j in 0..n {
            let tv = trans.get(i).and_then(|r| r.get(j)).copied().unwrap_or(0.0);
            next[j] += current[i] * tv;
        }
    }

    let mut out = Map::new();
    for (k, name) in forces.iter().enumerate() {
        out.insert(format!("mkc.{name}"), json!(fmt4(next[k])));
    }
    out.insert("mkc.total".into(), json!(fmt4(next.iter().sum())));
    Ok(Value::Object(out))
}

// ───────────────────────────── 4. tlm.tension
pub fn tlm_tension(payload: &Value) -> Result<Value> {
    let deadline = ival(payload, "deadline").unwrap_or(0);
    let chapter = ival(payload, "chapter").unwrap_or(0);
    if deadline <= 0 {
        return Err(anyhow!("tlm.tension: deadline must be > 0"));
    }
    let mut decay = fval(payload, "decay").unwrap_or(0.9);
    if decay <= 0.0 || decay >= 1.0 {
        decay = 0.9;
    }
    let remaining = (deadline - chapter).max(0);
    let elapsed = (10 - remaining).max(0);
    let tension = decay.powi(elapsed as i32);
    let dead_tension = decay.powi(10);
    let warn = dead_tension < 0.5;

    Ok(json!({
        "tlm.tension": fmt4(tension),
        "tlm.remaining": remaining.to_string(),
        "tlm.deadline": deadline.to_string(),
        "tlm.decay": fmt4(decay),
        "tlm.warn_hollow": if warn {"true"} else {"false"},
        "tlm.id": sval(payload, "foreshadow_id"),
    }))
}

// ───────────────────────────── 5. sna.influence
pub fn sna_influence(payload: &Value) -> Result<Value> {
    let node = sval(payload, "node");
    if node.is_empty() {
        return Err(anyhow!("sna.influence: node required"));
    }
    let adj = payload.get("adjacency").cloned().unwrap_or_else(|| json!({}));
    let mut w = fval(payload, "edge_weight").unwrap_or(1.0);
    if w <= 0.0 {
        w = 1.0;
    }
    let cf = fval(payload, "causal_factor").unwrap_or(0.0);
    if cf > 0.0 {
        w *= cf;
    }
    let degree = adj.get(&node).and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
    let influence = degree as f64 * w;
    let max_deg = adj
        .as_object()
        .map(|m| m.values().filter_map(|v| v.as_array()).map(|a| a.len()).max().unwrap_or(0))
        .unwrap_or(0);
    let normalized = if max_deg > 0 { influence / (max_deg as f64 * w) } else { 0.0 };

    Ok(json!({
        "sna.degree": degree.to_string(),
        "sna.influence": fmt4(influence),
        "sna.normalized": fmt4(normalized),
        "sna.max_degree": max_deg.to_string(),
        "sna.node": node,
        "sna.causal_factor": fmt4(cf),
    }))
}

// ───────────────────────────── 6. dmg.10dim
pub fn dmg_10dim(payload: &Value) -> Result<Value> {
    let raw = [
        fval(payload, "realm").unwrap_or(0.0) / 9.0,
        fval(payload, "gene").unwrap_or(0.0) / 9.0,
        fval(payload, "mod").unwrap_or(0.0) / 7.0,
        fval(payload, "offense").unwrap_or(0.0),
        fval(payload, "defense").unwrap_or(0.0),
        fval(payload, "shield").unwrap_or(0.0),
        fval(payload, "armor").unwrap_or(0.0),
        fval(payload, "res").unwrap_or(0.0),
        fval(payload, "synergy").unwrap_or(0.0),
        fval(payload, "luck").unwrap_or(0.0),
    ];
    let vals: Vec<f64> = raw.iter().map(|v| clamp01(*v)).collect();
    let score: f64 = (0..10).map(|i| DMG_WEIGHTS[i] * vals[i]).sum::<f64>() * 100.0;
    let grade = if score >= 98.0 {
        "S"
    } else if score >= 95.0 {
        "A"
    } else if score >= 80.0 {
        "B"
    } else {
        "C"
    };

    let mut out = Map::new();
    out.insert("dmg.score".into(), json!(fmt4(score)));
    out.insert("dmg.grade".into(), json!(grade));
    for (i, key) in DMG_KEYS.iter().enumerate() {
        out.insert(format!("dmg.{key}"), json!(fmt4(vals[i])));
    }
    Ok(Value::Object(out))
}

// ───────────────────────────── 7. sna.causal_density
pub fn sna_causal_density(payload: &Value) -> Result<Value> {
    let card_steps = payload.get("card_steps").cloned().unwrap_or_else(|| json!({}));
    let obj = card_steps
        .as_object()
        .cloned()
        .unwrap_or_default();
    if obj.is_empty() {
        return Err(anyhow!("sna.causal_density: card_steps required"));
    }
    let total_steps: i64 = obj
        .values()
        .map(|v| v.as_i64().unwrap_or(0).max(0))
        .sum();
    let entities = ival(payload, "entity_count").unwrap_or(0).max(1);
    let density = total_steps as f64 / entities as f64;
    let factor = 1.0 + density;
    Ok(json!({
        "sna.cd.density": fmt4(density),
        "sna.cd.factor": fmt4(factor),
        "sna.cd.total_steps": total_steps.to_string(),
        "sna.cd.entities": entities.to_string(),
        "sna.cd.cards": obj.len().to_string(),
        "sna.cd.domain": sval(payload, "domain"),
    }))
}

// ───────────────────────────── 8. bayes.sna_bridge
pub fn bayes_sna_bridge(payload: &Value) -> Result<Value> {
    let node = sval(payload, "node");
    if node.is_empty() {
        return Err(anyhow!("bayes.sna_bridge: node required"));
    }
    let adj = payload.get("adjacency").cloned().unwrap_or_else(|| json!({}));
    let evidence = fval(payload, "evidence").unwrap_or(0.0);
    if !(0.0..=1.0).contains(&evidence) {
        return Err(anyhow!("bayes.sna_bridge: evidence must be in [0,1]"));
    }
    let mut w = fval(payload, "edge_weight").unwrap_or(1.0);
    if w <= 0.0 {
        w = 1.0;
    }
    let _ = w;
    let degree = adj.get(&node).and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
    let max_deg = adj
        .as_object()
        .map(|m| m.values().filter_map(|v| v.as_array()).map(|a| a.len()).max().unwrap_or(0))
        .unwrap_or(0);
    let sna_norm = if max_deg > 0 { degree as f64 / max_deg as f64 } else { 0.0 };

    let mut lik_ratio = fval(payload, "likelihood_ratio").unwrap_or(2.0);
    if lik_ratio <= 0.0 {
        lik_ratio = 2.0;
    }
    let bridged_lr = 1.0 + sna_norm * (lik_ratio - 1.0);

    let mut prior = fval(payload, "prior").unwrap_or(0.5);
    if !(prior > 0.0 && prior < 1.0) {
        prior = 0.5;
    }
    let eff_lr = 1.0 + evidence * (bridged_lr - 1.0);
    let posterior = (prior * eff_lr) / (prior * eff_lr + (1.0 - prior));
    let deadline = ival(payload, "deadline").unwrap_or(0);
    let warn_hollow = deadline > 0 && posterior < 0.8;

    Ok(json!({
        "bayes_sna.node": node,
        "bayes_sna.sna_norm": fmt4(sna_norm),
        "bayes_sna.sna_degree": degree.to_string(),
        "bayes_sna.max_degree": max_deg.to_string(),
        "bayes_sna.prior": fmt4(prior),
        "bayes_sna.evidence": fmt4(evidence),
        "bayes_sna.bridged_lr": fmt4(bridged_lr),
        "bayes_sna.eff_lr": fmt4(eff_lr),
        "bayes_sna.posterior": fmt4(posterior),
        "bayes_sna.deadline": deadline.to_string(),
        "bayes_sna.warn_hollow": if warn_hollow {"true"} else {"false"},
        "bayes_sna.id": sval(payload, "foreshadow_id"),
    }))
}

// ───────────────────────────── 注册表 + 分发
pub const MODEL_IDS: [&str; 8] = [
    "lst.square",
    "bayes.mood",
    "mkc.transition",
    "tlm.tension",
    "sna.influence",
    "dmg.10dim",
    "sna.causal_density",
    "bayes.sna_bridge",
];

pub fn run_model(model_id: &str, payload: &Value) -> Result<Value> {
    match model_id {
        "lst.square" => lst_square(payload),
        "bayes.mood" => bayes_mood(payload),
        "mkc.transition" => mkc_transition(payload),
        "tlm.tension" => tlm_tension(payload),
        "sna.influence" => sna_influence(payload),
        "dmg.10dim" => dmg_10dim(payload),
        "sna.causal_density" => sna_causal_density(payload),
        "bayes.sna_bridge" => bayes_sna_bridge(payload),
        other => Err(anyhow!(
            "unknown model: {other} (available: {MODEL_IDS:?})"
        )),
    }
}

/// 跑全部 8 模型（能跑则跑，不能跑记 `_error`，与 Python run_all 同口径）。
pub fn run_all(payload: &Value) -> Value {
    let mut out = Map::new();
    for mid in MODEL_IDS {
        let v = match run_model(mid, payload) {
            Ok(v) => v,
            Err(e) => json!({"_error": e.to_string()}),
        };
        out.insert(mid.to_string(), v);
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lst_square_demo_matches_python() {
        // Python: lst_square({"atk_num":10,"def_num":8,"atk":"太玄","def":"星耀"})
        let r = lst_square(&json!({"atk_num":10,"def_num":8,"atk":"太玄","def":"星耀"})).unwrap();
        // wr=0.48 → alpha=0.48/0.52=0.9230769...
        assert_eq!(r["lst.alpha"], json!("0.9231"));
        assert_eq!(r["lst.atk_win_rate"], json!("1.0000"));
        assert_eq!(r["lst.draw"], json!("false"));
    }

    #[test]
    fn bayes_mood_demo() {
        let r = bayes_mood(&json!({"foreshadow_id":"F-001","evidence":0.6,"deadline":5})).unwrap();
        // eff_lr=1+0.6*1=1.6; post=(0.5*1.6)/(0.5*1.6+0.5)=0.8/1.3=0.6153846
        assert_eq!(r["bayes.lik_ratio"], json!("1.6000"));
        assert_eq!(r["bayes.posterior"], json!("0.6154"));
        assert_eq!(r["bayes.warn_hollow"], json!("true"));
    }

    #[test]
    fn dmg_10dim_demo() {
        let r = dmg_10dim(&json!({"realm":4,"gene":3,"mod":0,"offense":0.7,"defense":0.5})).unwrap();
        // realm: 4/9=0.4444*0.15; gene:3/9=0.3333*0.15; mod 0; off 0.7*0.15; def 0.5*0.15
        assert_eq!(r["dmg.grade"], json!("C"));
        assert_eq!(r["dmg.realm"], json!("0.4444"));
    }

    #[test]
    fn run_all_covers_8_and_errors_are_captured() {
        // payload 缺字段 → 部分模型 _error，但 8 键都在
        let r = run_all(&json!({"atk_num":10,"def_num":8}));
        let o = r.as_object().unwrap();
        assert_eq!(o.len(), 8);
        assert!(o["bayes.sna_bridge"]["_error"].is_string());
        assert!(o["lst.square"]["lst.draw"].is_string());
    }

    #[test]
    fn tlm_tension_rejects_bad_deadline() {
        assert!(tlm_tension(&json!({"deadline":0,"chapter":1})).is_err());
    }
}
