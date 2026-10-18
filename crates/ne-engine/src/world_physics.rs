//! 世界物理引擎 —— `world_physics.py` 的 Rust 忠实移植（3 公式，0 LLM / 0 随机）。
//!
//! 事实源：`05_管线框架/engine/world_physics.py`（250 行）
//!
//! 补齐 dynamics 8 个「章级」公式缺的**世界量**：
//!   ① civ.tide     文明兴衰潮汐相位
//!   ② ent.entropy  熵耗散代价守恒（无白嫖铁律数值化）
//!   ③ res.decay    残响衰减（记忆物理）
//!
//! 输出键名/精度/note 文案**逐字照抄**；Python 的 `ValueError` → Rust `anyhow::Err`。

use anyhow::{anyhow, Result};
use serde_json::{json, Map, Value};

fn clamp01(v: f64) -> f64 {
    v.max(0.0).min(1.0)
}
fn clamp100(v: f64) -> f64 {
    v.max(0.0).min(100.0)
}
fn fmt(v: f64, prec: usize) -> String {
    format!("{v:.prec$}")
}
fn fval(p: &Value, k: &str, d: f64) -> f64 {
    p.get(k).and_then(|v| v.as_f64()).unwrap_or(d)
}
fn ival(p: &Value, k: &str, d: i64) -> i64 {
    p.get(k).and_then(|v| v.as_i64()).unwrap_or(d)
}
fn sb(v: bool) -> String {
    if v { "true".into() } else { "false".into() }
}

/// stance → 每章相位速率（照抄 STANCE_RATE）。
pub fn stance_rate(stance: &str) -> f64 {
    match stance {
        "守成" => -0.05,
        "扩张" => 0.08,
        "衰退" => -0.12,
        "中立" => 0.0,
        _ => 0.0,
    }
}

pub const TIDE_CRITICAL_LOW: f64 = 15.0;
pub const TIDE_CRITICAL_HIGH: f64 = 95.0;
pub const ENTROPY_WARN: f64 = 40.0;
pub const ENTROPY_CRITICAL: f64 = 80.0;
pub const RES_DECAY_DEFAULT: f64 = 0.92;
pub const RES_FORGET_THRESHOLD: f64 = 0.1;

/// ① civ.tide —— 文明兴衰潮汐相位。
pub fn civ_tide(payload: &Value) -> Result<Value> {
    let mut phase = clamp100(fval(payload, "phase", 50.0));
    let stance = payload
        .get("stance")
        .and_then(|v| v.as_str())
        .unwrap_or("守成")
        .to_string();
    let rate = stance_rate(&stance);
    let chapter = ival(payload, "chapter", 0);
    let last = ival(payload, "last_chapter", 0);
    let mut steps = (chapter - last).max(0);

    // 跨卷回卷锚点：防止跨卷单调漂移
    if payload.get("reanchor").and_then(|v| v.as_bool()).unwrap_or(false) {
        let off = match stance.as_str() {
            "守成" => 10.0,
            "扩张" => 20.0,
            "衰退" => -10.0,
            _ => 0.0,
        };
        phase = 50.0 + off;
        steps = 0;
    }
    let new_phase = clamp100(phase + rate * steps as f64);
    let critical = new_phase < TIDE_CRITICAL_LOW || new_phase > TIDE_CRITICAL_HIGH;

    Ok(json!({
        "civ.tide.phase": fmt(new_phase, 2),
        "civ.tide.delta": fmt(new_phase - phase, 3),
        "civ.tide.steps": steps.to_string(),
        "civ.tide.stance": stance,
        "civ.tide.rate": fmt(rate, 3),
        "civ.tide.critical": sb(critical),
        "civ.tide.note": if critical { "相位临界·需卷级事件推演" } else { "相位正常" },
    }))
}

/// ② ent.entropy —— 熵耗散代价守恒。
pub fn ent_entropy(payload: &Value) -> Result<Value> {
    let debt = fval(payload, "entropy_debt", 0.0);
    let power_used = ival(payload, "power_used", 0);
    let power_cost = fval(payload, "power_cost", 0.0);
    if power_cost < 0.0 {
        return Err(anyhow!("ent.entropy: power_cost must be >=0 (got {power_cost})"));
    }
    let warn = fval(payload, "debt_warn", ENTROPY_WARN);
    let critical = fval(payload, "debt_critical", ENTROPY_CRITICAL);
    let recover = fval(payload, "recover_rate", 0.5);

    let (new_debt, state) = if power_used > 0 {
        let nd = debt + power_cost * power_used as f64;
        let st = if nd >= critical {
            "critical"
        } else if nd >= warn {
            "warn"
        } else {
            "ok"
        };
        (nd, st)
    } else {
        ((debt - recover).max(0.0), "ok")
    };

    Ok(json!({
        "ent.entropy.debt": fmt(new_debt, 2),
        "ent.entropy.delta": fmt(new_debt - debt, 3),
        "ent.entropy.state": state,
        "ent.entropy.action": match state {
            "critical" => "触发代价反噬·须写可观测代价",
            "warn" => "触发代价可观测·下章须呈现代价",
            _ => "无代价触发",
        },
    }))
}

