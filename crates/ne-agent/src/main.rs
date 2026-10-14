//! ne-agent CLI —— **唯一入口**。
//!
//! 设计（学太墟 cli 分层）：CLI **只做三件事**——
//!   1. 解析参数
//!   2. 初始化 Vault + Runtime 配置
//!   3. 把请求交给对应的 task/模块
//!
//! 业务逻辑一律不放这里。
//!
//! 当前（M0）：vault / models 两个子命令可用。
//! 后续：chat(M1) / memory(M2) / novel(M3) / tool(M4)。

use anyhow::{Context, Result};
use clap::Parser;

use crate::cli::{Cli, Cmd, LibraryCtlCmd, McpCmd, ModelsCmd, VaultCmd, WorldGovCmd};
use ne_agent::{RuntimeConfig, Vault};
use std::path::{Path, PathBuf};
mod cli;
mod cmds;

/// 法理链文本（与 ne_agent::world_gov::LAW_CHAIN 保持一致的展示用简写）。
const LAW_CHAIN_TEXT: &str = "L0 ≫ L0.5 ≫ L1 ≫ L1x ≫ ANCHOR ≫ GUARD ≫ L3 ≫ L4";

fn main() {
    if let Err(e) = run() {
        eprintln!("✗ {e:#}");
        std::process::exit(1);
    }
}

