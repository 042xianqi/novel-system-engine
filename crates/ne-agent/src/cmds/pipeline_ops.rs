//! 小说管线 / 质检 / 修复 子命令 —— 第二批平移（架构门禁：单文件 ≤ 700 行）。
//!
//! # 成员
//! - `run_novel`：章节生成主流程
//! - `run_pipeline`：管线视图（六层漏斗）
//! - `run_score` / `run_fix`：评分与修复
//! - `print_audit_one` / `print_audit_batch`：审计结果打印（`run()` 分发调用）
//!
//! 同样经静态扫描确认：唯一调用者是 `run()`，无 main.rs 私有助手耦合。

use anyhow::{Context, Result};
use ne_agent::governance::governed_call::ne_agent_gov_call;
use ne_agent::{RuntimeConfig, Vault};
use std::path::Path;

use crate::cli::PipelineCmd;

pub(crate) fn run_novel(
    vault: &Vault,
    chap_id: u32,
    volume: &str,
    lib: Option<&Path>,
    dry_run: bool,
    prompt_only: bool,
    use_memory: bool,
    json: bool,
) -> Result<()> {
    vault.init()?;
    let lib_root = match lib {
        Some(p) => p.to_path_buf(),
        None => vault.workspace(),
    };
    if !lib_root.exists() {
        anyhow::bail!(
            "库根不存在：{}\n提示：用 `--lib <资料库根>` 指定，或把它放到 vault 的 workspace/ 下",
            lib_root.display()
        );
    }

    let cfg = RuntimeConfig::load(vault.root())?;
    let prompt_dir = vault.path(ne_agent::vault::PROMPTS_DIR);

    if prompt_only {
        // 只组装 prompt，不调模型 → 不需要模型配置
        let ctx = ne_agent::novel_ctx::NovelCtx::open(&lib_root)?
            .with_prompt_fallback(&prompt_dir);
        let prompt = ne_engine::chapter::build_prompt(&ctx, chap_id, volume)
            .map_err(|e| anyhow::anyhow!("组装第 {chap_id} 章 prompt 失败（卷 {volume}）：{e}"))?;
        if json {
            println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                "chapter": chap_id, "volume": volume,
                "promptChars": prompt.chars().count(),
                "prompt": prompt,
            }))?);
        } else {
            println!("第 {chap_id} 章（{volume}）prompt 已组装：{} 字", prompt.chars().count());
            println!("── 前 800 字 ──");
            println!("{}", prompt.chars().take(800).collect::<String>());
        }
        return Ok(());
    }

    let llm = cfg.llm_params(&vault.llm_dir())?;
    // 【取消作用域】把整章生成包进任务作用域：期间 governed_call 的取消检查点
    // 就能感知取消（scope id 用 `cli-novel-{卷}-{章}`，可由同一进程内其他线程
    // 通过 cancel_token::fire 触发中断）。无作用域时检查点恒为 no-op——这正是
    // 原实现"令牌是孤岛"的根因。
    let scope_id = format!("cli-novel-{volume}-{chap_id}");
    let out = ne_agent::governance::cancel_token::with_task_scope(&scope_id, || {
        ne_agent::task::novel_chapter_with_memory(
            &lib_root,
            volume,
            chap_id,
            &llm,
            &vault.workspace(),
            &prompt_dir,
            dry_run,
            use_memory,
        )
    })?;

    if json {
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
            "chapter": out.chapter, "volume": out.volume,
            "chars": out.text.chars().count(),
            "writtenTo": out.written_to,
            "verdict": out.verdict, "p0": out.p0, "p1": out.p1,
            "notes": out.notes,
        }))?);
    } else {
        print!("{}", out.render());
    }
    Ok(())
}