/// ③ res.decay —— 残响衰减。
pub fn res_decay(payload: &Value) -> Result<Value> {
    let intensity = clamp01(fval(payload, "intensity", 1.0));
    let age = ival(payload, "age_ch", 0).max(0);
    let rate = fval(payload, "decay_rate", RES_DECAY_DEFAULT);
    if !(rate > 0.0 && rate < 1.0) {
        return Err(anyhow!("res.decay: decay_rate must be in (0,1) (got {rate})"));
    }
    let fth = fval(payload, "forget_threshold", RES_FORGET_THRESHOLD);
    let new_intensity = clamp01(intensity * rate.powi(age as i32));
    let forgotten = new_intensity < fth;

    Ok(json!({
        "res.decay.intensity": fmt(new_intensity, 4),
        "res.decay.delta": fmt(new_intensity - intensity, 4),
        "res.decay.forgotten": sb(forgotten),
        "res.decay.half_life_ch": fmt((0.5f64.ln()) / rate.ln(), 2),
        "res.decay.note": if forgotten {
            "强度跌破阈值·进入被遗忘（遗忘区规则：存在锚定失效）"
        } else {
            "残响可维持"
        },
    }))
}

/// 统一入口（照抄 MODELS / run_model / run_all）。
pub const MODEL_IDS: [&str; 3] = ["civ.tide", "ent.entropy", "res.decay"];

pub fn run_model(model_id: &str, payload: &Value) -> Result<Value> {
    match model_id {
        "civ.tide" => civ_tide(payload),
        "ent.entropy" => ent_entropy(payload),
        "res.decay" => res_decay(payload),
        other => Err(anyhow!(
            "world_physics: unknown model {other:?}; valid={MODEL_IDS:?}"
        )),
    }
}

/// 跑全部 3 模型。**注意**：Python 版 `run_all` 不吞异常（一个抛即整体抛），
/// 这里保持同语义——任一 Err 即返回 Err。
pub fn run_all(payload: &Value) -> Result<Value> {
    let mut out = Map::new();
    for mid in MODEL_IDS {
        out.insert(mid.to_string(), run_model(mid, payload)?);
    }
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 逐条复刻 world_physics.selftest() 的 12 项断言。
    #[test]
    fn selftest_civ_tide() {
        let r = civ_tide(&json!({"chapter":30,"last_chapter":0,"phase":60.0,"stance":"扩张"})).unwrap();
        assert!(r["civ.tide.phase"].as_str().unwrap().parse::<f64>().unwrap() > 60.0, "扩张应升");

        let r = civ_tide(&json!({"chapter":30,"last_chapter":0,"phase":60.0,"stance":"衰退"})).unwrap();
        assert!(r["civ.tide.phase"].as_str().unwrap().parse::<f64>().unwrap() < 60.0, "衰退应降");

        let r = civ_tide(&json!({"chapter":30,"last_chapter":0,"phase":99.0,"stance":"扩张","reanchor":true})).unwrap();
        assert!((r["civ.tide.phase"].as_str().unwrap().parse::<f64>().unwrap() - 70.0).abs() < 1e-9, "回卷重校到 70");

        let r = civ_tide(&json!({"chapter":200,"last_chapter":180,"phase":10.0,"stance":"衰退"})).unwrap();
        assert_eq!(r["civ.tide.critical"], "true", "跌破 15 触发临界");
    }

    #[test]
    fn selftest_ent_entropy() {
        let r = ent_entropy(&json!({"entropy_debt":10.0,"power_used":5,"power_cost":4.0})).unwrap();
        assert!((r["ent.entropy.debt"].as_str().unwrap().parse::<f64>().unwrap() - 30.0).abs() < 1e-9);

        let r = ent_entropy(&json!({"entropy_debt":45.0,"power_used":5,"power_cost":4.0})).unwrap();
        assert_eq!(r["ent.entropy.state"], "warn");

        let r = ent_entropy(&json!({"entropy_debt":85.0,"power_used":5,"power_cost":4.0})).unwrap();
        assert_eq!(r["ent.entropy.state"], "critical");

        let r = ent_entropy(&json!({"entropy_debt":44.0,"power_used":0})).unwrap();
        assert!((r["ent.entropy.debt"].as_str().unwrap().parse::<f64>().unwrap() - 43.5).abs() < 1e-9);
    }

    #[test]
    fn selftest_res_decay() {
        let r = res_decay(&json!({"intensity":0.8,"age_ch":10,"decay_rate":0.92})).unwrap();
        let expect = clamp01(0.8 * 0.92f64.powi(10));
        let got: f64 = r["res.decay.intensity"].as_str().unwrap().parse().unwrap();
        assert!((got - expect).abs() < 1e-4, "指数衰减值");

        let r = res_decay(&json!({"intensity":0.3,"age_ch":30,"decay_rate":0.92})).unwrap();
        assert_eq!(r["res.decay.forgotten"], "true", "跌破 0.1 进入被遗忘");

        let r = res_decay(&json!({"intensity":1.0,"age_ch":0})).unwrap();
        assert!(r["res.decay.half_life_ch"].as_str().unwrap().parse::<f64>().unwrap() > 8.0);
    }

    #[test]
    fn selftest_replay_deterministic() {
        let p = json!({"chapter":30,"last_chapter":0,"phase":60.0,"stance":"扩张",
                       "entropy_debt":10.0,"power_used":5,"power_cost":4.0,
                       "intensity":0.8,"age_ch":10});
        let a = run_all(&p).unwrap();
        let b = run_all(&p).unwrap();
        assert_eq!(a, b, "同参数两次跑 diff 空");
    }

    #[test]
    fn rejects_bad_params() {
        assert!(ent_entropy(&json!({"power_cost":-1.0})).is_err());
        assert!(res_decay(&json!({"decay_rate":1.5})).is_err());
        assert!(res_decay(&json!({"decay_rate":0.0})).is_err());
        assert!(run_model("nope", &json!({})).is_err());
    }
}