fn resolve_vault(cli: &Cli) -> Vault {
    if let Some(p) = &cli.vault {
        return Vault::open(p);
    }
    // 默认：当前目录下的 .neagent
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    Vault::open(cwd.join(".neagent"))
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let vault = resolve_vault(&cli);

    match &cli.cmd {
        Cmd::Vault { cmd } => run_vault(&vault, cmd, cli.json),
        Cmd::Models { cmd } => run_models(&vault, cmd, cli.json),
        Cmd::Chat { message, resume, session, new, plan, approval } => run_chat(
            &vault,
            message.as_deref(),
            *resume,
            session.as_deref(),
            *new,
            *plan,
            approval.as_deref(),
            cli.json,
            cli.work_root.clone(),
        ),
        Cmd::Session { cmd } => cmds::run_session(&vault, cmd, cli.json),
        Cmd::Context { window, reserve, auto_compact, auto_threshold, keep_recent, dry_run } => {
            cmds::run_context(
                &vault,
                *window,
                *reserve,
                *auto_compact,
                *auto_threshold,
                *keep_recent,
                *dry_run,
                cli.json,
            )
        }
        Cmd::Compress { id, mode, anchor, mechanical, show, clear } => cmds::run_compress(
            &vault,
            id.as_deref(),
            mode,
            anchor.as_deref(),
            *mechanical,
            *show,
            *clear,
            cli.json,
        ),
        Cmd::History { cmd } => cmds::run_history(&vault, cmd, cli.json),
        Cmd::Memory { cmd } => cmds::run_memory(&vault, cmd, cli.json),
        Cmd::Score { chap_id, volume, lib, file } => {
            run_score(&vault, *chap_id, volume, lib.as_deref(), file.as_deref())
        }
        Cmd::Fix { chap_id, volume, lib, findings, dry_run } => {
            run_fix(&vault, *chap_id, volume, lib.as_deref(), findings.as_deref(), *dry_run)
        }
        Cmd::Novel { chap_id, volume, lib, dry_run, prompt_only, use_memory, check_compliance } => {
            let r = run_novel(&vault, *chap_id, volume, lib.as_deref(), *dry_run, *prompt_only, *use_memory, cli.json);
            // 可选：落盘后独立做一次治理登记（默认关；只记录 refs/待评，不改正文）
            if *check_compliance && r.is_ok() {
                let lib_root = lib.clone().unwrap_or_else(|| vault.workspace());
                if let Ok(conn) = ne_agent::governance::open_gov(&lib_root) {
                    if let Ok((_b, r3, r4)) = ne_agent::governance::compliance_baseline(&conn, &lib_root, volume, *chap_id) {
                        let _ = ne_agent::governance::set_refs(&conn, volume, *chap_id, &r3, &r4);
                    }
                }
            }
            return r;
        }
        Cmd::AuditLayerHealth { semantic, lib } => {
            let lib_root = lib.clone().or_else(|| cli.work_root.clone()).unwrap_or_else(|| vault.workspace());
            run_layer_health(&lib_root, *semantic, cli.json)
        }
        Cmd::LibraryCtl { cmd } => {
            let lib_root = cli.work_root.clone().unwrap_or_else(|| vault.workspace());
            run_library_ctl(&lib_root, cmd, cli.json)
        }
        Cmd::WorldGov { cmd } => {
            let lib_root = cli.work_root.clone().unwrap_or_else(|| vault.workspace());
            run_world_gov(&vault, &lib_root, cmd, cli.json)
        }
        Cmd::AuditChapterCompliance { from, to, volume, lib, semantic } => {
            let lib_root = lib.clone().or_else(|| cli.work_root.clone()).unwrap_or_else(|| vault.workspace());
            run_compliance(&vault, &lib_root, *from, *to, volume, *semantic, cli.json)
        }
        Cmd::AuditRefs { volume, lib } => {
            let lib_root = lib.clone().or_else(|| cli.work_root.clone()).unwrap_or_else(|| vault.workspace());
            run_refs(&lib_root, volume, cli.json)
        }
        Cmd::AuditRiskList { all, lib } => {
            let lib_root = lib.clone().or_else(|| cli.work_root.clone()).unwrap_or_else(|| vault.workspace());
            run_risk_list(&lib_root, !*all, cli.json)
        }
        Cmd::LibCheckpoint { lib, out } => {
            let lib_root = lib.clone().or_else(|| cli.work_root.clone()).unwrap_or_else(|| vault.workspace());
            run_checkpoint(&lib_root, out.clone(), cli.json)
        }
        Cmd::AuditMaterialCoverage { from, to, volume, lib } => {
            let lib_root = lib.clone().or_else(|| cli.work_root.clone()).unwrap_or_else(|| vault.workspace());
            if let Some(to) = to {
                let rep = ne_agent::audit_material::audit_batch(&lib_root, volume, *from, *to)?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&rep)?);
                } else {
                    print_audit_batch(&rep);
                }
            } else {
                let rep = ne_agent::audit_material::audit_chapter(&lib_root, volume, *from)?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&rep)?);
                } else {
                    print_audit_one(&rep);
                }
            }
            Ok(())
        }
        Cmd::Mcp { cmd } => run_mcp(&vault, cmd, cli.json),
        Cmd::Task { cmd } => cmds::run_task(&vault, cmd, cli.json),
        Cmd::Subagent { file, session, max_tasks } => {
            cmds::run_subagent(&vault, file, session.as_deref(), *max_tasks, cli.json)
        }
        Cmd::Tools => {
            for t in ne_agent::tool::Tool::all() {
                if cli.json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "name": t.name(),
                            "tier": format!("{:?}", t.tier()),
                            "description": t.description(),
                            "params": t.params(),
                        })
                    );
                } else {
                    println!("{:<8} {:<6?} {}", t.name(), t.tier(), t.description());
                    println!("         参数：{}", t.params().join(", "));
                }
            }
            Ok(())
        }
        Cmd::Version => {
            let v = ne_agent::VERSION;
            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "name": env!("CARGO_PKG_NAME"),
                        "version": v,
                        "stage": ne_agent::STAGE,
                    })
                );
            } else {
                println!("{} {}（{}）", env!("CARGO_PKG_NAME"), v, ne_agent::STAGE);
            }
            Ok(())
        }
        Cmd::Estimate { file } => run_estimate(file.as_deref(), cli.json),
    }
}

