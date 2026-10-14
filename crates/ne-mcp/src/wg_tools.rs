//! ne-mcp 的 world-gov 能力封装（第 18 批 · 待办②「MCP 可调用接口集」）。
//!
//! # 为什么直接依赖 `ne-agent`（而不是 fork 子进程）
//!
//! `ne-agent` 的 `governance` / `world_gov` 全部是
//! `pub fn f(conn: &Connection, lib_root: &Path, …) -> Result<Value>` 形式的**纯库函数**，
//! 只依赖 `serde_json` / `anyhow` / `rusqlite` / `std` + `ne_engine::{http,world_physics}`，
//! 不触达 session/tool/memory 等重模块。因此 ne-mcp 直接 `use ne_agent::…` 复用同一实现，
//! 不 fork shell、不另起一套（对齐 SKILL.md 硬约束 1 与用户「底层全部复用成熟逻辑」）。
//!
//! # 三重锁（真实 LLM 调用沿用同一收口）
//!
//! 真实外呼仍走 `semantic_pipeline::call_raw` 的唯一收口：
//! 编译 feature `llm_integration`（ne-mcp 透传到 ne-agent）+
//! 环境变量 `WOGOV_ALLOW_REAL_LLM=yes` + 工具参数 `real_semantic=true`。
//! 默认三者不全 → 返回 `GATE_LOCKED`，**绝不外呼、不消耗 token**。
//!
//! # 库路径语义
//!
//! `Cfg.root`（`--root` 传入）与 ne-agent 的 `lib_root`（`--work-root`）是**同一个东西**：
//! 含 `novel.db` / `02_资料库` / `04_正文` 的资料库根。

use anyhow::Result;
use serde_json::{json, Value};

use crate::tools::Cfg;

/// 打开治理库（只读既有库 + 幂等附加治理表；与主索引物理隔离）。
fn gov(cfg: &Cfg) -> Result<rusqlite::Connection> {
    ne_agent::governance::open_gov(&cfg.root)
}

/// 最近一次逻辑快照的 snap_id（冲突记录锚点）；无快照则 None。
fn latest_snap(conn: &rusqlite::Connection) -> Option<String> {
    ne_agent::governance::diff_snapshot::list(conn)
        .ok()
        .and_then(|v| v.into_iter().next())
        .and_then(|x| x["snap_id"].as_str().map(String::from))
}

// ─────────────────────────────────────── 1. 全库语义扫描（scan-library）
/// 扫描全库（或分片）产语义脏点探针清单；`semantic=true` 只标注通路，不在此处外呼。
pub fn scan_library(
    cfg: &Cfg,
    slice: Option<&str>,
    semantic: bool,
    dry_run: bool,
    all_layers: bool,
    since: Option<&str>,
) -> Result<Value> {
    let conn = gov(cfg)?;
    let (since_ts, since_note) = match since {
        Some(v) => {
            let (t, n) = ne_agent::governance::scan_snapshot::parse_since(v, &cfg.root)?;
            (t, Some(n))
        }
        None => (None, None),
    };
    let r = ne_agent::world_gov::scan_library(
        &conn,
        &cfg.root,
        slice,
        semantic,
        dry_run,
        all_layers,
        since_ts,
        None,
        None,
    )?;
    let mut out = r;
    if let Some(n) = since_note {
        out["since_note"] = json!(n);
    }
    Ok(out)
}

// ─────────────────────────────────────── 2. 单卡检视（inspect-card）
/// 对单卡产探针包；`real_semantic=true` 才走统一收口真实推理 + 落冲突库。
pub fn inspect_card(
    cfg: &Cfg,
    key: &str,
    semantic: bool,
    real_semantic: bool,
    dry_run: bool,
) -> Result<Value> {
    let conn = gov(cfg)?;
    let rec = ne_agent::world_gov::inspect_card(&conn, &cfg.root, key)?;
    if rec["status"] == "NOT_FOUND" || !semantic {
        return Ok(rec);
    }
    let built = ne_agent::governance::prompt_builder::build_inspect_prompt(&rec["probe_pack"]);
    if dry_run {
        return Ok(json!({
            "command": "wg_inspect_card", "key": key, "dry_run": true,
            "system": built.system, "user": built.user,
            "gate": ne_agent::governance::llm_gate::lock_status(real_semantic),
        }));
    }
    infer_one(&conn, cfg, key, &rec["probe_pack"], &built, real_semantic)
}

