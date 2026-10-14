//! 沙盘推演（world-gov，第 15 批）—— 多步**确定性**推演，可重放、可对比。
//!
//! # 定位
//! 把 `ne_engine::world_physics` 的 3 个世界量公式（civ.tide 文明潮汐 / ent.entropy 熵债 /
//! res.decay 残响衰减）串成**多步状态机**：每步的产出回喂下一步，形成「沙盘」。
//! 因公式 0 LLM / 0 随机，同一 scenario 任意次运行结果**逐字节一致** → 可重放校验。
//!
//! # 环节闭合（全链路）
//! 扫描 → 增量 → 标签(10) → 快照(11) → 关系网(14) → **沙盘推演(15)** →（可选 AI 旁白，三重锁，13）
//! - 受影响集来自关系网（第 14 批 `rel_edge`）→ 沙盘知道「这次推演牵动哪些卡」。
//! - 可选 AI 旁白：只在第 13 批三重锁全开时可用；默认**只跑数值、不产散文**。
//!
//! # 存储（复用 novel.db，与主索引隔离）
//! `sandbox_run(id, label, actor, steps, created_ts, scenario_json, final_json, verdict,
//!   critical_steps, fingerprint, engine_epoch)` + `sandbox_step(run_id, step, state_json)`。
//!
//! # 铁则
//! - ❌ 不改资料库正文、不改 `library_index`；只写隔离表。
//! - ✅ 零外呼、零真实 LLM（除非三重锁全开且显式要旁白）；结果可重放。
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use std::path::Path;

use super::{digest, rel_graph};

const SCHEMA: &str = r#"
-- 沙盘推演（2026-10-14，第 15 批）—— 幂等；与主索引隔离。
CREATE TABLE IF NOT EXISTS sandbox_run (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    label          TEXT NOT NULL DEFAULT '',
    actor          TEXT,
    steps          INTEGER NOT NULL DEFAULT 0,
    created_ts     INTEGER NOT NULL,
    scenario_json  TEXT NOT NULL DEFAULT '{}',
    final_json     TEXT NOT NULL DEFAULT '{}',
    verdict        TEXT NOT NULL DEFAULT 'ok',
    critical_steps INTEGER NOT NULL DEFAULT 0,
    fingerprint    TEXT NOT NULL DEFAULT '',
    engine_epoch   INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_sandbox_actor ON sandbox_run(actor);
CREATE TABLE IF NOT EXISTS sandbox_step (
    run_id    INTEGER NOT NULL,
    step      INTEGER NOT NULL,
    state_json TEXT NOT NULL DEFAULT '{}',
    PRIMARY KEY(run_id, step)
);
"#;

/// 推演步数硬上限（防手抖跑飞）。
pub const MAX_STEPS: u32 = 1000;
/// 本轮引擎纪元（与 self_check epoch 对齐；变化即视为推演口径变化）。
pub const ENGINE_EPOCH: i64 = 15;

pub fn ensure_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA)?;
    Ok(())
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// 单步可折叠状态（全部来自 world_physics 的输入口径）。
#[derive(Debug, Clone)]
pub struct SimState {
    pub chapter: i64,
    pub last_chapter: i64,
    pub phase: f64,
    pub stance: String,
    pub entropy_debt: f64,
    pub intensity: f64,
    pub age_ch: i64,
    pub decay_rate: f64,
}

impl Default for SimState {
    fn default() -> Self {
        SimState {
            chapter: 0,
            last_chapter: 0,
            phase: 50.0,
            stance: "守成".into(),
            entropy_debt: 0.0,
            intensity: 1.0,
            age_ch: 0,
            decay_rate: 0.92,
        }
    }
}

impl SimState {
    /// 构造 world_physics 的输入 payload（含本步操作覆盖）。
    pub fn payload(&self, op: &Value) -> Value {
        let mut m = Map::new();
        m.insert("chapter".into(), json!(self.chapter));
        m.insert("last_chapter".into(), json!(self.last_chapter));
        m.insert("phase".into(), json!(self.phase));
        m.insert("stance".into(), json!(self.stance));
        m.insert("entropy_debt".into(), json!(self.entropy_debt));
        m.insert("intensity".into(), json!(self.intensity));
        m.insert("age_ch".into(), json!(self.age_ch));
        m.insert("decay_rate".into(), json!(self.decay_rate));
        // 本步操作覆盖（power_used / power_cost / reanchor / debt_warn …）
        if let Some(o) = op.as_object() {
            for (k, v) in o {
                m.insert(k.clone(), v.clone());
            }
        }
        Value::Object(m)
    }