/// 【规约适配】pipeline 命令组（scan-auto / world-analyze）。
///
/// 第 18 批：对齐《项目总规约》的命令面。全部是**适配层**——底层复用 world-gov 的
/// `scan_library` 与统一 `semantic_pipeline` 收口，不另起一套。默认零外呼；
/// `--ai-run` 才显式开命令行锁（仍需 feature + WOGOV_ALLOW_REAL_LLM=yes）。
pub(crate) fn run_pipeline(vault: &Vault, lib_root: &Path, cmd: &PipelineCmd, json: bool, quiet: bool) -> Result<()> {
    let _ = vault;
    match cmd {
        PipelineCmd::ScanAuto { slice, ai_guided, ai_run, dry_run, all_layers, lib: _ } => {
            let conn = ne_agent::governance::open_gov(lib_root)?;
            ne_agent::governance::progress::emit(quiet, "pipeline scan-auto", 0, 1, None);
            // 复用 world-gov scan-library 同一实现；semantic = --ai-run（显式）
            let r = ne_agent::world_gov::scan_library(
                &conn, lib_root, slice.as_deref(), *ai_run, *dry_run, *all_layers, None, None, None,
            )?;
            ne_agent::governance::progress::emit(quiet, "pipeline scan-auto", 1, 1, Some(0));
            if json {
                let mut out = r.clone();
                out["command"] = serde_json::json!("pipeline scan-auto");
                out["ai_guided"] = serde_json::json!(ai_guided);
                out["ai_run"] = serde_json::json!(ai_run);
                out["llm_gate"] = ne_agent::governance::llm_gate::lock_status(*ai_run);
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                let s = &r["slice"];
                println!("== pipeline scan-auto（AI 引导扫描包装）==");
                println!("分片 {}/{}：本片 {} 条 / 扫描域 {} 条", s["n"], s["m"], s["items"], s["total"]);
                println!("semantic={} ai_guided={} ai_run={} dry_run={}", r["semantic"], ai_guided, ai_run, dry_run);
                if !*ai_run {
                    println!("[默认零外呼] 只产探针清单；加 --ai-run 才走真实语义扫描（仍需三重锁）。");
                } else {
                    let st = ne_agent::governance::llm_gate::lock_status(true);
                    if st["gate_unlocked"] == true {
                        println!("[三重锁已开] 真实语义扫描通路已启用。");
                    } else {
                        println!("[三重锁未全开] {}", st["status"].as_str().unwrap_or(""));
                    }
                }
                if *dry_run { println!("[--dry-run] 未落盘清单文件。"); }
            }
            Ok(())
        }
        PipelineCmd::WorldAnalyze { slice, ai_run, dry_run, lib: _ } => {
            let conn = ne_agent::governance::open_gov(lib_root)?;
            let r = ne_agent::world_gov::scan_library(
                &conn, lib_root, slice.as_deref(), *ai_run, *dry_run, false, None, None, None,
            )?;
            if json {
                let mut out = r.clone();
                out["command"] = serde_json::json!("pipeline world-analyze");
                out["ai_run"] = serde_json::json!(ai_run);
                out["llm_gate"] = ne_agent::governance::llm_gate::lock_status(*ai_run);
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                println!("== pipeline world-analyze（= scan-library --semantic 别名）==");
                println!("扫描域 {} 条；semantic={} ai_run={}", r["counts"]["used"], r["semantic"], ai_run);
                println!("入库通路: {}", r["semantic_channel"]);
                if !*ai_run {
                    println!("[默认零外呼] 结构化报告骨架已产；加 --ai-run 走真实语义（仍需三重锁）。");
                }
            }
            Ok(())
        }
    }
}


/// 生成一章。
///
/// 库根解析顺序：显式 `--lib` / `$NE_AGENT_LIB` → vault 的 `workspace/`
/// （vault 里放一份库，或软链指向真实资料库）。
/// 按 l5_质检.md 对正文做 LLM 深读（输出质检报告）。
pub(crate) fn run_score(
    vault: &Vault,
    chap_id: u32,
    volume: &str,
    lib: Option<&Path>,
    file: Option<&Path>,
) -> Result<()> {
    let lib_root = lib.map(|p| p.to_path_buf()).unwrap_or_else(|| vault.workspace());
    let prompt_dir = vault.path(ne_agent::vault::PROMPTS_DIR);
    let ctx = ne_agent::novel_ctx::NovelCtx::open(&lib_root)?.with_prompt_fallback(&prompt_dir);
    // 读正文
    let text_path = file
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| lib_root.join("04_正文").join(volume).join(format!("第{chap_id}章.md")));
    let text = std::fs::read_to_string(&text_path)
        .with_context(|| format!("读不到正文：{}", text_path.display()))?;
    let tpl = ctx.prompt_template("l5_质检.md", Some(&prompt_dir)).unwrap_or_default();
    let outline = ne_engine::chapter::ChapterCtx::injection(&ctx, volume, chap_id)
        .ok()
        .and_then(|v| v.get("chapter").and_then(|c| c.get("outline")).and_then(|o| o.as_str()).map(|s| s.to_string()))
        .unwrap_or_default();
    let prompt = format!("{tpl}\n\n── 待质检正文（第{chap_id}章）──\n{text}\n\n── 本章细纲 ──\n{outline}\n");
    let llm = RuntimeConfig::load(vault.root())?.llm_params(&vault.llm_dir())?;
    let msgs = vec![
        ne_engine::llm::Message::system("你是《残响之河》的质检员。按模板逐维扫描，输出 findings（P0/P1）与总评，不要改写正文。"),
        ne_engine::llm::Message::user(prompt),
    ];
    // 【取消作用域】质检也是一次真实外呼，同样纳入任务作用域。
    let out = ne_agent::governance::cancel_token::with_task_scope(
        &format!("cli-score-{volume}-{chap_id}"),
        || ne_agent_gov_call(&llm, &msgs),
    )?;
    let rep = lib_root.join("07_勘误工坊").join(format!("质检_卷{volume}_第{chap_id}章.md"));
    // 【决策二·E级修复】rep 为根路径（如 "/"）时 parent() 返回 None，原 unwrap 会 panic。
    let Some(dir) = rep.parent() else {
        anyhow::bail!("导出路径 {rep:?} 没有父目录，无法创建");
    };
    std::fs::create_dir_all(dir)?;
    std::fs::write(&rep, &out.content)?;
    println!("质检报告 → {}", rep.display());
    println!("{}", out.content.chars().take(600).collect::<String>());
    Ok(())
}