/// 逐卡真实推理（inspect-card / inspect-batch 共用；唯一收口 `call_raw` + 冲突库入库）。
fn infer_one(
    conn: &rusqlite::Connection,
    cfg: &Cfg,
    key: &str,
    probe_pack: &Value,
    built: &ne_agent::governance::prompt_builder::BuiltPrompt,
    real_semantic: bool,
) -> Result<Value> {
    use ne_agent::governance::{conflict_store as cs, llm_config as lc, prompt_builder as pb, semantic_pipeline as sp};
    let llm_cfg = lc::load(&cfg.root)?;
    let probe_hash = ne_agent::governance::digest::sha1_hex(probe_pack.to_string().as_bytes());
    let baseline_fp = ne_agent::world_gov::read_baseline_fingerprint(&cfg.root).unwrap_or_default();
    let snap_id = latest_snap(conn);
    let raw = sp::RawPrompt {
        system: built.system.clone(),
        user: built.user.clone(),
        probe_hash: probe_hash.clone(),
        schema: pb::inspect_schema(),
        entity_fields: vec!["referenced_ids".to_string()],
        ..Default::default()
    };
    let outcome = sp::call_raw(conn, &cfg.root, &llm_cfg, real_semantic, &raw)?;
    let oj = outcome.to_json();
    let status = oj["status"].as_str().unwrap_or("?").to_string();
    let (content_opt, tok_in, tok_out) = match &outcome {
        sp::Outcome::Success { content, token_in, token_out, .. } => (Some(content.clone()), *token_in, *token_out),
        sp::Outcome::CacheHit { content, .. } => (Some(content.clone()), 0i64, 0i64),
        _ => (None, 0i64, 0i64),
    };
    let is_cache_hit = matches!(outcome, sp::Outcome::CacheHit { .. });
    let mut conflict_id = None;
    let mut verdict = Value::Null;
    // 第 18.5 批：CacheHit 去重——命中缓存意味着同 probe_hash 的完全相同输出，
    // 若该卡已有 pending 观点则跳过 insert（零信息增量，避免重复入库放大复核噪音）。
    let mut cache_dedup = false;
    if is_cache_hit {
        if let Some(existing) = cs::find_pending_by_probe(conn, key)? {
            conflict_id = Some(existing);
            cache_dedup = true;
        }
    }
    if let Some(content) = content_opt {
        let parsed = ne_agent::world_gov::parse_semantic_verdict(key, &content.to_string());
        if parsed.get("verdict").is_some() {
            if conflict_id.is_none() {
                conflict_id = Some(cs::insert(conn, &cs::NewConflict {
                    probe_key: key.to_string(),
                    snap_id: snap_id.clone(),
                    baseline_fp: baseline_fp.clone(),
                    verdict: parsed.clone(),
                    raw_prompt: built.user.clone(),
                    raw_response: content.to_string(),
                    token_in: tok_in,
                    token_out: tok_out,
                })?);
                let mut f = parsed.clone();
                if f.get("key").is_none() {
                    f["key"] = json!(key);
                }
                let _ = ne_agent::world_gov::emit_semantic_findings(conn, "world-gov inspect [mcp]", &[f]);
            }
            verdict = parsed["verdict"].clone();
        }
    }
    Ok(json!({
        "command": "wg_inspect_card", "key": key, "status": status,
        "cache_dedup": cache_dedup,
        "pipeline": oj, "verdict": verdict, "conflict_id": conflict_id,
        "audit": {"probe_hash": probe_hash, "baseline_fp": baseline_fp, "snap_id": snap_id},
    }))
}

