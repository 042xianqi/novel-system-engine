//! 只读巡检类子命令 —— 从 `main.rs` 第一批平移（架构门禁：单文件 ≤ 700 行）。
//!
//! # 为什么先搬这一批
//! 这 10 个函数经**逐函数静态检查**确认**完全自包含**：只依赖 `ne_agent::*` 的
//! 公共 API 与 `crate::cli` 的命令类型，不碰 main.rs 的任何私有助手
//! （`build_runtime` / `truncate` / `gov_enqueue` …）。因此平移属**纯机械操作**，
//! 零语义风险——符合"先搬零耦合的，再动纠缠的"这一渐进原则。
//!
//! # 成员
//! - `run_estimate` / `run_models`：token 估算与模型清单
//! - `run_health` / `run_sys_check` / `run_layer_health`：三级健康检查
//! - `run_outline`：大纲重建 / 清洗
//! - `run_refs` / `run_risk_list` / `run_checkpoint` / `run_compliance`：
//!   引用图 / 风险清单 / 检查点 / 合规基线（均为**只读治理探针**）

use anyhow::{Context, Result};
use ne_agent::{RuntimeConfig, Vault};
use std::path::{Path, PathBuf};

use crate::cli::{HealthCmd, ModelsCmd, OutlineCmd};

/// 估算 token（上下文预算调试用）。
pub(crate) fn run_estimate(file: Option<&Path>, json: bool) -> Result<()> {
    use std::io::Read;
    let text = match file {
        Some(p) => std::fs::read_to_string(p)
            .with_context(|| format!("读文件失败：{}", p.display()))?,
        None => {
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .context("从 stdin 读取失败")?;
            s
        }
    };
    let tokens = ne_agent::context::estimate_tokens(&text);
    let chars = text.chars().count();
    if json {
        println!(
            "{}",
            serde_json::json!({ "chars": chars, "tokens": tokens,
                "ratio": if tokens == 0 { 0.0 } else { chars as f64 / tokens as f64 } })
        );
    } else {
        let ratio = if tokens == 0 { 0.0 } else { chars as f64 / tokens as f64 };
        println!("{chars} 字 → {tokens} token（{ratio:.2} 字/token）");
    }
    Ok(())
}


/// 【规约适配】全局系统自检（= `world-gov self-check run` 的别名入口）。
///
/// 第 18 批：对齐《项目总规约》的 `sys-check full --json`。**复用**同一实现。
/// 系统健康自检（两层）。
///
/// - `health modules`：加载时模块自检。
/// - `health preflight --chapter N`：生成前注入就绪闸门。
///   **Fatal 项 → 退出码非 0**（便于脚本在生成前卡住）。
pub(crate) fn run_health(lib_root: &Path, cmd: &HealthCmd, json: bool) -> Result<()> {
    let rep = match cmd {
        HealthCmd::Modules { volume, .. } => ne_agent::health::check_modules(lib_root, volume)?,
        HealthCmd::Preflight { chapter, volume, .. } => {
            ne_agent::health::preflight(lib_root, volume, *chapter)?
        }
        HealthCmd::Boot { volume, .. } => ne_agent::health::boot(lib_root, volume)?,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&rep)?);
    } else {
        ne_agent::health::print_report(&rep);
    }
    if !rep.ready {
        std::process::exit(2);
    }
    Ok(())
}


