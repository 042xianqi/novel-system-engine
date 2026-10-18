#!/bin/bash
# 全系统自动审计+实测套件
set -u
R="${NOVEL_LIB_ROOT:-/sdcard/Download/小说相关备份/projects/projects/系统/资料库}"
NROOT="/root/novel-system"
T="$NROOT/tools/audit"
mkdir -p "$T/reports"

echo "[P0] 静态探查..."
python3 "$T/p0_inventory.py" > "$T/reports/p0.txt" 2>&1

cat > "$T/p1_probe.rs" <<'RSEOF'
use serde_json::json;
fn main() {
 let (mut ok,mut bad)=(0,0);
 macro_rules! t { ($n:expr,$b:expr) => {{ match $b { Ok(v)=>{ok+=1; println!("OK\t{}\t{}",$n,v);}, Err(e)=>{bad+=1; println!("FAIL\t{}\t{}",$n,e);} } }}; }
 t!("emotion.initial_state", Ok::<serde_json::Value,anyhow::Error>(ne_engine::emotion::initial_state("C","n")));
 let st=ne_engine::emotion::initial_state("C","n");
 let ev=json!([{"event_id":"E","affected_roles":["C"],"emotion_delta":{"fear":30},"i":0.8}]);
 t!("emotion.step", ne_engine::emotion::step(1,&ev,&st,None));
 for m in ["lst.square","bayes.mood","mkc.transition","tlm.tension","sna.influence","dmg.10dim","sna.causal_density","bayes.sna_bridge"] {
  let p=json!({"atk_num":10,"def_num":8,"deadline":5,"chapter":1,"evidence":0.6,"node":"A","adjacency":{"A":{"B":1.0}},"realm":4,"card_steps":{"x":2},"entity_count":3,"forces":["A","B"],"current":[0.5,0.5],"transition":[[0.5,0.5],[0.5,0.5]]});
  t!(format!("dynamics.{m}"), ne_engine::dynamics::run_model(m,&p));
 }
 for m in ["civ.tide","ent.entropy","res.decay"] {
  let p=json!({"chapter":90,"phase":78,"stance":"守成","entropy_debt":12,"power_cost":4,"intensity":0.6,"age_ch":12});
  t!(format!("world_physics.{m}"), ne_engine::world_physics::run_model(m,&p));
 }
 println!("SUMMARY {ok} OK / {bad} FAIL");
}
RSEOF
cp "$T/p1_probe.rs" "$NROOT/crates/ne-engine/examples/p1_probe.rs"
echo "[P1] 编译探针..."
(cd "$NROOT" && cargo build --release -p ne-engine --example p1_probe 2>&1 | grep -E '^error' | head -5)
echo "[P1] 跑模型探针..."
"$NROOT/target/release/examples/p1_probe" > "$T/reports/p1.txt" 2>&1
cat "$T/reports/p1.txt"
echo "[P1] 完成，结果在 $T/reports/p1.txt"