// ─────────────────────────────────────── 3. 批量检视（inspect-batch）
/// 批量检视：`keys` 显式列表（空=全库），`slice` 分片 `N/M`；`real_semantic=true` 才逐卡推理。
pub fn inspect_batch(
    cfg: &Cfg,
    keys: Option<&[String]>,
    slice: Option<&str>,
    all_layers: bool,
    real_semantic: bool,
) -> Result<Value> {
    use ne_agent::governance::inspect_batch as ib;
    let conn = gov(cfg)?;
    // 候选 key 全集
    let all_keys: Vec<String> = match keys {
        Some(ks) if !ks.is_empty() => ks.to_vec(),
        _ => ib::resolve_keys(&conn, &cfg.root, &ib::BatchSource::All, all_layers)?.0,
    };
    // 分片（在候选全集上切）
    let selected = match slice {
        Some(s) => {
            let (n, m) = ne_agent::world_gov::parse_slice_strict(s).map_err(|e| anyhow::anyhow!("{e}"))?;
            ib::slice_keys(all_keys, n, m)
        }
        None => all_keys,
    };
    if !real_semantic {
        return Ok(json!({
            "command": "wg_inspect_batch", "count": selected.len(),
            "slice": slice, "keys": selected, "note": "仅预览；real_semantic=true 才逐卡真实推理",
        }));
    }
    let mut results: Vec<Value> = Vec::new();
    let gate = ne_agent::governance::llm_gate::lock_status(true);
    if !ne_agent::governance::llm_gate::real_llm_unlocked(true) {
        return Ok(json!({
            "command": "wg_inspect_batch", "status": "REAL_SEMANTIC_LOCKED",
            "llm_gate": gate, "count": selected.len(),
            "note": "三重锁未全开；未发出任何请求，未消耗 token。",
        }));
    }
    for k in &selected {
        let rec = ne_agent::world_gov::inspect_card(&conn, &cfg.root, k)?;
        if rec["status"] == "NOT_FOUND" {
            results.push(json!({"key": k, "status": "NOT_FOUND"}));
            continue;
        }
        let built = ne_agent::governance::prompt_builder::build_inspect_prompt(&rec["probe_pack"]);
        results.push(infer_one(&conn, cfg, k, &rec["probe_pack"], &built, true)?);
    }
    Ok(json!({
        "command": "wg_inspect_batch", "count": results.len(),
        "slice": slice, "inference": results,
    }))
}

// ─────────────────────────────────────── 4. 快照管理（snapshot）
/// 逻辑快照管理：create | list | show | delete（只存索引+元信息哈希，不复制正文）。
pub fn snapshot(
    cfg: &Cfg,
    action: &str,
    label: Option<&str>,
    snap_id: Option<&str>,
) -> Result<Value> {
    use ne_agent::governance::diff_snapshot as ds;
    let conn = gov(cfg)?;
    match action {
        "create" => {
            let lab = label.unwrap_or("");
            let (sid, n) = ds::create(&conn, &cfg.root, lab)?;
            Ok(json!({
                "command": "wg_snapshot create", "snap_id": sid,
                "label": if lab.is_empty() { "(auto)" } else { lab }, "entry_count": n,
                "note": "逻辑快照：只存索引+元信息哈希，未复制任何正文。",
            }))
        }
        "list" => {
            let rows = ds::list(&conn)?;
            Ok(json!({"command": "wg_snapshot list", "count": rows.len(), "snapshots": rows}))
        }
        "show" => {
            let sid = snap_id.ok_or_else(|| anyhow::anyhow!("show 需要 snap_id"))?;
            match ds::show_meta(&conn, sid)? {
                Some(m) => Ok(m),
                None => Ok(json!({"status": "NOT_FOUND", "snap_id": sid})),
            }
        }
        "delete" => {
            let sid = snap_id.ok_or_else(|| anyhow::anyhow!("delete 需要 snap_id"))?;
            let ok = ds::delete(&conn, sid)?;
            Ok(json!({"command": "wg_snapshot delete", "snap_id": sid, "deleted": ok}))
        }
        other => anyhow::bail!("snapshot 未知 action：{other}（create|list|show|delete）"),
    }
}