/// 估算 token（上下文预算调试用）。
fn run_estimate(file: Option<&Path>, json: bool) -> Result<()> {
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

fn run_vault(vault: &Vault, cmd: &VaultCmd, json: bool) -> Result<()> {
    match cmd {
        VaultCmd::Init => {
            let created = vault.init()?;
            if json {
                let v: Vec<String> = created.iter().map(|p| p.display().to_string()).collect();
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                println!("vault 就绪：{}", vault.root().display());
                if created.is_empty() {
                    println!("  （已存在，未改动）");
                } else {
                    println!("  新建 {} 项：", created.len());
                    for p in &created {
                        println!("    + {}", p.strip_prefix(vault.root()).unwrap_or(p).display());
                    }
                }
                if !vault.path("models.yml").is_file() {
                    println!("提示：models.yml 为空，可跑 `ne-agent vault import-llm` 导入全局配置");
                }
            }
            Ok(())
        }
        VaultCmd::Show => {
            let ready = vault.is_initialized();
            let llm_dir = vault.llm_dir();
            if json {
                let obj = serde_json::json!({
                    "root": vault.root().display().to_string(),
                    "initialized": ready,
                    "llm_dir": llm_dir.display().to_string(),
                    "agent_db": vault.agent_db().display().to_string(),
                    "workspace": vault.workspace().display().to_string(),
                });
                println!("{}", serde_json::to_string_pretty(&obj)?);
            } else {
                println!("vault 根：{}", vault.root().display());
                println!("  状态   ：{}", if ready { "已初始化" } else { "未初始化（跑 `ne-agent vault init`）" });
                println!("  模型目录：{}", llm_dir.display());
                println!("  会话库 ：{}", vault.agent_db().display());
                println!("  工作区 ：{}", vault.workspace().display());
            }
            Ok(())
        }
        VaultCmd::ImportLlm => {
            let (created, src) = import_llm(vault)?;
            let src_placeholder = ne_agent::vault::is_placeholder_file(&src.join(ne_agent::vault::MODELS_YML));
            if json {
                let v: Vec<String> = created.iter().map(|p| p.display().to_string()).collect();
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                    "source": src.display().to_string(),
                    "sourceEmpty": src_placeholder,
                    "imported": v,
                }))?);
            } else {
                println!("已从 {} 导入：", src.display());
                if created.is_empty() {
                    println!("  （无新文件）");
                } else {
                    for p in &created {
                        println!("  + {}", p.strip_prefix(vault.root()).unwrap_or(p).display());
                    }
                }
                if src_placeholder {
                    eprintln!(
                        "⚠ 源配置是空的（{} 里 providers 为空）——\n  导入的仍是空壳。请填好 models.yml，或用 $TAIXU_LLM_DIR 指定配置目录。",
                        src.join(ne_agent::vault::MODELS_YML).display()
                    );
                }
            }
            Ok(())
        }
    }
}

/// 从全局 LLM 配置目录导入 models.yml / config.yml 到 vault。
///
/// **只导入一份副本**，之后 vault 自包含；不再依赖 `/root/.omp/agent`。
fn import_llm(vault: &Vault) -> Result<(Vec<PathBuf>, PathBuf)> {
    vault.init()?;
    let src = ne_engine::providers::resolve_llm_dir(None);
    let mut created = Vec::new();
    for name in [ne_agent::vault::MODELS_YML, ne_agent::vault::CONFIG_YML] {
        let from = src.join(name);
        if !from.is_file() {
            continue;
        }
        let to = vault.path(name);
        // 空壳（providers: {} / 仅注释）视为"未填充"，可以被导入覆盖
        if to.is_file() && !ne_agent::vault::is_placeholder_file(&to) {
            continue;
        }
        std::fs::copy(&from, &to)
            .with_context(|| format!("复制 {} -> {} 失败", from.display(), to.display()))?;
        created.push(to);
    }
    Ok((created, src))
}

/// 装配运行一次对话所需的全部运行时对象。
fn build_runtime(vault: &Vault) -> Result<RuntimeHandles> {
    vault.init()?;
    let cfg = RuntimeConfig::load(vault.root())?;
    let mut llm = cfg.llm_params(&vault.llm_dir())?;
    let enabled = ne_agent::resolve_enabled_tools(&cfg)?;
    // 工具定义注入 provider 原生 `tools` 通道（对齐太墟 NATIVE 模式）。
    // 不注入的话模型看不到工具，会直接回答"我没有文件读取工具"——真机验证暴露的头号缺陷。
    llm.tools = ne_agent::protocol::builtin_tool_definitions(&enabled);
    let store = ne_agent::session::SessionStore::open(vault.agent_db())?;
    let memory = ne_agent::memory::MemoryStore::open(vault.agent_db())?;
    Ok(RuntimeHandles { cfg, llm, enabled, store, memory })
}