    /// 用上一步产出推进状态（确定性回喂）。
    fn advance(&mut self, out: &Value, next_chapter: i64) {
        self.last_chapter = self.chapter;
        self.chapter = next_chapter;
        if let Some(p) = out["civ.tide"]["civ.tide.phase"].as_str().and_then(|s| s.parse::<f64>().ok()) {
            self.phase = p;
        }
        if let Some(d) = out["ent.entropy"]["ent.entropy.debt"].as_str().and_then(|s| s.parse::<f64>().ok()) {
            self.entropy_debt = d;
        }
        if let Some(i) = out["res.decay"]["res.decay.intensity"].as_str().and_then(|s| s.parse::<f64>().ok()) {
            self.intensity = i;
        }
        self.age_ch += 1;
    }
}

fn parse_bool_str(v: &Value) -> bool {
    v.as_str().map(|s| s == "true").unwrap_or(false)
}

/// 单步判决（从 world_physics 产出推导；纯函数）。
pub fn step_verdict(out: &Value) -> &'static str {
    let tide_critical = parse_bool_str(&out["civ.tide"]["civ.tide.critical"]);
    let ent_state = out["ent.entropy"]["ent.entropy.state"].as_str().unwrap_or("ok");
    let forgotten = parse_bool_str(&out["res.decay"]["res.decay.forgotten"]);
    if tide_critical || ent_state == "critical" || forgotten {
        "critical"
    } else if ent_state == "warn" {
        "warn"
    } else {
        "ok"
    }
}

/// 严重度排序（用于聚合最坏判决）。
fn rank(v: &str) -> u8 {
    match v {
        "critical" => 3,
        "warn" => 2,
        _ => 1,
    }
}

/// 从 scenario JSON 抽初始状态（缺字段走默认）。
pub fn state_from_scenario(sc: &Value) -> SimState {
    let w = &sc["world"];
    let mut s = SimState::default();
    if let Some(v) = w["chapter"].as_i64() { s.chapter = v; }
    if let Some(v) = w["last_chapter"].as_i64() { s.last_chapter = v; }
    if let Some(v) = w["phase"].as_f64() { s.phase = v; }
    if let Some(v) = w["stance"].as_str() { s.stance = v.to_string(); }
    if let Some(v) = w["entropy_debt"].as_f64() { s.entropy_debt = v; }
    if let Some(v) = w["intensity"].as_f64() { s.intensity = v; }
    if let Some(v) = w["age_ch"].as_i64() { s.age_ch = v; }
    if let Some(v) = w["decay_rate"].as_f64() { s.decay_rate = v; }
    s
}

/// 纯函数：跑一次多步推演（**不落库**）。返回 (final_state_json, steps[], verdict, critical_steps)。
pub fn simulate(scenario: &Value) -> Result<(Value, Vec<Value>, String, usize)> {
    let steps = scenario["steps"].as_u64().unwrap_or(10).min(MAX_STEPS as u64) as u32;
    let ops: Vec<Value> = scenario["ops"].as_array().cloned().unwrap_or_default();
    let mut st = state_from_scenario(scenario);
    let mut out_steps: Vec<Value> = Vec::new();
    let mut worst = "ok".to_string();
    let mut critical_steps = 0usize;

    for i in 0..steps {
        let op = if ops.is_empty() { Value::Null } else { ops[(i as usize) % ops.len()].clone() };
        let payload = st.payload(&op);
        let out = ne_engine::world_physics::run_all(&payload)?;
        let sv = step_verdict(&out);
        if rank(sv) > rank(&worst) { worst = sv.to_string(); }
        if sv == "critical" { critical_steps += 1; }
        out_steps.push(json!({
            "step": i,
            "chapter": st.chapter,
            "op": op,
            "state": {
                "phase": st.phase,
                "entropy_debt": st.entropy_debt,
                "intensity": st.intensity,
                "age_ch": st.age_ch,
            },
            "world_physics": out,
            "verdict": sv,
        }));
        st.advance(&out, st.chapter + 1);
    }

    let final_state = json!({
        "chapter": st.chapter,
        "last_chapter": st.last_chapter,
        "phase": st.phase,
        "stance": st.stance,
        "entropy_debt": st.entropy_debt,
        "intensity": st.intensity,
        "age_ch": st.age_ch,
        "decay_rate": st.decay_rate,
    });
    Ok((final_state, out_steps, worst, critical_steps))
}