// ─────────────────────────────────────── 5. 冲突查询（conflict）
/// 冲突记录库（AI 观点，人是终审）：list | show | summary。
pub fn conflict(
    cfg: &Cfg,
    action: &str,
    status: Option<&str>,
    key: Option<&str>,
    id: Option<i64>,
) -> Result<Value> {
    use ne_agent::governance::conflict_store as cs;
    let conn = gov(cfg)?;
    match action {
        "list" => {
            let rows = cs::list(&conn, status, key)?;
            Ok(json!({"command": "wg_conflict list", "count": rows.len(), "records": rows}))
        }
        "show" => {
            let id = id.ok_or_else(|| anyhow::anyhow!("show 需要 id"))?;
            match cs::show(&conn, id)? {
                Some((j, _p, _r)) => Ok(j),
                None => Ok(json!({"status": "NOT_FOUND", "id": id})),
            }
        }
        "summary" => Ok(cs::summary(&conn)?),
        other => anyhow::bail!("conflict 未知 action：{other}（list|show|summary）"),
    }
}

// ─────────────────────────────────────── 6. 沙盘推演（sandbox）
/// 沙盘：run | list | show | spawn（确定性多步推演，可重放；与基准库强隔离）。
pub fn sandbox(
    cfg: &Cfg,
    action: &str,
    quick: Option<&str>,
    scenario: Option<&Value>,
    actor: Option<&str>,
    save: bool,
    run_id: Option<i64>,
    from_snap: Option<&str>,
) -> Result<Value> {
    use ne_agent::governance::sandbox as sb;
    let conn = gov(cfg)?;
    match action {
        "run" => {
            let sc = match (scenario, quick) {
                (Some(v), None) => v.clone(),
                (None, Some(q)) => sb::quick_scenario(q)?,
                (Some(_), Some(_)) => anyhow::bail!("scenario 与 quick 只能二选一"),
                (None, None) => anyhow::bail!("需给 scenario（对象）或 quick（warn|critical|tip|calm）"),
            };
            Ok(sb::run(&conn, &cfg.root, "", actor, &sc, save)?)
        }
        "list" => Ok(sb::list(&conn, 50)?),
        "show" => {
            let id = run_id.ok_or_else(|| anyhow::anyhow!("show 需要 run_id"))?;
            Ok(sb::show(&conn, id, true)?)
        }
        "spawn" => {
            let sid = from_snap.ok_or_else(|| anyhow::anyhow!("spawn 需要 from_snap"))?;
            Ok(sb::spawn_from_snapshot(&conn, &cfg.root, sid, save)?)
        }
        "verify" => {
            let id = run_id.ok_or_else(|| anyhow::anyhow!("verify 需要 run_id"))?;
            Ok(sb::verify(&conn, id)?)
        }
        "trace" => {
            let id = run_id.ok_or_else(|| anyhow::anyhow!("trace 需要 run_id"))?;
            Ok(sb::trace(&conn, id)?)
        }
        other => anyhow::bail!("sandbox 未知 action：{other}（run|list|show|spawn|verify|trace）"),
    }
}

// ─────────────────────────────────────── 7. LLM 状态与缓存（llm）
/// LLM 状态：gate | stats | cache | backends | config（**全部只读，不外呼**）。
pub fn llm_status(cfg: &Cfg, action: &str) -> Result<Value> {
    use ne_agent::governance::{inference_cache as ic, llm_adapter as la, llm_config as lc, llm_obs as lo};
    match action {
        "gate" => Ok(ne_agent::governance::llm_gate::lock_status(false)),
        "stats" => {
            let conn = gov(cfg)?;
            Ok(lo::stats(&conn)?)
        }
        "cache" => {
            let c = lc::load(&cfg.root)?;
            Ok(ic::status(&cfg.root, c.cache_ttl_days)?)
        }
        "backends" => {
            let c = lc::load(&cfg.root)?;
            Ok(la::available_backends(&c))
        }
        "config" => {
            let c = lc::load(&cfg.root)?;
            Ok(lc::safe_snapshot(&c))
        }
        other => anyhow::bail!("llm 未知 action：{other}（gate|stats|cache|backends|config）"),
    }
}

// ─────────────────────────────────────── 8. 健康自检（self-check / sys-check）
/// 健康自检（环境/索引/目录/管线）；`fix=true` 仅创建缺失目录。
pub fn self_check(cfg: &Cfg, fix: bool) -> Result<Value> {
    let conn = gov(cfg)?;
    Ok(ne_agent::governance::self_check::run(&conn, &cfg.root, fix, 30)?)
}