/// 按 l5_修复.md 定向修复（读质检 findings + 原文）。
pub(crate) fn run_fix(
    vault: &Vault,
    chap_id: u32,
    volume: &str,
    lib: Option<&Path>,
    findings: Option<&Path>,
    dry_run: bool,
) -> Result<()> {
    let lib_root = lib.map(|p| p.to_path_buf()).unwrap_or_else(|| vault.workspace());
    let prompt_dir = vault.path(ne_agent::vault::PROMPTS_DIR);
    let ctx = ne_agent::novel_ctx::NovelCtx::open(&lib_root)?.with_prompt_fallback(&prompt_dir);
    let text_path = lib_root.join("04_正文").join(volume).join(format!("第{chap_id}章.md"));
    let text = std::fs::read_to_string(&text_path)
        .with_context(|| format!("读不到正文：{}", text_path.display()))?;
    let fnd = match findings {
        Some(p) => std::fs::read_to_string(p).unwrap_or_default(),
        None => std::fs::read_to_string(lib_root.join("07_勘误工坊").join(format!("质检_卷{volume}_第{chap_id}章.md"))).unwrap_or_default(),
    };
    let tpl = ctx.prompt_template("l5_修复.md", Some(&prompt_dir)).unwrap_or_default();
    let prompt = format!("{tpl}\n\n── 质检 findings ──\n{fnd}\n\n── 原正文 ──\n{text}\n");
    let llm = RuntimeConfig::load(vault.root())?.llm_params(&vault.llm_dir())?;
    let msgs = vec![
        ne_engine::llm::Message::system("你是《残响之河》的修复者。最小改动，只修命中的维度，输出修复后正文全文。"),
        ne_engine::llm::Message::user(prompt),
    ];
    let out = ne_agent_gov_call(&llm, &msgs)?;
    if dry_run {
        println!("{}", out.content);
    } else {
        let path = ne_engine::chapter::write_chapter(&lib_root, &out.content, chap_id, volume)?;
        println!("修复后正文 → {path}");
    }
    Ok(())
}


/// 人读输出：单章。
pub(crate) fn print_audit_one(a: &ne_agent::audit_material::ChapterAudit) {
    use ne_agent::audit_material::Status;
    println!("== 审计 {} 第{}章 ==", a.volume, a.chapter);
    println!("细纲切出: {}  引用卡 {} → 选入 {}", a.outline_found, a.refs_total, a.refs_selected);
    println!("prompt 合计 {} 字；记忆注入 {} token", a.prompt_chars, a.mem.inject_tokens);
    println!("短期 {} 条 / 快照 {:?} / char_state {} 行 / 锚点 {} 条", a.mem.short_term_rows, a.mem.snapshot_id, a.mem.char_state_rows, a.mem.anchor_rows);
    println!("压缩细纲 {} 字（原 {} 字）", a.mem.compressed_outline_chars, a.mem.raw_outline_chars);
    println!("--");
    for c in &a.checks {
        let tag = match c.status { Status::Hit => "HIT   ", Status::Empty => "EMPTY ", Status::NotQueried => "NOQUERY" };
        println!("[{tag}] {} :: {}", c.item, c.evidence);
    }
}


/// 人读输出：批量汇总表。
pub(crate) fn print_audit_batch(rep: &serde_json::Value) {
    println!("== 批量审计 {} 第{}-{}章（共 {} 章）==", rep["volume"], rep["from"], rep["to"], rep["chapters"]);
    println!("{:<52} {:>5} {:>6} {:>9}  {}", "项", "HIT", "EMPTY", "NOQUERY", "判定");
    if let Some(sum) = rep["summary"].as_array() {
        for s in sum {
            println!(
                "{:<52} {:>5} {:>6} {:>9}  {}",
                s["item"].as_str().unwrap_or("").chars().take(50).collect::<String>(),
                s["hit"], s["empty"], s["not_queried"], s["verdict"].as_str().unwrap_or("")
            );
        }
    }
}


// ─────────────────────────── 治理工具实现（独立外挂，默认只读/默认关）