/// 确定性指纹：对 (scenario + 最终态 + 各步判决) 取 SHA-1。
pub fn fingerprint(scenario: &Value, final_state: &Value, steps: &[Value]) -> String {
    let mut canon = String::new();
    canon.push_str(&serde_json::to_string(scenario).unwrap_or_default());
    canon.push('\u{1}');
    canon.push_str(&serde_json::to_string(final_state).unwrap_or_default());
    canon.push('\u{1}');
    for s in steps {
        canon.push_str(s["verdict"].as_str().unwrap_or(""));
        canon.push(',');
    }
    digest::sha1_hex(canon.as_bytes())
}

/// 受影响卡集（来自关系网：actor 的下游 + 上游；关系网未建则空，如实标注）。
pub fn affected_cards(conn: &Connection, actor: &str) -> Value {
    let edges = match rel_graph::load_edges(conn) {
        Ok(e) => e,
        Err(_) => return json!({"actor": actor, "cards": [], "note": "关系网未建；跑 world-gov graph build"}),
    };
    let mut set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (s, d) in &edges {
        if s == actor { set.insert(d.clone()); }
        if d == actor { set.insert(s.clone()); }
    }
    json!({
        "actor": actor,
        "cards": set.into_iter().collect::<Vec<_>>(),
        "note": "该角色在引用图上的一跳邻居（上/下游）。",
    })
}

/// 快捷预设场景（避免手写大段 JSON；全部为确定性参数）。
pub fn quick_scenario(name: &str) -> Result<Value> {
    let s = match name {
        "warn" => json!({
            "steps": 8,
            "world": {"chapter": 1, "last_chapter": 0, "phase": 55.0, "stance": "守成",
                       "entropy_debt": 20.0, "intensity": 0.8, "age_ch": 0, "decay_rate": 0.92},
            "ops": [{"power_used": 3, "power_cost": 5.0}]
        }),
        "critical" => json!({
            "steps": 40,
            "world": {"chapter": 1, "last_chapter": 0, "phase": 10.0, "stance": "衰退",
                       "entropy_debt": 0.0, "intensity": 0.5, "age_ch": 0, "decay_rate": 0.92},
            "ops": [{"power_used": 5, "power_cost": 4.0}]
        }),
        "tip" => json!({
            "steps": 20,
            "world": {"chapter": 150, "last_chapter": 140, "phase": 60.0, "stance": "扩张",
                       "entropy_debt": 10.0, "intensity": 1.0, "age_ch": 0, "decay_rate": 0.95},
            "ops": [{"power_used": 2, "power_cost": 3.0}]
        }),
        "calm" => json!({
            "steps": 10,
            "world": {"chapter": 5, "last_chapter": 0, "phase": 50.0, "stance": "守成",
                       "entropy_debt": 0.0, "intensity": 1.0, "age_ch": 0, "decay_rate": 0.92},
            "ops": []
        }),
        other => anyhow::bail!("未知 quick 预设 '{other}'（合法：warn | critical | tip | calm）"),
    };
    Ok(s)
}

/// 跑沙盘并落库。`save=false` 时只算不写。
pub fn run(
    conn: &Connection,
    lib_root: &Path,
    label: &str,
    actor: Option<&str>,
    scenario: &Value,
    save: bool,
) -> Result<Value> {
    let _ = lib_root;
    ensure_table(conn)?;
    let (final_state, steps, verdict, critical_steps) = simulate(scenario)?;
    let fp = fingerprint(scenario, &final_state, &steps);
    let affected = actor.map(|a| affected_cards(conn, a));

    if !save {
        return Ok(json!({
            "command": "world-gov sandbox run",
            "saved": false,
            "label": label,
            "actor": actor,
            "steps": steps.len(),
            "verdict": verdict,
            "critical_steps": critical_steps,
            "fingerprint": fp,
            "final_state": final_state,
            "affected": affected,
            "note": "未落库（--dry-run / 未加 --save）。数值可重放。",
        }));
    }

    let ts = now_secs();
    let scenario_json = serde_json::to_string(scenario)?;
    let final_json = serde_json::to_string(&final_state)?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO sandbox_run(label,actor,steps,created_ts,scenario_json,final_json,verdict,critical_steps,fingerprint,engine_epoch)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![label, actor, steps.len() as i64, ts, scenario_json, final_json, verdict, critical_steps as i64, fp, ENGINE_EPOCH],
    )?;
    let run_id = tx.last_insert_rowid();
    {
        let mut ins = tx.prepare("INSERT INTO sandbox_step(run_id,step,state_json) VALUES(?1,?2,?3)")?;
        for s in &steps {
            ins.execute(params![run_id, s["step"].as_i64().unwrap_or(0), serde_json::to_string(s)?])?;
        }
    }
    tx.commit()?;

    Ok(json!({
        "command": "world-gov sandbox run",
        "saved": true,
        "run_id": run_id,
        "label": label,
        "actor": actor,
        "steps": steps.len(),
        "verdict": verdict,
        "critical_steps": critical_steps,
        "fingerprint": fp,
        "final_state": final_state,
        "affected": affected,
        "note": "只写隔离表 sandbox_run/sandbox_step；未改资料库与主索引。",
    }))
}