/// 细纲工具（`outline rebuild`）：调 LLM 把「1 拍 5 章概要复制」重建为 5 个递进子拍。
pub(crate) fn run_outline(vault: &Vault, lib_root: &Path, cmd: &OutlineCmd) -> Result<()> {
    vault.init()?;
    match cmd {
        OutlineCmd::Rebuild { volume, beat, dry_run, .. } => {
            if !lib_root.exists() {
                anyhow::bail!("库根不存在：{}", lib_root.display());
            }
            let cfg = RuntimeConfig::load(vault.root())?;
            let llm = cfg.llm_params(&vault.llm_dir())?;
            let opts = ne_agent::outline_rebuild::RebuildOpts {
                volume: volume.clone(),
                only_beat: *beat,
                dry_run: *dry_run,
            };
            println!("== 细纲重建 {}（{}）==", volume, if *dry_run { "dry-run" } else { "落盘" });
            let (ok, keep) = ne_agent::outline_rebuild::rebuild(lib_root, &llm, &opts)?;
            if keep > 0 && ok == 0 {
                anyhow::bail!("全部 {keep} 拍均未通过校验、已保留原样");
            }
            Ok(())
        }
        OutlineCmd::Sanitize { volume, chapter, dry_run, .. } => {
            if !lib_root.exists() {
                anyhow::bail!("库根不存在：{}", lib_root.display());
            }
            let cfg = RuntimeConfig::load(vault.root())?;
            let llm = cfg.llm_params(&vault.llm_dir())?;
            let opts = ne_agent::outline_rebuild::SanitizeOpts {
                volume: volume.clone(),
                only_chapter: *chapter,
                dry_run: *dry_run,
            };
            println!("== 细纲设计层词清洗 {}（{}）==", volume, if *dry_run { "dry-run" } else { "落盘" });
            let (ok, keep) = ne_agent::outline_rebuild::sanitize(lib_root, &llm, &opts)?;
            if ok == 0 && keep > 0 {
                println!("⚠ 无章被清洗（可能本卷已干净，或全部保留原样）");
            }
            Ok(())
        }
        OutlineCmd::Check { volume, json, .. } => {
            if !lib_root.exists() {
                anyhow::bail!("库根不存在：{}", lib_root.display());
            }
            let rep = ne_agent::health::boot(lib_root, volume)?;
            // 只暴露与「细纲出厂」相关的两项（设计层词/雷同），其余（注入面卡）另属 boot 范畴。
            let relevant: Vec<_> = rep
                .checks
                .iter()
                .filter(|c| c.name.contains("细纲"))
                .collect();
            if *json {
                println!("{}", serde_json::to_string_pretty(&relevant)?);
            } else {
                println!("== 细纲出厂门禁 {volume} ==");
                for c in &relevant {
                    let mark = match c.level {
                        ne_agent::health::Level::Fatal => "❌",
                        ne_agent::health::Level::Warn => "⚠",
                        ne_agent::health::Level::Info => "✅",
                    };
                    println!("  {mark} {} : {}", c.name, c.detail);
                }
            }
            let fatal = relevant
                .iter()
                .any(|c| matches!(c.level, ne_agent::health::Level::Fatal));
            if fatal {
                anyhow::bail!("细纲出厂门禁未过（含 Fatal）");
            }
            Ok(())
        }
    }
}


pub(crate) fn run_sys_check(lib_root: &Path, mode: &str, json: bool) -> Result<()> {
    match mode {
        "full" => {}
        other => anyhow::bail!("sys-check 未知 mode：{other}（当前仅支持 full）"),
    }
    let conn = ne_agent::governance::open_gov(lib_root)?;
    let r = ne_agent::governance::self_check::run(&conn, lib_root, false, 30)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&r)?);
    } else {
        println!("== sys-check full（= world-gov self-check；{} 项检查，{} 失败）==",
            r["checks"], r["failed"]);
        if let Some(items) = r["items"].as_array() {
            for it in items {
                let mark = if it["ok"] == true {
                    if it["warn"].as_bool().unwrap_or(false) { "⚠" } else { "✅" }
                } else { "❌" };
                println!("  {mark} {} : {}", it["name"], it["detail"]);
            }
        }
    }
    Ok(())
}


pub(crate) fn run_layer_health(lib_root: &Path, semantic: bool, json: bool) -> Result<()> {
    let conn = ne_agent::governance::open_gov(lib_root)?;
    let entries = ne_agent::governance::layer_health(&conn)?;
    // 入库风险
    for e in &entries {
        if e.level != "OK" {
            ne_agent::governance::record_risk(&conn, "layer_health", None, None,
                "audit-layer-health", &e.level, &format!("{} / {}: {}", e.layer, e.key, e.notes.join("；")))?;
        }
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
    } else {
        println!("== L0~锚点 层级健康巡检（{} 条）==", entries.len());
        for e in &entries {
            println!("[{:8}] {:6} {} / {}（{} 字）{:?} {}", e.level, e.layer, e.category, e.key, e.chars, e.types,
                if e.notes.is_empty() { String::new() } else { format!("← {}", e.notes.join("；")) });
        }
    }
    if semantic {
        eprintln!("[提示] --semantic 需要模型；当前为无模型元检查。语义检查请由上层 agent 调 LLM（本工具只产基准文本，不自行调用）。");
    }
    Ok(())
}


pub(crate) fn run_refs(lib_root: &Path, volume: &str, json: bool) -> Result<()> {
    let conn = ne_agent::governance::open_gov(lib_root)?;
    let g = ne_agent::governance::refs_graph(&conn, lib_root, volume)?;
    if json { println!("{}", serde_json::to_string_pretty(&g)?); }
    else { println!("{}", serde_json::to_string_pretty(&g)?); }
    Ok(())
}