struct RuntimeHandles {
    cfg: RuntimeConfig,
    llm: ne_engine::llm::LlmParams,
    enabled: Vec<ne_agent::tool::Tool>,
    store: ne_agent::session::SessionStore,
    memory: ne_agent::memory::MemoryStore,
}

/// 选定/新建会话。
fn pick_session(
    store: &ne_agent::session::SessionStore,
    explicit: Option<&str>,
    resume: bool,
    new: bool,
    auto_title: &str,
    // 本次是否带消息（带消息时默认续接，符合"连续对话"直觉）
    has_message: bool,
) -> Result<String> {
    if let Some(id) = explicit {
        if store.get_session(id)?.is_none() {
            anyhow::bail!("会话不存在：{id}");
        }
        return Ok(id.to_string());
    }
    if new {
        return store.create_session(auto_title);
    }
    // 续接条件：显式 --resume，**或**本次带消息（`chat -m` 默认接上一个会话，
    // 避免每问一句就散成新会话——那正是"松散"的根源）
    if resume || has_message {
        if let Some(s) = store.latest_session()? {
            return Ok(s.id);
        }
    }
    store.create_session(auto_title)
}

fn run_chat(
    vault: &Vault,
    message: Option<&str>,
    resume: bool,
    session: Option<&str>,
    new: bool,
    plan: bool,
    approval: Option<&str>,
    json: bool,
    work_root_override: Option<std::path::PathBuf>,
) -> Result<()> {
    let mut h = build_runtime(vault)?;
    // CLI 覆盖 runtime.yaml 的审批设置
    if plan {
        h.cfg.approval.plan_only = true;
    }
    if let Some(m) = approval {
        let parsed = ne_agent::approval::ApprovalMode::from_id(m);
        h.cfg.approval.mode = parsed.id().to_string();
    }
    let title = match message {
        Some(m) => format!("cli: {}", truncate(m, 30)),
        None => "交互对话".to_string(),
    };
    let sid = pick_session(&h.store, session, resume, new, &title, message.is_some())?;
    // 工作区沙箱：默认 vault.workspace()；--work-root 可覆盖（如指向资料库根，
    // 让 chat 任务能直接读写真实库而不用绕 workspace 镜像）。
    let work_root = work_root_override.unwrap_or_else(|| vault.workspace());
    if let Some(msg) = message {
        let (out, reasoning) = ne_agent::task::chat_once(
            &h.store,
            &h.memory,
            &sid,
            &h.cfg,
            h.llm,
            work_root,
            vault.path(ne_agent::vault::PROMPTS_DIR),
            h.enabled,
            msg,
            vault.root().to_path_buf(),  // vault 根（M8：load_rule/load_skill 用）
        )?;
        if json {
            println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                "session": sid, "reply": out, "reasoning": reasoning,
            }))?);
        } else {
            // 思考过程走 stderr，正文走 stdout（宿主可分别消费/合并）。
            // 显式标记包裹：思考文本含换行，不能靠前缀行判断边界。
            if !reasoning.trim().is_empty() {
                eprintln!("<<<NE_REASONING>>>{reasoning}<<<END_NE_REASONING>>>");
            }
            println!("{out}");
        }
        return Ok(());
    }

    ne_agent::task::chat_interactive(
        &h.store,
        &h.memory,
        &sid,
        &h.cfg,
        h.llm,
        work_root,
        vault.path(ne_agent::vault::PROMPTS_DIR),
        h.enabled,
        vault.root().to_path_buf(),
    )
}

fn truncate(s: &str, n: usize) -> String {
    let one_line = s.lines().next().unwrap_or("");
    if one_line.chars().count() > n {
        format!("{}…", one_line.chars().take(n).collect::<String>())
    } else {
        one_line.to_string()
    }
}

