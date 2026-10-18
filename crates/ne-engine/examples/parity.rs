//! 端到端对拍：把 Rust 输出打成 JSON，交给 Python 侧逐值比对。
use ne_engine::{dynamics, emotion, ooc_gate};
use serde_json::json;

fn main() {
    let mut out = serde_json::Map::new();

    // ── ① dynamics 8 公式（3 组 payload）
    let payloads = json!([
        {"atk_num":10,"def_num":8,"atk":"太玄","def":"星耀"},
        {"foreshadow_id":"F-001","evidence":0.6,"deadline":5},
        {"realm":4,"gene":3,"mod":0,"offense":0.7,"defense":0.5},
    ]);
    let mut dyn_out = Vec::new();
    for p in payloads.as_array().unwrap() {
        dyn_out.push(dynamics::run_all(p));
    }
    out.insert("dynamics".into(), json!(dyn_out));

    // ── ② emotion.step（对齐 Python 的演示序列）
    let st = emotion::initial_state("CHAR-99-001", "陈寂");
    let evs = json!([{"event_id":"E1","i":0.8,"emotion_delta":{"fear":60.0,"joy":-20.0},"memory_dim":"fear","memory_strength":70}]);
    let s1 = emotion::step(1, &evs, &st, None).unwrap();
    let s2 = emotion::step(2, &json!([]), &s1, None).unwrap();
    let s3 = emotion::step(3, &json!([]), &s2, None).unwrap();
    out.insert("emotion_step1".into(), s1);
    out.insert("emotion_step2".into(), s2);
    out.insert("emotion_step3".into(), s3);

    // ── ③ ooc_gate（5 判据样例）
    // 公平对拍：读与 Python 完全相同的 kb 文件（环境变量 NOVEL_KB 指定）
    let kb = std::env::var("NOVEL_KB").ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({"drift_blacklist":[],"canon_invariants":[],"whitelist":[]}));
    let base = json!({
        "char_code":"CHAR-99-001","alive":true,"realm":"共鸣境","_realm_max":"共鸣境",
        "suppress":0.6,"emotions":{"fear":5.0,"joy":50.0}
    });
    let evt = json!({"EVT-Y":{"emotion_delta":{"fear":30.0}}});
    let g = ooc_gate::ooc_gate("平惧骤惊", &base, Some(&evt), 20.0, Some("霍去病在归墟海修炼"), Some(&kb));
    let gj: Vec<_> = g.iter().map(|f| f.to_json()).collect();
    out.insert("ooc_gate".into(), json!(gj));

    // ── ④ deterministic_check（另一样例文本）
    let mut dtext = String::from("不禁他走进屋子，看着桌上的灯。那灯很旧。他没有说话。窗外风很大。");
    for _ in 0..120 { dtext.push_str("他坐下，拿起一本书，翻开第一页。"); }
    let dd = ne_engine::chapter::deterministic_check(&dtext, 1);
    out.insert("det_check".into(), serde_json::to_value(&dd).unwrap());

    println!("{}", serde_json::to_string_pretty(&serde_json::Value::Object(out)).unwrap());
}