/// 重放校验：读回 scenario → 重跑 → 比对指纹（确定性证据）。
pub fn verify(conn: &Connection, run_id: i64) -> Result<Value> {
    ensure_table(conn)?;
    let row: Option<(String, String, i64, String)> = conn
        .query_row(
            "SELECT scenario_json,fingerprint,engine_epoch,verdict FROM sandbox_run WHERE id=?1",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((scenario_json, stored_fp, epoch, stored_verdict)) = row else {
        return Ok(json!({"command": "world-gov sandbox verify", "run_id": run_id, "status": "NOT_FOUND"}));
    };
    let scenario: Value = serde_json::from_str(&scenario_json).unwrap_or(json!({}));
    let (final_state, steps, verdict, critical_steps) = simulate(&scenario)?;
    let new_fp = fingerprint(&scenario, &final_state, &steps);
    let same_fp = new_fp == stored_fp;
    let same_verdict = verdict == stored_verdict;
    Ok(json!({
        "command": "world-gov sandbox verify",
        "run_id": run_id,
        "status": if same_fp && same_verdict { "REPLAY_OK" } else { "REPLAY_MISMATCH" },
        "deterministic": same_fp && same_verdict,
        "stored_fingerprint": stored_fp,
        "replay_fingerprint": new_fp,
        "stored_verdict": stored_verdict,
        "replay_verdict": verdict,
        "replay_critical_steps": critical_steps,
        "engine_epoch_stored": epoch,
        "engine_epoch_now": ENGINE_EPOCH,
        "note": "同 scenario 重跑，指纹与判决须逐字节一致（0 LLM/0 随机）。",
    }))
}

/// 列出推演记录。
pub fn list(conn: &Connection, limit: usize) -> Result<Value> {
    ensure_table(conn)?;
    let mut stmt = conn.prepare(
        "SELECT id,label,IFNULL(actor,''),steps,created_ts,verdict,critical_steps,fingerprint,engine_epoch
         FROM sandbox_run ORDER BY id DESC LIMIT ?1",
    )?;
    let rows: Vec<Value> = stmt
        .query_map(params![limit as i64], |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "label": r.get::<_, String>(1)?,
                "actor": r.get::<_, String>(2)?,
                "steps": r.get::<_, i64>(3)?,
                "created_ts": r.get::<_, i64>(4)?,
                "verdict": r.get::<_, String>(5)?,
                "critical_steps": r.get::<_, i64>(6)?,
                "fingerprint": r.get::<_, String>(7)?,
                "engine_epoch": r.get::<_, i64>(8)?,
            }))
        })?
        .filter_map(|x| x.ok())
        .collect();
    Ok(json!({"command": "world-gov sandbox list", "count": rows.len(), "runs": rows}))
}