/// 管理外部 MCP 服务。
fn run_mcp(vault: &Vault, cmd: &McpCmd, json: bool) -> Result<()> {
    vault.init()?;
    let cfg = RuntimeConfig::load(vault.root())?;
    match cmd {
        McpCmd::List => {
            if cfg.mcp.servers.is_empty() {
                if json {
                    println!("[]");
                } else {
                    println!("（未配置 MCP 服务；在 runtime.yaml 的 mcp.servers 里添加）");
                }
                return Ok(());
            }
            if json {
                let v: Vec<serde_json::Value> = cfg
                    .mcp
                    .servers
                    .iter()
                    .map(|s| serde_json::json!({
                        "name": s.name, "command": s.command, "args": s.args, "enabled": s.enabled,
                    }))
                    .collect();
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                for s in &cfg.mcp.servers {
                    println!(
                        "  {:<14} {} {}  {}",
                        s.name,
                        s.command,
                        s.args.join(" "),
                        if s.enabled { "" } else { "（已禁用）" }
                    );
                }
            }
            Ok(())
        }
        McpCmd::Tools { name } => {
            let specs: Vec<ne_agent::config::McpServerCfg> = cfg
                .mcp
                .servers
                .iter()
                .filter(|s| s.enabled && name.as_ref().map(|n| &s.name == n).unwrap_or(true))
                .cloned()
                .collect();
            if specs.is_empty() {
                anyhow::bail!("没有匹配的（已启用）MCP 服务");
            }
            let mut reg = ne_agent::mcp::McpRegistry::new(specs);
            let tools = reg.discover_all();
            if json {
                let v: Vec<serde_json::Value> = tools
                    .iter()
                    .map(|(srv, t)| serde_json::json!({
                        "qualified": format!("mcp__{srv}__{}", t.name),
                        "server": srv, "name": t.name,
                        "description": t.description, "inputSchema": t.input_schema,
                    }))
                    .collect();
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else if tools.is_empty() {
                println!("（服务未提供任何工具，或全部连接失败）");
            } else {
                for (srv, t) in tools {
                    println!("  mcp__{srv}__{}  {}", t.name, t.description);
                }
            }
            Ok(())
        }
        McpCmd::Call { server, tool, args_json } => {
            let spec = cfg
                .mcp
                .servers
                .iter()
                .find(|s| s.name == *server && s.enabled)
                .ok_or_else(|| anyhow::anyhow!("未配置（或已禁用）的 MCP 服务：{server}"))?
                .clone();
            let args: serde_json::Value = serde_json::from_str(args_json)
                .map_err(|e| anyhow::anyhow!("args_json 不是合法 JSON：{e}"))?;
            let mut reg = ne_agent::mcp::McpRegistry::new(vec![spec]);
            let out = reg.call(server, tool, args)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({"result": out}))?);
            } else {
                println!("{out}");
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
fn run_score(
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
    let out = ne_engine::llm::call(&llm, &msgs)?;
    let rep = lib_root.join("07_勘误工坊").join(format!("质检_卷{volume}_第{chap_id}章.md"));
    std::fs::create_dir_all(rep.parent().unwrap())?;
    std::fs::write(&rep, &out.content)?;
    println!("质检报告 → {}", rep.display());
    println!("{}", out.content.chars().take(600).collect::<String>());
    Ok(())
}

/// 按 l5_修复.md 定向修复（读质检 findings + 原文）。
fn run_fix(
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
    let out = ne_engine::llm::call(&llm, &msgs)?;
    if dry_run {
        println!("{}", out.content);
    } else {
        let path = ne_engine::chapter::write_chapter(&lib_root, &out.content, chap_id, volume)?;
        println!("修复后正文 → {path}");
    }
    Ok(())
}

fn run_novel(
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
    let out = ne_agent::task::novel_chapter_with_memory(
        &lib_root,
        volume,
        chap_id,
        &llm,
        &vault.workspace(),
        &prompt_dir,
        dry_run,
        use_memory,
    )?;

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

fn run_models(vault: &Vault, cmd: &ModelsCmd, json: bool) -> Result<()> {
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

/// 人读输出：单章。
fn print_audit_one(a: &ne_agent::audit_material::ChapterAudit) {
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
fn print_audit_batch(rep: &serde_json::Value) {
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

fn run_layer_health(lib_root: &Path, semantic: bool, json: bool) -> Result<()> {
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

fn run_library_ctl(lib_root: &Path, cmd: &LibraryCtlCmd, json: bool) -> Result<()> {
    let conn = ne_agent::governance::open_gov(lib_root)?;
    let out = match cmd {
        LibraryCtlCmd::Init => ne_agent::governance::library_init(&conn, lib_root)?,
        LibraryCtlCmd::Classify { out } => {
            let dir = out.clone().unwrap_or_else(|| lib_root.join("07_勘误工坊/export"));
            ne_agent::governance::library_classify(&conn, Some(&dir))?
        }
        LibraryCtlCmd::Audit { semantic: _ } => ne_agent::governance::library_audit(&conn, lib_root)?,
        LibraryCtlCmd::Export { out, layer } => {
            let dir = out.clone().unwrap_or_else(|| lib_root.join("07_勘误工坊/export"));
            ne_agent::governance::library_export_layer(&conn, lib_root, &dir, layer.as_deref())?
        }
        LibraryCtlCmd::Reindex { batch: _ } => ne_agent::governance::library_reindex(lib_root)?,
        LibraryCtlCmd::Stats => ne_agent::governance::library_stats(&conn)?,
    };
    if json { println!("{}", serde_json::to_string_pretty(&out)?); }
    else { println!("{}", serde_json::to_string_pretty(&out)?); }
    Ok(())
}

/// world-gov —— 世界观法务官（独立外挂；默认零模型只产探针包；--semantic 才调模型）。
fn run_world_gov(vault: &Vault, lib_root: &Path, cmd: &WorldGovCmd, json: bool) -> Result<()> {
    use ne_agent::world_gov;
    let conn = ne_agent::governance::open_gov(lib_root)?;
    match cmd {
        WorldGovCmd::Bootstrap { dry_run, lib: _ } => {
            let r = world_gov::bootstrap(&conn, lib_root, *dry_run)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                println!("== 世界观法理基线（{} 条上层条目）==", r["entry_count"]);
                println!("基线指纹: {}", r["baseline_fingerprint"]);
                println!("法理链: {}", LAW_CHAIN_TEXT);
                if let Some(entries) = r["entries"].as_array() {
                    for e in entries {
                        println!("\n[{:5}] {}（{} 字）{}", e["layer"], e["key"], e["chars"], e["fingerprint"]);
                        println!("  摘要: {}", e["summary"].as_str().unwrap_or(""));
                        if let Some(bs) = e["boundaries"].as_array() {
                            if !bs.is_empty() {
                                println!("  边界/禁止域线索 {} 条：", bs.len());
                                for b in bs.iter().take(6) {
                                    println!("    · {}", b.as_str().unwrap_or(""));
                                }
                            }
                        }
                    }
                }
                if *dry_run {
                    println!("\n[--dry-run] 未写 world_baseline.json");
                } else {
                    println!("\n已写入: {}/07_勘误工坊/world_baseline.json", lib_root.display());
                }
            }
        }
        WorldGovCmd::InspectCard { key, semantic, dry_run, lib: _ } => {
            let r = world_gov::inspect_card(&conn, lib_root, key)?;
            if r["status"] == "NOT_FOUND" {
                if json { println!("{}", serde_json::to_string_pretty(&r)?); }
                else { println!("未找到 key={key}（不做任何判定）"); }
                return Ok(());
            }
            if !*semantic {
                if json {
                    println!("{}", serde_json::to_string_pretty(&r)?);
                } else {
                    let p = &r["probe_pack"];
                    println!("== 探针包 probe_pack（零模型；交上层 agent 或 --semantic 裁决）==");
                    println!("key={} layer={} chars={} codes={}",
                        p["key"], p["layer"], p["card_chars"], p["card_codes"]);
                    println!("候选上层法理 {} 条（按优先级降序）：",
                        p["candidate_laws"].as_array().map(|a| a.len()).unwrap_or(0));
                    if let Some(laws) = p["candidate_laws"].as_array() {
                        for l in laws {
                            println!("  [{:5}] {}  overlap={:?}  依据: {}",
                                l["layer"], l["key"], l["char_overlap"],
                                l["basis_snippet"].as_str().unwrap_or("").chars().take(60).collect::<String>());
                        }
                    }
                    if let Some(qs) = p["questions"].as_array() {
                        println!("待裁决问题：");
                        for q in qs { println!("  ? {}", q.as_str().unwrap_or("")); }
                    }
                }
                return Ok(());
            }
            // --semantic：显式开关才走到这里
            if *dry_run {
                let (sys, usr) = world_gov::semantic_messages(&r["probe_pack"]);
                if json {
                    println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                        "dry_run": true, "system": sys, "user_chars": usr.chars().count(),
                    }))?);
                } else {
                    println!("[--dry-run] 只打印将发送的 prompt，不调模型、不写库：");
                    println!("── system ──\n{sys}\n── user（{} 字）──\n{}", usr.chars().count(),
                        usr.chars().take(500).collect::<String>());
                }
                return Ok(());
            }
            let (sys, usr) = world_gov::semantic_messages(&r["probe_pack"]);
            let llm = RuntimeConfig::load(vault.root())?.llm_params(&vault.llm_dir())?;
            let msgs = vec![
                ne_engine::llm::Message::system(sys),
                ne_engine::llm::Message::user(usr),
            ];
            let out = ne_engine::llm::call(&llm, &msgs)?;
            let verdict = world_gov::parse_semantic_verdict(key, &out.content);
            // 语义风险入库（is_semantic=1）；UNCERTAIN 不入风险库
            let v = verdict["verdict"].as_str().unwrap_or("UNCERTAIN");
            if v == "HIGH-RISK" || v == "WARN" {
                let law = verdict["findings"][0]["law_ref"].as_str().unwrap_or("");
                ne_agent::governance::record_risk_semantic(&conn, "world_gov",
                    None, None, "world-gov inspect-card", v,
                    &format!("语义检视 {key}: {v}（依据: {law}）"), law)?;
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                    "command": "world-gov inspect-card", "key": key, "semantic": true,
                    "verdict": verdict,
                }))?);
            } else {
                println!("== 语义检视结论 key={key} verdict={v} ==");
                println!("{}", serde_json::to_string_pretty(&verdict)?);
            }
        }
        WorldGovCmd::Trace { key, dry_run: _, lib: _ } => {
            let r = world_gov::trace(&conn, lib_root, key)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else if r["status"] == "NOT_FOUND" {
                println!("未找到 key={key}");
            } else {
                println!("== 法理溯源链 ==");
                println!("{}", r["visual"].as_str().unwrap_or(""));
                println!("（共 {} 层上层约束；详见 --json）", r["chain_len"]);
            }
        }
        WorldGovCmd::ScanLibrary { semantic, slice: _, dry_run: _, lib: _ } => {
            let _ = semantic;
            anyhow::bail!("scan-library 属第 2 批交付（重算力语义扫描），本轮未实现；第 1 批已交付 bootstrap/inspect-card/trace。");
        }
        WorldGovCmd::CheckOutline { volume: _, semantic: _, dry_run: _, lib: _ } => {
            anyhow::bail!("check-outline 属第 2 批交付（卷细纲法理评估），本轮未实现。");
        }
    }
    Ok(())
}