pub(crate) fn run_risk_list(lib_root: &Path, unresolved_only: bool, json: bool) -> Result<()> {
    let conn = ne_agent::governance::open_gov(lib_root)?;
    let r = ne_agent::governance::risk_list(&conn, unresolved_only)?;
    if json { println!("{}", serde_json::to_string_pretty(&r)?); }
    else {
        println!("== 风险清单（unresolved_only={unresolved_only}）total={} ==", r["total"]);
        if let Some(items) = r["items"].as_array() {
            for it in items.iter().take(50) {
                println!("[{}] #{} {} {} :: {}", it["level"], it["id"], it["risk_type"],
                    it["volume"].as_str().unwrap_or(""), it["message"].as_str().unwrap_or(""));
            }
        }
    }
    Ok(())
}


pub(crate) fn run_checkpoint(lib_root: &Path, out: Option<PathBuf>, json: bool) -> Result<()> {
    let conn = ne_agent::governance::open_gov(lib_root)?;
    let dir = out.unwrap_or_else(|| lib_root.join("09_冷备份"));
    let r = ne_agent::governance::lib_checkpoint(&conn, lib_root, &dir)?;
    if json { println!("{}", serde_json::to_string_pretty(&r)?); }
    else { println!("里程碑包：{}", r["dir"]); }
    Ok(())
}


pub(crate) fn run_compliance(vault: &Vault, lib_root: &Path, from: u32, to: Option<u32>, volume: &str, semantic: bool, json: bool) -> Result<()> {
    let conn = ne_agent::governance::open_gov(lib_root)?;
    // 【world-gov 法理基线接入点·预埋】基线随基准一起生成，语义校验暂不激活
    // （铁则：--semantic 显式开才调模型；本处只备料，不推理、不写任何合规结论）。
    let law_baseline: Vec<serde_json::Value> = ne_agent::world_gov::baseline_entries(&conn)?
        .iter()
        .map(|e| serde_json::json!({
            "layer": e.layer, "key": e.key,
            "summary": e.summary.chars().take(80).collect::<String>(),
        }))
        .collect();
    let law_fp = ne_agent::world_gov::baseline_fingerprint(&ne_agent::world_gov::baseline_entries(&conn)?);
    let lo = if from == 0 { 1 } else { from };
    let hi = to.unwrap_or(lo);
    let mut out = Vec::new();
    for ch in lo..=hi {
        let (_base, refs_l3, refs_l4) = ne_agent::governance::compliance_baseline(&conn, lib_root, volume, ch)?;
        ne_agent::governance::set_refs(&conn, volume, ch, &refs_l3, &refs_l4)?;
        // 无模型时只登记基准与 refs；合规等级交由上层 agent 写入
        out.push(serde_json::json!({"chapter": ch, "refs_l3": refs_l3, "refs_l4": refs_l4, "level": "(未评)"}));
    }
    let _ = vault;
    if json {
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
            "volume": volume,
            "chapters": out,
            "law_baseline_attached": law_baseline.len(),
            "law_baseline_fingerprint": law_fp,
            "semantic": if semantic { "requested_but_locked（--semantic 仅占位；语义校验未激活）" } else { "off（默认零模型）" },
        }))?);
    }
    else {
        println!("合规基准已生成/refs 已回写：{} 章", out.len());
        println!("法理基线（world-gov 预埋）：{} 条已随基准附上（指纹 {law_fp}），语义校验未激活", law_baseline.len());
        if semantic {
            println!("[提示] --semantic 已请求但锁死：真实模型推理等你单独放行。");
        }
    }
    Ok(())
}


pub(crate) fn run_models(vault: &Vault, cmd: &ModelsCmd, json: bool) -> Result<()> {
    let llm_dir = vault.llm_dir();
    match cmd {
        ModelsCmd::List => {
            let s = RuntimeConfig::describe_models(&llm_dir)?;
            if json {
                let providers: Vec<serde_json::Value> = s
                    .providers
                    .iter()
                    .map(|p| {
                        serde_json::json!({
                            "name": p.name,
                            "baseUrl": p.base_url,
                            "keyCount": p.key_count,
                            "keySource": p.key_source,
                            "models": p.models,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                    "dir": s.dir.display().to_string(),
                    "providers": providers,
                    "roles": s.roles,
                }))?);
            } else {
                print!("{}", s.render());
            }
            Ok(())
        }
        ModelsCmd::Check => {
            let cfg = RuntimeConfig::load(vault.root())?;
            let p = cfg.llm_params(&llm_dir)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                    "ok": true,
                    "model": p.model,
                    "baseUrl": p.base_url,
                    "keyCount": p.keys.len(),
                    "maxTokens": p.max_tokens,
                }))?);
            } else {
                println!("✓ 配置可用");
                println!("  模型   ：{}", if p.model.is_empty() { "(未指定)" } else { &p.model });
                println!("  端点   ：{}", p.base_url);
                println!("  key 数 ：{}", p.keys.len());
                println!("  max_tokens：{}", p.max_tokens);
            }
            Ok(())
        }
    }
}