/// 取单条（含 scenario 与逐步状态）。
pub fn show(conn: &Connection, run_id: i64, full: bool) -> Result<Value> {
    ensure_table(conn)?;
    let row: Option<(i64, String, Option<String>, i64, i64, String, String, String, i64, String, i64)> = conn
        .query_row(
            "SELECT id,label,actor,steps,created_ts,scenario_json,final_json,verdict,critical_steps,fingerprint,engine_epoch
             FROM sandbox_run WHERE id=?1",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?)),
        )
        .optional()?;
    let Some((id, label, actor, steps, ts, sc, fin, verdict, crit, fp, epoch)) = row else {
        return Ok(json!({"command": "world-gov sandbox show", "run_id": run_id, "status": "NOT_FOUND"}));
    };
    let mut out = json!({
        "command": "world-gov sandbox show",
        "run_id": id,
        "label": label,
        "actor": actor,
        "steps": steps,
        "created_ts": ts,
        "verdict": verdict,
        "critical_steps": crit,
        "fingerprint": fp,
        "engine_epoch": epoch,
        "final_state": serde_json::from_str::<Value>(&fin).unwrap_or(json!({})),
    });
    if full {
        out["scenario"] = serde_json::from_str::<Value>(&sc).unwrap_or(json!({}));
        let mut stmt = conn.prepare("SELECT step,state_json FROM sandbox_step WHERE run_id=?1 ORDER BY step")?;
        let stepsv: Vec<Value> = stmt
            .query_map(params![run_id], |r| {
                let sj: String = r.get(1)?;
                Ok(json!({"step": r.get::<_, i64>(0)?, "state": serde_json::from_str::<Value>(&sj).unwrap_or(json!({}))}))
            })?
            .filter_map(|x| x.ok())
            .collect();
        out["steps_detail"] = json!(stepsv);
    }
    Ok(out)
}

/// 删除推演记录（含步骤）。
pub fn delete(conn: &Connection, run_id: i64) -> Result<bool> {
    ensure_table(conn)?;
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM sandbox_step WHERE run_id=?1", params![run_id])?;
    let n = tx.execute("DELETE FROM sandbox_run WHERE id=?1", params![run_id])?;
    tx.commit()?;
    Ok(n > 0)
}

/// 从逻辑快照派生沙箱初始态（第 17 批）—— **只读**，不落库、不推演。
///
/// 快照只存索引元信息（key/layer/mtime/hash），不含正文；故派生出的沙箱
/// `world` 采用**确定性**映射：以快照条目数与元指纹为相位输入，绝不臆造剧情数值。
/// `--save` 才把该初始态作为 scenario 落库成一条 0 步基线记录（可选）。
pub fn spawn_from_snapshot(
    conn: &Connection,
    lib_root: &Path,
    snap_id: &str,
    save: bool,
) -> Result<Value> {
    ensure_table(conn)?;
    let meta = crate::governance::diff_snapshot::read_meta(conn, snap_id)?;
    let Some(meta) = meta else {
        return Ok(json!({
            "command": "world-gov sandbox spawn",
            "status": "SNAPSHOT_NOT_FOUND",
            "snap_id": snap_id,
            "note": "未找到该快照；先 world-gov diff snap create --label ...",
        }));
    };
    let n = meta.len();
    let meta_fp = crate::governance::diff_snapshot::meta_fingerprint(&meta);
    // 确定性初始相：由快照规模 + 指纹首字节派生（纯函数，可重放；不涉剧情语义）。
    let b0 = u8::from_str_radix(meta_fp.get(0..2).unwrap_or("00"), 16).unwrap_or(0);
    let phase = 30.0 + (b0 as f64 / 255.0) * 40.0; // [30,70)
    let scenario = json!({
        "steps": 0,
        "world": {
            "chapter": 0, "last_chapter": 0,
            "phase": (phase * 100.0).round() / 100.0,
            "stance": "守成", "entropy_debt": 0.0, "intensity": 1.0,
            "age_ch": 0, "decay_rate": 0.92,
        },
        "ops": [],
        "spawned_from_snap": snap_id,
    });
    let affected = if let Some(first) = meta.first() {
        affected_cards(conn, &first.key)
    } else {
        json!({"actor": null, "cards": []})
    };
    let mut out = json!({
        "command": "world-gov sandbox spawn",
        "status": "OK",
        "snap_id": snap_id,
        "entries": n,
        "meta_fingerprint": meta_fp,
        "scenario": scenario,
        "affected": affected,
        "saved": false,
        "note": "从逻辑快照派生的沙箱初始态（确定性；只读快照，不复制正文）。用 sandbox run --scenario <导出文件> 推进。",
    });
    let _ = lib_root;
    if save {
        let (final_state, steps, verdict, crit) = simulate(&scenario)?;
        let fp = fingerprint(&scenario, &final_state, &steps);
        let ts = now_secs();
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO sandbox_run(label,actor,steps,created_ts,scenario_json,final_json,verdict,critical_steps,fingerprint,engine_epoch)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                format!("spawn:{snap_id}"), Option::<String>::None, steps.len() as i64, ts,
                serde_json::to_string(&scenario)?, serde_json::to_string(&final_state)?,
                verdict, crit as i64, fp, ENGINE_EPOCH
            ],
        )?;
        let run_id = tx.last_insert_rowid();
        tx.commit()?;
        out["saved"] = json!(true);
        out["run_id"] = json!(run_id);
    }
    Ok(out)
}