fn run_compliance(vault: &Vault, lib_root: &Path, from: u32, to: Option<u32>, volume: &str, _semantic: bool, json: bool) -> Result<()> {
    let conn = ne_agent::governance::open_gov(lib_root)?;
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
    if json { println!("{}", serde_json::to_string_pretty(&out)?); }
    else { println!("合规基准已生成/refs 已回写：{} 章", out.len()); }
    Ok(())
}

fn run_refs(lib_root: &Path, volume: &str, json: bool) -> Result<()> {
    let conn = ne_agent::governance::open_gov(lib_root)?;
    let g = ne_agent::governance::refs_graph(&conn, lib_root, volume)?;
    if json { println!("{}", serde_json::to_string_pretty(&g)?); }
    else { println!("{}", serde_json::to_string_pretty(&g)?); }
    Ok(())
}

fn run_risk_list(lib_root: &Path, unresolved_only: bool, json: bool) -> Result<()> {
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

fn run_checkpoint(lib_root: &Path, out: Option<PathBuf>, json: bool) -> Result<()> {
    let conn = ne_agent::governance::open_gov(lib_root)?;
    let dir = out.unwrap_or_else(|| lib_root.join("09_冷备份"));
    let r = ne_agent::governance::lib_checkpoint(&conn, lib_root, &dir)?;
    if json { println!("{}", serde_json::to_string_pretty(&r)?); }
    else { println!("里程碑包：{}", r["dir"]); }
    Ok(())
}