/// trace 轨迹：把一次推演的逐步状态列成可读轨迹（只读，不落库）。
pub fn trace(conn: &Connection, run_id: i64) -> Result<Value> {
    ensure_table(conn)?;
    let head: Option<(String, i64, String, i64)> = conn
        .query_row(
            "SELECT IFNULL(label,''),steps,verdict,engine_epoch FROM sandbox_run WHERE id=?1",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((label, steps, verdict, epoch)) = head else {
        return Ok(json!({"command": "world-gov sandbox trace", "run_id": run_id, "status": "NOT_FOUND"}));
    };
    let mut stmt = conn.prepare("SELECT step,state_json FROM sandbox_step WHERE run_id=?1 ORDER BY step")?;
    let trace: Vec<Value> = stmt
        .query_map(params![run_id], |r| {
            let sj: String = r.get(1)?;
            let v: Value = serde_json::from_str(&sj).unwrap_or(json!({}));
            Ok(json!({
                "step": r.get::<_, i64>(0)?,
                "chapter": v["chapter"],
                "verdict": v["verdict"],
                "state": v["state"],
                "op": v["op"],
            }))
        })?
        .filter_map(|x| x.ok())
        .collect();
    Ok(json!({
        "command": "world-gov sandbox trace",
        "run_id": run_id,
        "label": label,
        "steps": steps,
        "verdict": verdict,
        "engine_epoch": epoch,
        "trace": trace,
        "note": "逐步轨迹（只读；数值来自 world_physics，可重放）。",
    }))
}

/// 供 self-check：结构 + 脏检查（只读）。
pub fn table_check(conn: &Connection) -> Result<Value> {
    let exists: i64 = conn
        .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='sandbox_run'", [], |r| r.get(0))
        .unwrap_or(0);
    if exists == 0 {
        return Ok(json!({"ok": false, "missing_table": "sandbox_run", "hint": "跑任意 world-gov 命令（open_gov 幂等建表）即可创建"}));
    }
    let runs: i64 = conn.query_row("SELECT COUNT(*) FROM sandbox_run", [], |r| r.get(0)).unwrap_or(0);
    let steps: i64 = conn.query_row("SELECT COUNT(*) FROM sandbox_step", [], |r| r.get(0)).unwrap_or(0);
    let mut dirty: Vec<Value> = Vec::new();
    let mut stmt = conn.prepare("SELECT id,steps,fingerprint FROM sandbox_run")?;
    for x in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?)))?.filter_map(|v| v.ok()) {
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM sandbox_step WHERE run_id=?1", params![x.0], |r| r.get(0)).unwrap_or(0);
        if n != x.1 {
            dirty.push(json!({"run_id": x.0, "why": "step_count_mismatch", "declared": x.1, "actual": n}));
        } else if x.2.len() != 40 {
            dirty.push(json!({"run_id": x.0, "why": "bad_fingerprint", "fingerprint": x.2}));
        }
    }
    Ok(json!({"ok": dirty.is_empty(), "runs": runs, "steps": steps, "dirty": dirty}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        ensure_table(&c).unwrap();
        c
    }

    fn base_scenario() -> Value {
        json!({
            "steps": 5,
            "world": {"chapter": 10, "last_chapter": 9, "phase": 60.0, "stance": "扩张",
                       "entropy_debt": 10.0, "intensity": 0.8, "age_ch": 0, "decay_rate": 0.92},
            "ops": [{"power_used": 5, "power_cost": 4.0}]
        })
    }

    fn seed_lib(c: &Connection) {
        c.execute_batch("CREATE TABLE library_index(key TEXT); CREATE TABLE rel_edge(id INTEGER PRIMARY KEY, src_key TEXT, dst_key TEXT, observed_in TEXT, ts INTEGER);").unwrap();
    }

    #[test]
    fn table_isolated_and_idempotent() {
        let c = Connection::open_in_memory().unwrap();
        ensure_table(&c).unwrap();
        ensure_table(&c).unwrap();
        let names: Vec<String> = c
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .filter_map(|v| v.ok())
            .collect();
        assert!(names.contains(&"sandbox_run".to_string()));
        assert!(!names.contains(&"library_index".to_string()));
    }

    #[test]
    fn simulate_is_deterministic() {
        let sc = base_scenario();
        let a = simulate(&sc).unwrap();
        let b = simulate(&sc).unwrap();
        assert_eq!(a.0, b.0, "最终态一致");
        assert_eq!(a.1.len(), 5);
        assert_eq!(a.2, b.2, "判决一致");
        assert_eq!(fingerprint(&sc, &a.0, &a.1), fingerprint(&sc, &b.0, &b.1), "指纹一致");
    }

    #[test]
    fn simulate_multistep_entropy_accumulates() {
        let sc = json!({
            "steps": 3,
            "world": {"chapter": 1, "last_chapter": 0, "phase": 50.0, "stance": "守成",
                       "entropy_debt": 0.0, "intensity": 1.0, "age_ch": 0},
            "ops": [{"power_used": 5, "power_cost": 4.0}]
        });
        let (fin, steps, verdict, _) = simulate(&sc).unwrap();
        // 每步 +20 熵债 → 3 步后 60（>= warn 阈值 40）
        assert!((fin["entropy_debt"].as_f64().unwrap() - 60.0).abs() < 1e-6);
        assert_eq!(verdict, "warn");
        assert_eq!(steps[0]["verdict"], "ok");
        assert_eq!(steps[2]["verdict"], "warn");
    }

    #[test]
    fn simulate_critical_then_forgotten() {
        let sc = json!({
            "steps": 40,
            "world": {"chapter": 1, "last_chapter": 0, "phase": 10.0, "stance": "衰退",
                       "entropy_debt": 0.0, "intensity": 0.5, "age_ch": 0, "decay_rate": 0.92},
            "ops": [{"power_used": 5, "power_cost": 4.0}]
        });
        let (_fin, _steps, verdict, crit) = simulate(&sc).unwrap();
        assert_eq!(verdict, "critical");
        assert!(crit > 0);
    }

    #[test]
    fn step_verdict_ranking() {
        let ok = json!({"civ.tide":{"civ.tide.critical":"false"},"ent.entropy":{"ent.entropy.state":"ok"},"res.decay":{"res.decay.forgotten":"false"}});
        assert_eq!(step_verdict(&ok), "ok");
        let warn = json!({"civ.tide":{"civ.tide.critical":"false"},"ent.entropy":{"ent.entropy.state":"warn"},"res.decay":{"res.decay.forgotten":"false"}});
        assert_eq!(step_verdict(&warn), "warn");
        let crit = json!({"civ.tide":{"civ.tide.critical":"true"},"ent.entropy":{"ent.entropy.state":"ok"},"res.decay":{"res.decay.forgotten":"false"}});
        assert_eq!(step_verdict(&crit), "critical");
        let forgot = json!({"civ.tide":{"civ.tide.critical":"false"},"ent.entropy":{"ent.entropy.state":"ok"},"res.decay":{"res.decay.forgotten":"true"}});
        assert_eq!(step_verdict(&forgot), "critical");
    }

    #[test]
    fn state_from_scenario_defaults() {
        let s = state_from_scenario(&json!({}));
        assert_eq!(s.phase, 50.0);
        assert_eq!(s.stance, "守成");
        assert_eq!(s.decay_rate, 0.92);
    }

    #[test]
    fn run_save_then_verify_ok() {
        let c = mem();
        seed_lib(&c);
        let sc = base_scenario();
        let r = run(&c, Path::new("/tmp"), "t1", None, &sc, true).unwrap();
        assert_eq!(r["saved"], true);
        let id = r["run_id"].as_i64().unwrap();
        let v = verify(&c, id).unwrap();
        assert_eq!(v["status"], "REPLAY_OK");
        assert_eq!(v["deterministic"], true);
        let l = list(&c, 10).unwrap();
        assert_eq!(l["count"], 1);
        let s = show(&c, id, true).unwrap();
        assert_eq!(s["steps"], 5);
        assert_eq!(s["steps_detail"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn verify_not_found() {
        let c = mem();
        let v = verify(&c, 999).unwrap();
        assert_eq!(v["status"], "NOT_FOUND");
    }

    #[test]
    fn delete_removes_run_and_steps() {
        let c = mem();
        seed_lib(&c);
        let r = run(&c, Path::new("/tmp"), "t2", None, &base_scenario(), true).unwrap();
        let id = r["run_id"].as_i64().unwrap();
        assert!(delete(&c, id).unwrap());
        assert_eq!(list(&c, 10).unwrap()["count"], 0);
        let sc: i64 = c.query_row("SELECT COUNT(*) FROM sandbox_step WHERE run_id=?1", params![id], |r| r.get(0)).unwrap();
        assert_eq!(sc, 0);
    }

    #[test]
    fn dry_run_does_not_save() {
        let c = mem();
        seed_lib(&c);
        let r = run(&c, Path::new("/tmp"), "t3", None, &base_scenario(), false).unwrap();
        assert_eq!(r["saved"], false);
        assert_eq!(list(&c, 10).unwrap()["count"], 0);
    }

    #[test]
    fn steps_capped_at_max() {
        let sc = json!({"steps": 99999, "world": {}});
        let (_, steps, _, _) = simulate(&sc).unwrap();
        assert_eq!(steps.len(), MAX_STEPS as usize);
    }

    #[test]
    fn table_check_detects_missing() {
        let c = Connection::open_in_memory().unwrap();
        assert_eq!(table_check(&c).unwrap()["ok"], false);
        let c2 = mem();
        assert_eq!(table_check(&c2).unwrap()["ok"], true);
    }

    #[test]
    fn spawn_from_snapshot_not_found() {
        let c = mem();
        seed_lib(&c);
        let r = spawn_from_snapshot(&c, Path::new("/tmp"), "nonexistent", false).unwrap();
        assert_eq!(r["status"], "SNAPSHOT_NOT_FOUND");
    }

    /// 让 seed_lib 的极简 library_index 具备 diff_snapshot::collect_meta 需要的列。
    fn full_library_index(c: &Connection) {
        c.execute_batch(
            "CREATE TABLE library_index(key TEXT, layer TEXT, source TEXT, src_mtime INTEGER NOT NULL DEFAULT 0);
             INSERT INTO library_index(key,layer,source,src_mtime) VALUES('K1','L3','',0),('K2','L4','',0);",
        ).unwrap();
    }

    #[test]
    fn spawn_from_snapshot_derives_deterministic_state() {
        let c = mem();
        full_library_index(&c);
        // 造一个逻辑快照（lib_snapshot 由 diff_snapshot::create 建）
        let (sid, n) = crate::governance::diff_snapshot::create(&c, Path::new("/tmp"), "spawn-test").unwrap();
        let r = spawn_from_snapshot(&c, Path::new("/tmp"), &sid, false).unwrap();
        assert_eq!(r["status"], "OK");
        assert_eq!(r["snap_id"], sid);
        assert_eq!(r["entries"], n);
        // 确定性：同快照两次派生 → 完全一致
        let r2 = spawn_from_snapshot(&c, Path::new("/tmp"), &sid, false).unwrap();
        assert_eq!(r["meta_fingerprint"], r2["meta_fingerprint"]);
        assert_eq!(r["scenario"]["world"]["phase"], r2["scenario"]["world"]["phase"]);
        // phase 落在 [30,70)
        let ph = r["scenario"]["world"]["phase"].as_f64().unwrap();
        assert!((30.0..70.0).contains(&ph), "phase 应落在 [30,70)，实得 {ph}");
        assert_eq!(r["saved"], false);
    }

    #[test]
    fn spawn_save_creates_baseline_run() {
        let c = mem();
        full_library_index(&c);
        let (sid, _) = crate::governance::diff_snapshot::create(&c, Path::new("/tmp"), "spawn-save").unwrap();
        let r = spawn_from_snapshot(&c, Path::new("/tmp"), &sid, true).unwrap();
        assert_eq!(r["saved"], true);
        assert_eq!(list(&c, 10).unwrap()["count"], 1);
    }

    #[test]
    fn trace_lists_steps() {
        let c = mem();
        seed_lib(&c);
        let r = run(&c, Path::new("/tmp"), "t-trace", None, &base_scenario(), true).unwrap();
        let id = r["run_id"].as_i64().unwrap();
        let tr = trace(&c, id).unwrap();
        assert_eq!(tr["steps"], 5);
        assert_eq!(tr["trace"].as_array().unwrap().len(), 5);
        assert_eq!(tr["trace"][0]["step"], 0);
    }

    #[test]
    fn trace_not_found() {
        let c = mem();
        assert_eq!(trace(&c, 12345).unwrap()["status"], "NOT_FOUND");
    }
}