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

use crate::cli::{Cli, Cmd, LibraryCtlCmd, McpCmd, ModelsCmd, QueueCmd, VaultCmd, WorldGovCmd, AuditExportCmd, CacheCmd, ProbeExportCmd, SelfCheckCmd, TagCmd, NoteCmd, DiffCmd, DiffSnapCmd, WebCmd, ConflictCmd, GraphCmd, SandboxCmd, SandboxTraceCmd, LlmCmd, LlmCacheCmd, LlmConfigCmd, PipelineCmd};
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
    // 初始化结构化日志（默认 plain；structured 时 stderr 每行 JSON，不污染 stdout）
    {
        let mode = ne_agent::governance::structured_log::LogMode::parse(&cli.log_mode)?;
        ne_agent::governance::structured_log::init(mode, cli.quiet, cli.log_file.clone());
    }
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
            run_world_gov(&vault, &lib_root, cmd, cli.json, cli.quiet)
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
        Cmd::SysCheck { mode, lib } => {
            let lib_root = lib.clone().or_else(|| cli.work_root.clone()).unwrap_or_else(|| vault.workspace());
            run_sys_check(&lib_root, mode, cli.json)
        }
        Cmd::Pipeline { cmd } => {
            let lib_root = cli.work_root.clone().unwrap_or_else(|| vault.workspace());
            run_pipeline(&vault, &lib_root, cmd, cli.json, cli.quiet)
        }
        // 【规约适配】顶层别名：构造等价的 WorldGovCmd 并委托 run_world_gov（完全复用底层，不另起逻辑）
        Cmd::Snap { cmd, lib } => {
            let lib_root = lib.clone().or_else(|| cli.work_root.clone()).unwrap_or_else(|| vault.workspace());
            let wg = WorldGovCmd::Diff { cmd: DiffCmd::Snap { cmd: cmd.clone() }, lib: None };
            run_world_gov(&vault, &lib_root, &wg, cli.json, cli.quiet)
        }
        Cmd::Web { cmd, lib } => {
            let lib_root = lib.clone().or_else(|| cli.work_root.clone()).unwrap_or_else(|| vault.workspace());
            let wg = WorldGovCmd::Web { cmd: cmd.clone(), lib: None };
            run_world_gov(&vault, &lib_root, &wg, cli.json, cli.quiet)
        }
        Cmd::Sandbox { cmd, lib } => {
            let lib_root = lib.clone().or_else(|| cli.work_root.clone()).unwrap_or_else(|| vault.workspace());
            let wg = WorldGovCmd::Sandbox { cmd: cmd.clone(), lib: None };
            run_world_gov(&vault, &lib_root, &wg, cli.json, cli.quiet)
        }
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
        McpCmd::ProbeAll { call_test } => {
            let specs: Vec<ne_agent::config::McpServerCfg> = cfg
                .mcp
                .servers
                .iter()
                .filter(|s| s.enabled)
                .cloned()
                .collect();
            if specs.is_empty() {
                if json {
                    println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                        "command": "mcp probe-all", "servers": [], "note": "未配置（或全部禁用）MCP 服务",
                    }))?);
                } else {
                    println!("（未配置 MCP 服务；在 runtime.yaml 的 mcp.servers 里添加）");
                }
                return Ok(());
            }
            let mut reports: Vec<serde_json::Value> = Vec::new();
            for spec in &specs {
                // 逐服务独立连接，一个失败不影响其它（可用性探测的语义）
                let mut reg = ne_agent::mcp::McpRegistry::new(vec![spec.clone()]);
                let tools = reg.discover_all();
                let server_tools: Vec<serde_json::Value> = tools
                    .iter()
                    .filter(|(srv, _)| srv == &spec.name)
                    .map(|(_, t)| serde_json::json!({"name": t.name, "description": t.description}))
                    .collect();
                let ok = !server_tools.is_empty();
                let mut calls: Vec<serde_json::Value> = Vec::new();
                if *call_test && ok {
                    for (_, t) in tools.iter().filter(|(srv, _)| srv == &spec.name) {
                        // 轻量探活：用空参数调用；执行期失败也算"可达"（关键在握手/往返通）
                        match reg.call(&spec.name, &t.name, serde_json::json!({})) {
                            Ok(_) => calls.push(serde_json::json!({"tool": t.name, "reachable": true})),
                            Err(e) => calls.push(serde_json::json!({"tool": t.name, "reachable": false, "error": e.to_string()})),
                        }
                    }
                }
                reports.push(serde_json::json!({
                    "server": spec.name, "command": spec.command, "args": spec.args,
                    "ok": ok, "tool_count": server_tools.len(), "tools": server_tools,
                    "call_test": if *call_test { serde_json::json!(calls) } else { serde_json::Value::Null },
                }));
            }
            let all_ok = reports.iter().all(|r| r["ok"] == true);
            if json {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                    "command": "mcp probe-all", "all_ok": all_ok,
                    "server_count": reports.len(), "servers": reports,
                }))?);
            } else {
                println!("== mcp probe-all（{} 个服务；{}）==", reports.len(),
                    if all_ok { "全部可达" } else { "存在不可达" });
                for r in &reports {
                    println!("  {} {} 工具 {} — {}",
                        if r["ok"] == true { "✅" } else { "❌" },
                        r["server"].as_str().unwrap_or(""),
                        r["tool_count"],
                        if r["ok"] == true { "握手 + 列工具成功" } else { "连接失败或无工具" });
                    if *call_test {
                        if let Some(cs) = r["call_test"].as_array() {
                            for c in cs {
                                println!("      {} {} {}", if c["reachable"] == true { "·" } else { "✗" },
                                    c["tool"].as_str().unwrap_or(""),
                                    c["error"].as_str().unwrap_or(""));
                            }
                        }
                    }
                }
            }
            Ok(())
        }
    }
}

/// 【规约适配】全局系统自检（= `world-gov self-check run` 的别名入口）。
///
/// 第 18 批：对齐《项目总规约》的 `sys-check full --json`。**复用**同一实现。
fn run_sys_check(lib_root: &Path, mode: &str, json: bool) -> Result<()> {
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

/// 【规约适配】pipeline 命令组（scan-auto / world-analyze）。
///
/// 第 18 批：对齐《项目总规约》的命令面。全部是**适配层**——底层复用 world-gov 的
/// `scan_library` 与统一 `semantic_pipeline` 收口，不另起一套。默认零外呼；
/// `--ai-run` 才显式开命令行锁（仍需 feature + WOGOV_ALLOW_REAL_LLM=yes）。
fn run_pipeline(vault: &Vault, lib_root: &Path, cmd: &PipelineCmd, json: bool, quiet: bool) -> Result<()> {
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
/// `--enqueue` 统一入口：
/// - 非 --persist：纯内存，同调用内走通 enqueue→simulate→emit（进程退出即消失）。
/// - --persist：只登记 **Pending** 到 novel.db.task_jobs（不模拟、不入库），
///   推进交由显式 `world-gov queue step`（一步一推，绝不自动轮询）。
fn gov_enqueue(
    conn: &rusqlite::Connection,
    kind: ne_agent::governance::pipeline::JobKind,
    slice: Option<(u32, u32)>,
    volume: Option<String>,
    probe_path: String,
    key: Option<&str>,
    persist: bool,
    json: bool,
    label: &str,
) -> Result<()> {
    use ne_agent::governance::pipeline as pipe;
    use ne_agent::governance::pipeline_store as store;

    if persist {
        store::ensure_task_table(conn)?;
        // **跨进程幂等**：id 由参数派生（不含时间戳），同参数重复提交 → 同 id → 不灌水。
        let id = pipe::derived_id(kind, slice, volume.as_deref(), key);
        let fresh = !store::job_exists(conn, &id)?;
        if fresh {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let job = pipe::SemanticJob {
                id: id.clone(),
                kind,
                slice,
                volume: volume.clone(),
                probe_path: probe_path.clone(),
                created: now,
                state: pipe::JobState::Pending,
            };
            store::job_persist(conn, &job, None)?;
        }
        if json {
            println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                "command": format!("world-gov {label} --enqueue"), "mode": "persist(pending)",
                "job_id": id, "fresh": fresh, "state": "Pending",
                "note": if fresh {
                    "已落库，待 `world-gov queue step` 逐步推进（不自动轮询）".to_string()
                } else {
                    "同参数任务已存在（跨进程幂等，未重复写入）；状态保持不变".to_string()
                },
            }))?);
        } else {
            println!("== 持久入队（{label}）==");
            println!("任务 {} （{}）", id,
                if fresh { "已登记为 Pending" } else { "同参数已存在 → 跨进程幂等跳过，未重复写入" });
            if fresh { println!("下一步：world-gov queue step（一步一推；非自动轮询）"); }
        }
        return Ok(());
    }

    // 非持久：纯内存完整沙盘
    let (id, fresh, findings) = pipe::enqueue_and_simulate(kind, slice, volume)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let _ = pipe::set_probe_path(&id, &probe_path);
    let n_risks = if fresh {
        world_gov_emit(conn, label, &findings)?
    } else { 0 };
    if json {
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
            "command": format!("world-gov {label} --enqueue"), "mode": "pipeline_sim",
            "job_id": id, "fresh": fresh, "persisted": false,
            "findings_count": findings.len(), "risks_emitted": n_risks,
        }))?);
    } else {
        println!("== 管线沙盘（{label}）==");
        println!("任务 {} {} → 模拟 findings {} 条 → 入库 {} 条（纯内存，退出即消失）",
            id, if fresh { "入队(新建)" } else { "重复入队(幂等跳过)" }, findings.len(), n_risks);
        println!("（沙盘只产 OK/UNCERTAIN 样例，均不入库；真实推理等放行令）");
    }
    Ok(())
}

/// emit 薄封装（统一 source 前缀）。
fn world_gov_emit(
    conn: &rusqlite::Connection,
    label: &str,
    findings: &[serde_json::Value],
) -> Result<usize> {
    ne_agent::world_gov::emit_semantic_findings(conn, &format!("world-gov {label} --enqueue"), findings)
}
/// 探针包内容指纹（第 13 批）：稳定 40-hex，用于冲突库审计锚点。纯函数。
fn wg_probe_hash(probe: &serde_json::Value) -> String {
    ne_agent::governance::digest::sha1_hex(probe.to_string().as_bytes())
}

/// 读可选 JSON 探针文件（第 17 批）：`None` → `Value::Null`；路径不存在 → 报错。
fn read_optional_json(path: &Option<String>) -> Result<serde_json::Value> {
    match path {
        None => Ok(serde_json::Value::Null),
        Some(p) => {
            let text = std::fs::read_to_string(p)
                .with_context(|| format!("读探针文件失败：{p}"))?;
            let v: serde_json::Value = serde_json::from_str(&text)
                .with_context(|| format!("探针文件不是合法 JSON：{p}"))?;
            Ok(v)
        }
    }
}

/// 由模板名 + 探针 JSON 构造模板变量（第 17 批）。
///
/// 探针对象里的顶层键**原样**成为模板变量（供 `{{var}}` 渲染）；无探针时给空表，
/// 渲染结果会保留未替换的占位符（只渲染不执行，不报错）。
fn build_prompt_vars(template: &str, probe: &serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    let mut vars = serde_json::Map::new();
    vars.insert("template".into(), serde_json::json!(template));
    if let Some(obj) = probe.as_object() {
        for (k, v) in obj {
            vars.insert(k.clone(), v.clone());
        }
    }
    vars
}


/// 最近一次逻辑快照的 snap_id（第 13 批）：作为冲突记录锚点；无快照则 None。
fn wg_latest_snapshot(conn: &rusqlite::Connection) -> Option<String> {
    ne_agent::governance::diff_snapshot::list(conn)
        .ok()
        .and_then(|v| v.into_iter().next())
        .and_then(|x| x["snap_id"].as_str().map(String::from))
}
/// 导出路径防护（第 10 批）：拒绝含 `..` 的路径；相对路径解析到 lib_root 下。
/// `--to` 允许绝对路径（用户显式指定），但仍拒绝 `..` 段。
fn safe_export_path(lib_root: &Path, to: &Path) -> Result<PathBuf> {
    let s = to.to_string_lossy();
    if s.contains("..") {
        anyhow::bail!("导出路径不得含 '..'（防路径逃逸）：{s}");
    }
    if to.is_absolute() {
        Ok(to.to_path_buf())
    } else {
        Ok(lib_root.join(to))
    }
}

fn run_world_gov(vault: &Vault, lib_root: &Path, cmd: &WorldGovCmd, json: bool, quiet: bool) -> Result<()> {
    use ne_agent::world_gov;
    use ne_agent::governance::{get_semantic_gate, semantic_gate_status};
    let conn = ne_agent::governance::open_gov(lib_root)?;
    match cmd {
        WorldGovCmd::GateCheck { semantic } => {
            let mut st = semantic_gate_status(*semantic);
            // 能力标识（仅告知，不自动开启）
            st["persist_mode_available"] = serde_json::json!(true);
            st["llm_runtime_defined"] = serde_json::json!(true);
            st["llm_real_call_enabled"] = serde_json::json!(false);
            // 审计报告导出：纯读拼接，不依赖门控，恒可用
            st["report_export_ready"] = serde_json::json!(true);
            st["prompt_preview_ready"] = serde_json::json!(true);
            st["incremental_scan_ready"] = serde_json::json!(true);
            st["batch_inspect_ready"] = serde_json::json!(true);
            // 工具链版本标记（仅标识，无逻辑作用）
            st["toolchain_epoch"] = serde_json::json!(17);
            st["llm_epoch"] = serde_json::json!(17);
            st["llm_integrated"] = serde_json::json!(true);
            // 第 17 批：LLM 深度接入状态（与第 13 批三重锁打通）
            {
                let ls = ne_agent::governance::llm_gate::lock_status(false);
                st["llm_available"] = serde_json::json!(ne_agent::governance::llm_gate::llm_integration_enabled());
                st["llm_gate_locked"] = ls["gate_locked"].clone();
                st["active_backend"] = serde_json::json!(
                    ne_agent::governance::llm_config::load(lib_root)
                        .map(|c| c.default_backend).unwrap_or_else(|_| "ollama".into()));
                st["cache_enabled"] = serde_json::json!(
                    ne_agent::governance::llm_config::load(lib_root)
                        .map(|c| c.inference_cache_enable).unwrap_or(true));
            }
            st["tag_note_ready"] = serde_json::json!(true);
            st["diff_snap_ready"] = serde_json::json!(true);
            st["webpanel_ready"] = serde_json::json!(true);
            // 第 13 批：三重锁状态（缺哪把锁逐条列出；不自动开启）
            st["real_llm_triple_lock"] = ne_agent::governance::llm_gate::lock_status(false);
            st["conflict_store_ready"] = serde_json::json!(true);
            st["rel_graph_ready"] = serde_json::json!(true);
            st["sandbox_ready"] = serde_json::json!(true);
            st["llm_real_call_enabled"] = serde_json::json!(
                ne_agent::governance::llm_gate::real_llm_unlocked(false)
            );
            // 第 9 批能力标记
            for (k, v) in ne_agent::governance::self_check::capabilities().as_object().cloned().unwrap_or_default() {
                st[k] = v;
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&st)?);
            } else {
                println!("== 语义门控状态 ==");
                println!("① 编译期 SEMANTIC_ENABLE_GATE: {}", st["compile_gate"]);
                println!("② 命令行 --semantic:            {}", st["cli_semantic"]);
                println!("③ NOVEL_SEMANTIC_UNLOCK=yes:    {}（raw: {}）", st["env_unlock"], st["env_raw"]);
                println!("→ {}", st["status"]);
                println!("persist_mode_available: {}", st["persist_mode_available"]);
                println!("llm_runtime_defined: {} / llm_real_call_enabled: {}",
                    st["llm_runtime_defined"], st["llm_real_call_enabled"]);
            }
            return Ok(());
        }
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
        WorldGovCmd::InspectCard { key, semantic, dry_run, real_semantic, enqueue, persist, lib: _ } => {
            if *enqueue {
                let probe = format!("07_勘误工坊/inspect_{key}.json(占位)");
return gov_enqueue(&conn, ne_agent::governance::pipeline::JobKind::InspectCard,
                    None, None, probe, Some(key), *persist, json, &format!("inspect-card key={key}"));
            }
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
            // --semantic：dry-run 沙盘不发请求，永远放行预览；真实请求才过门控
            if *dry_run {
                let (sys, usr) = world_gov::semantic_messages(&r["probe_pack"]);
                if json {
                    println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                        "dry_run": true, "system": sys, "user_chars": usr.chars().count(),
                        "gate": semantic_gate_status(*semantic),
                    }))?);
                } else {
                    println!("[--dry-run] 只打印将发送的 prompt，不调模型、不写库：");
                    println!("── system ──\n{sys}\n── user（{} 字）──\n{}", usr.chars().count(),
                        usr.chars().take(500).collect::<String>());
                }
                return Ok(());
            }
            // --semantic 但未加 --real-semantic：既有门控路径（向后兼容，不变）
            if !*real_semantic {
                // 真实请求：三条件门控收口，未解锁一律 requested_but_locked（绝不裸调 llm::call）
                if !get_semantic_gate(*semantic) {
                    let st = semantic_gate_status(*semantic);
                    if json {
                        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                            "command": "world-gov inspect-card", "key": key,
                            "status": "REQUESTED_BUT_LOCKED",
                            "probe_pack": r["probe_pack"],
                            "gate": st,
                        }))?);
                    } else {
                        println!("[门控拦截] --semantic 真实请求未解锁（用 world-gov gate-check 查三条件真值）：");
                        println!("  ① 编译期 SEMANTIC_ENABLE_GATE: {}", st["compile_gate"]);
                        println!("  ② 命令行 --semantic:            {}", st["cli_semantic"]);
                        println!("  ③ NOVEL_SEMANTIC_UNLOCK=yes:    {}（raw: {}）", st["env_unlock"], st["env_raw"]);
                        println!("本次不发起任何真实模型请求，仅产探针包（可用 --dry-run 预览将发送的 prompt）。");
                    }
                    return Ok(());
                }
                // 既有门控已开但未加 --real-semantic（第 13 批第二道锁）→ 明确提示
                anyhow::bail!(
                    "既有门控已开，但第 13 批真实外呼还需三重锁：编译 feature `real-semantic` + WOGOV_ALLOW_REAL_LLM=yes + --real-semantic；用 world-gov gate-check 查真值。本次未发出任何请求。"
                );
            }
            // 第 17.3 批：统一走 semantic_pipeline（与 `llm run` 模板路径**同一条收口**：
            // 闸门→熔断→缓存→后端→护栏→记账→修复回合）。旧 `llm_client` 退化为过渡存根。
            use ne_agent::governance::{conflict_store as cs, llm_config as lc, prompt_builder as pb, semantic_pipeline as sp};
            let built = pb::build_inspect_prompt(&r["probe_pack"]);
            let baseline_fp = world_gov::read_baseline_fingerprint(lib_root).unwrap_or_default();
            let probe_hash = wg_probe_hash(&r["probe_pack"]);
            let snap_id = wg_latest_snapshot(&conn);
            let cfg = lc::load(lib_root)?;
            // 第 17.4 批：强制顶层 verdict（统一契约，schema 注入 system；网关忽略 response_format）。
            let raw = sp::RawPrompt {
                system: built.system.clone(),
                user: built.user.clone(),
                probe_hash: probe_hash.clone(),
                schema: pb::inspect_schema(),
                entity_fields: vec!["referenced_ids".to_string()],
                ..Default::default()
            };
            let outcome = sp::call_raw(&conn, lib_root, &cfg, *real_semantic, &raw)?;
            let oj = outcome.to_json();
            let gate_st = ne_agent::governance::llm_gate::lock_status(*real_semantic);
            let (verdict, tok_in, tok_out, status) = match &outcome {
                sp::Outcome::Success { content, token_in, token_out, .. } => (content.clone(), *token_in, *token_out, "OK"),
                sp::Outcome::CacheHit { content, .. } => (content.clone(), 0i64, 0i64, "CACHE_HIT"),
                sp::Outcome::Malformed { attempts, last_raw_head } =>
                    (serde_json::json!({"status":"MALFORMED","attempts":attempts,"last_raw_head":last_raw_head}), 0, 0, "MALFORMED"),
                sp::Outcome::GuardRejected { reason, guards } =>
                    (serde_json::json!({"status":"GUARD_REJECTED","reason":reason,"guards":guards}), 0, 0, "GUARD_REJECTED"),
                sp::Outcome::GateLocked(_) => (serde_json::json!({"status":"GATE_LOCKED"}), 0, 0, "GATE_LOCKED"),
                sp::Outcome::BreakerOpen => (serde_json::json!({"status":"BREAKER_OPEN"}), 0, 0, "BREAKER_OPEN"),
                sp::Outcome::Unavailable(m) => (serde_json::json!({"status":"UNAVAILABLE","note":m}), 0, 0, "UNAVAILABLE"),
                sp::Outcome::BackendError { kind, message } =>
                    (serde_json::json!({"status":"BACKEND_ERROR","kind":kind,"message":message}), 0, 0, "BACKEND_ERROR"),
            };
            // 仅成功/缓存命中 → 提取顶层 verdict（宽容解析）并落冲突库 + emit findings（第 17.4 批）
            // 第 18.5 批：CacheHit 去重——同 probe_hash 的相同输出且该卡已有 pending 观点时复用现有记录。
            let mut conflict_id = None;
            if status == "OK" || status == "CACHE_HIT" {
                // 宽容解析：即便模型外面套了 markdown，也抠出含 verdict 的 JSON 对象
                let parsed = world_gov::parse_semantic_verdict(key, &verdict.to_string());
                let has_verdict = parsed.get("verdict").is_some();
                let existing = if status == "CACHE_HIT" {
                    cs::find_pending_by_probe(&conn, key)?
                } else {
                    None
                };
                if let Some(existing) = existing {
                    conflict_id = Some(existing);
                } else if has_verdict {
                    conflict_id = Some(cs::insert(&conn, &cs::NewConflict {
                        probe_key: key.clone(),
                        snap_id: snap_id.clone(),
                        baseline_fp: baseline_fp.clone(),
                        verdict: parsed.clone(),
                        raw_prompt: built.user.clone(),
                        raw_response: verdict.to_string(),
                        token_in: tok_in,
                        token_out: tok_out,
                    })?);
                    let findings = vec![{
                        let mut f = parsed.clone();
                        if f.get("key").is_none() { f["key"] = serde_json::json!(key); }
                        f
                    }];
                    let _ = world_gov_emit(&conn, "inspect-card", &findings);
                }
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                    "command": "world-gov inspect-card", "key": key,
                    "status": status, "llm_gate": gate_st, "pipeline": oj,
                    "verdict": verdict, "conflict_id": conflict_id,
                    "audit": {"baseline_fp": baseline_fp, "probe_hash": probe_hash, "snap_id": snap_id},
                }))?);
            } else {
                println!("== inspect-card --real-semantic（统一 semantic_pipeline）==");
                println!("状态: {status}");
                match &outcome {
                    sp::Outcome::GateLocked(_) => {
                        println!("[三重锁拦截] 未解锁（缺以下条件）：");
                        if let Some(m) = gate_st["missing"].as_array() {
                            for x in m { println!("  ✗ {}", x.as_str().unwrap_or("")); }
                        }
                        println!("本次未发出任何请求，未消耗 token。");
                    }
                    sp::Outcome::Malformed { attempts, .. } => {
                        println!("[格式非法] 重试 {attempts} 次仍非合法 JSON（已标记 malformed，未写缓存）。");
                    }
                    _ => {}
                }
                println!("判定: {}", serde_json::to_string_pretty(&verdict)?);
                if let Some(id) = conflict_id { println!("已写入冲突库 id={id}（world-gov conflict show {id}）"); }
            }
        }
        WorldGovCmd::Conflict { cmd, lib: _ } => {
            use ne_agent::governance::conflict_store as cf;
            match cmd {
                ConflictCmd::List { status, key, full } => {
                    let rows = cf::list(&conn, status.as_deref(), key.as_deref())?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                            "count": rows.len(), "records": rows,
                        }))?);
                    } else {
                        println!("== 冲突记录库（{} 条；AI 观点仅参考，人是终审）==", rows.len());
                        for r in &rows {
                            let v = &r["verdict"]["verdict"];
                            let snap = r["snap_id"].as_str().unwrap_or("-");
                            println!("  #{:<5} {:<20} [{}] snap={} verdict={}",
                                r["id"], r["probe_key"].as_str().unwrap_or(""),
                                r["review_status"].as_str().unwrap_or(""), snap,
                                if v.is_null() { "-".to_string() } else { v.to_string() });
                        }
                        if rows.is_empty() { println!("  （空；先跑 inspect-card --semantic --real-semantic）"); }
                        if *full { println!("（--full 用 conflict show <id> --full 查看原始 prompt/response）"); }
                    }
                }
                ConflictCmd::Show { id, full } => {
                    match cf::show(&conn, *id)? {
                        None => {
                            if json { println!("{}", serde_json::json!({"status": "NOT_FOUND", "id": id})); }
                            else { println!("未找到冲突记录 id={id}"); }
                        }
                        Some((j, prompt, resp)) => {
                            if json {
                                let mut o = j.clone();
                                if *full {
                                    o["raw_prompt"] = serde_json::json!(prompt);
                                    o["raw_response"] = serde_json::json!(resp);
                                }
                                println!("{}", serde_json::to_string_pretty(&o)?);
                            } else {
                                println!("== 冲突记录 #{id} ==");
                                println!("probe_key: {}  review_status: {}", j["probe_key"], j["review_status"]);
                                println!("snap_id: {}  baseline_fp: {}", j["snap_id"], j["baseline_fp"]);
                                println!("verdict: {}", serde_json::to_string_pretty(&j["verdict"])?);
                                println!("token: in={} out={}", j["token_in"], j["token_out"]);
                                if *full {
                                    println!("── raw_prompt（{} 字）──\n{}", j["raw_prompt_chars"], prompt);
                                    println!("── raw_response（{} 字）──\n{}", j["raw_response_chars"], resp);
                                } else {
                                    println!("（加 --full 查看原始 prompt/response 全文）");
                                }
                            }
                        }
                    }
                }
                ConflictCmd::Review { id, status, flag } => {
                    let ok = cf::set_review(&conn, *id, status)?;
                    if !ok {
                        if json { println!("{}", serde_json::json!({"status": "NOT_FOUND", "id": id})); }
                        else { println!("未找到冲突记录 id={id}"); }
                        return Ok(());
                    }
                    let mut flagged = false;
                    if *flag {
                        if let Some((j, _, _)) = cf::show(&conn, *id)? {
                            if let Some(k) = j["probe_key"].as_str() {
                                cf::flag_probe_key_as_ai(&conn, k)?;
                                flagged = true;
                            }
                        }
                    }
                    if json {
                        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                            "id": id, "review_status": status, "ai_flagged": flagged,
                            "note": "仅更新复核状态/打 ai-flagged 标签；绝不改资料库正文或主索引。",
                        }))?);
                    } else {
                        println!("已更新 #{id} → {}", status);
                        if flagged { println!("并给条目打标签 `{}`（供人复核）", cf::AI_FLAG_TAG); }
                        println!("（不改资料库正文与 library_index）");
                    }
                }
                ConflictCmd::Summary => {
                    let s = cf::summary(&conn)?;
                    if json { println!("{}", serde_json::to_string_pretty(&s)?); }
                    else {
                        println!("== 冲突记录库统计 ==");
                        println!("总数: {}", s["total"]);
                        if let Some(by) = s["by_review_status"].as_object() {
                            for k in cf::REVIEW_STATUSES {
                                println!("  {:<12} {}", k, by.get(*k).cloned().unwrap_or(serde_json::json!(0)));
                            }
                        }
                    }
                }
            }
        }
        WorldGovCmd::Graph { cmd, lib: _ } => {
            use ne_agent::governance::rel_graph as rg;
            match cmd {
                GraphCmd::Build { dry_run, max_cards, snap } => {
                    let r = rg::build(&conn, lib_root, *max_cards, *dry_run, snap.as_deref())?;
                    if json { println!("{}", serde_json::to_string_pretty(&r)?); }
                    else {
                        println!("== 关系图构建{} ==", if *dry_run { "（--dry-run）" } else { "" });
                        println!("扫描卡数: {}  节点: {}  边: {}  悬空(去重): {}",
                            r["cards_scanned"], r["nodes"], r["edges"], r["dangling_unique"]);
                        if r["unreadable"].as_u64().unwrap_or(0) > 0 {
                            println!("[warn] {} 张卡源文件不可读（已跳过）", r["unreadable"]);
                        }
                        println!("{}", r["note"].as_str().unwrap_or(""));
                    }
                }
                GraphCmd::List { key, limit } => {
                    let r = rg::list(&conn, key.as_deref(), *limit)?;
                    if json { println!("{}", serde_json::to_string_pretty(&r)?); }
                    else {
                        println!("== 引用边（显示 {} 条）==", r["shown"]);
                        if let Some(es) = r["edges"].as_array() {
                            for e in es {
                                println!("  {} → {}", e["src"].as_str().unwrap_or(""), e["dst"].as_str().unwrap_or(""));
                            }
                        }
                    }
                }
                GraphCmd::Summary { top } => {
                    let r = rg::summary(&conn, *top)?;
                    if json { println!("{}", serde_json::to_string_pretty(&r)?); }
                    else {
                        println!("== 关系网摘要 ==");
                        println!("节点: {}  边: {}  悬空(去重): {}  孤立卡: {}",
                            r["nodes"], r["edges"], r["dangling_unique"], r["orphans"]);
                        println!("出度 TOP：");
                        if let Some(t) = r["top_out_degree"].as_array() {
                            for x in t { println!("  {}  ({})", x["key"].as_str().unwrap_or(""), x["degree"]); }
                        }
                        println!("入度 TOP：");
                        if let Some(t) = r["top_in_degree"].as_array() {
                            for x in t { println!("  {}  ({})", x["key"].as_str().unwrap_or(""), x["degree"]); }
                        }
                    }
                }
                GraphCmd::Dangling { limit } => {
                    let r = rg::dangling(&conn, *limit)?;
                    if json { println!("{}", serde_json::to_string_pretty(&r)?); }
                    else {
                        println!("== 悬空引用（共 {}，显示 {}）==", r["total"], r["shown"]);
                        if let Some(it) = r["items"].as_array() {
                            for x in it {
                                let srcs = x["referenced_by"].as_array().map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default();
                                println!("  ✗ {} ← 被引用于: {}", x["missing_key"].as_str().unwrap_or(""), srcs);
                            }
                        }
                    }
                }
                GraphCmd::Orphans { limit } => {
                    let r = rg::orphans(&conn, *limit)?;
                    if json { println!("{}", serde_json::to_string_pretty(&r)?); }
                    else {
                        println!("== 孤立卡（无入边，共 {}，显示 {}）==", r["total"], r["shown"]);
                        if let Some(it) = r["items"].as_array() {
                            for x in it { println!("  - {}", x.as_str().unwrap_or("")); }
                        }
                    }
                }
                GraphCmd::Neighbors { key, dir } => {
                    let r = rg::neighbors(&conn, key, dir.as_deref().unwrap_or(""))?;
                    if json { println!("{}", serde_json::to_string_pretty(&r)?); }
                    else {
                        println!("== {key} 的邻居 ==");
                        if let Some(d) = r["downstream_refs"].as_array() {
                            println!("下游（该卡引用）:");
                            for x in d { println!("  → {}{}", x["key"].as_str().unwrap_or(""), if x["exists"] == false { "（悬空）" } else { "" }); }
                        }
                        if let Some(u) = r["upstream_refs"].as_array() {
                            println!("上游（引用该卡）:");
                            for x in u { println!("  ← {}{}", x["key"].as_str().unwrap_or(""), if x["exists"] == false { "（悬空）" } else { "" }); }
                        }
                    }
                }
                GraphCmd::Path { from, to, max_depth } => {
                    let r = rg::path(&conn, from, to, *max_depth)?;
                    if json { println!("{}", serde_json::to_string_pretty(&r)?); }
                    else if r["found"] == true {
                        let chain = r["chain"].as_array().map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(" → ")).unwrap_or_default();
                        println!("== 引用链（{} 跳）==\n  {}", r["len"], chain);
                    } else {
                        println!("{from} → {to}：在深度 {} 内未找到引用链", r["max_depth"]);
                    }
                }
                GraphCmd::Cycles { max_len, limit } => {
                    let r = rg::cycles(&conn, *max_len, *limit)?;
                    if json { println!("{}", serde_json::to_string_pretty(&r)?); }
                    else {
                        println!("== 循环引用（共 {}，显示 {}；max_len={}）==", r["total"], r["shown"], r["max_len"]);
                        if let Some(it) = r["items"].as_array() {
                            for c in it {
                                let s = c.as_array().map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(" → ")).unwrap_or_default();
                                println!("  ⟳ {s} → (回到起点)");
                            }
                        }
                    }
                }
            }
        }
        WorldGovCmd::Sandbox { cmd, lib: _ } => {
            use ne_agent::governance::sandbox as sb;
            match cmd {
                SandboxCmd::Run { scenario, quick, actor, label, save } => {
                    let sc = match (scenario, quick) {
                        (Some(p), None) => serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(p)?)
                            .map_err(|e| anyhow::anyhow!("场景 JSON 解析失败：{e}"))?,
                        (None, Some(q)) => sb::quick_scenario(q)?,
                        (Some(_), Some(_)) => anyhow::bail!("--scenario 与 --quick 只能二选一"),
                        (None, None) => anyhow::bail!("需给 --scenario <文件> 或 --quick <warn|critical|tip|calm>"),
                    };
                    let r = sb::run(&conn, lib_root, label, actor.as_deref(), &sc, *save)?;
                    if json { println!("{}", serde_json::to_string_pretty(&r)?); }
                    else {
                        println!("== 沙盘推演{} ==", if r["saved"] == true { format!(" #{}", r["run_id"]) } else { "（未落库）".into() });
                        println!("步数: {}  判决: {}  临界步: {}", r["steps"], r["verdict"], r["critical_steps"]);
                        println!("指纹: {}", r["fingerprint"]);
                        println!("最终态: {}", serde_json::to_string(&r["final_state"])?);
                        if let Some(a) = r["affected"].as_object() {
                            let n = a["cards"].as_array().map(|x| x.len()).unwrap_or(0);
                            println!("受影响卡（关系网一跳）: {n} 张  actor={}", a["actor"]);
                        }
                        println!("{}", r["note"].as_str().unwrap_or(""));
                    }
                }
                SandboxCmd::Verify { run_id } => {
                    let r = sb::verify(&conn, *run_id)?;
                    if json { println!("{}", serde_json::to_string_pretty(&r)?); }
                    else if r["status"] == "NOT_FOUND" {
                        println!("未找到推演记录 #{run_id}");
                    } else {
                        println!("== 重放校验 #{} ==", run_id);
                        println!("结果: {}（确定性: {}）", r["status"], r["deterministic"]);
                        println!("记录指纹: {}", r["stored_fingerprint"]);
                        println!("重放指纹: {}", r["replay_fingerprint"]);
                        println!("记录判决: {}  重放判决: {}", r["stored_verdict"], r["replay_verdict"]);
                    }
                }
                SandboxCmd::List { limit } => {
                    let r = sb::list(&conn, *limit)?;
                    if json { println!("{}", serde_json::to_string_pretty(&r)?); }
                    else {
                        println!("== 沙盘推演记录（{} 条）==", r["count"]);
                        if let Some(rs) = r["runs"].as_array() {
                            for x in rs {
                                println!("  #{} [{}] steps={} verdict={} crit={} fp={}",
                                    x["id"], x["label"].as_str().unwrap_or(""),
                                    x["steps"], x["verdict"], x["critical_steps"],
                                    x["fingerprint"].as_str().unwrap_or("").chars().take(8).collect::<String>());
                            }
                        }
                    }
                }
                SandboxCmd::Show { run_id, full } => {
                    let r = sb::show(&conn, *run_id, *full)?;
                    if json { println!("{}", serde_json::to_string_pretty(&r)?); }
                    else if r["status"] == "NOT_FOUND" {
                        println!("未找到推演记录 #{run_id}");
                    } else {
                        println!("== 沙盘推演 #{} ==", run_id);
                        println!("标签: {}  actor: {}", r["label"], r["actor"]);
                        println!("步数: {}  判决: {}  临界步: {}  epoch: {}", r["steps"], r["verdict"], r["critical_steps"], r["engine_epoch"]);
                        println!("指纹: {}", r["fingerprint"]);
                        println!("最终态: {}", serde_json::to_string_pretty(&r["final_state"])?);
                        if *full {
                            if let Some(sd) = r["steps_detail"].as_array() {
                                println!("── 逐步（{}）──", sd.len());
                                for s in sd {
                                    println!("  step {} chapter {} verdict {}  state={}",
                                        s["step"], s["state"]["chapter"], s["state"]["verdict"],
                                        serde_json::to_string(&s["state"]["state"]).unwrap_or_default());
                                }
                            }
                        } else { println!("（加 --full 查看 scenario 与逐步状态）"); }
                    }
                }
                SandboxCmd::Delete { run_id } => {
                    let ok = sb::delete(&conn, *run_id)?;
                    if json { println!("{}", serde_json::json!({"deleted": ok, "run_id": run_id})); }
                    else { println!("{}推演记录 #{}", if ok { "已删除" } else { "未找到" }, run_id); }
                }
                SandboxCmd::Spawn { from_snap, save, ai_sandbox, dry_run } => {
                    let r = sb::spawn_from_snapshot(&conn, lib_root, from_snap, *save && !*ai_sandbox)?;
                    // 第 18 批修正：--ai-sandbox 时**只输出一个** JSON 文档（spawn 段并入 `spawn` 字段），
                    // 避免两个顶层 JSON 文档拼接 → jq/解析器报 `Extra data`。
                    if !*ai_sandbox {
                        if json { println!("{}", serde_json::to_string_pretty(&r)?); }
                        else if r["status"] == "SNAPSHOT_NOT_FOUND" {
                            println!("未找到快照 {from_snap}；先 world-gov diff snap create --label ...");
                        } else {
                            println!("== 沙盘 spawn（从快照派生初始态）==");
                            println!("快照: {}  条目: {}", r["snap_id"].as_str().unwrap_or("-"), r["entries"]);
                            println!("元指纹: {}", r["meta_fingerprint"].as_str().unwrap_or("-"));
                            println!("初始态: {}", serde_json::to_string(&r["scenario"]["world"])?);
                            if r["saved"] == true { println!("已落库为 0 步基线记录 #{}", r["run_id"]); }
                            println!("{}", r["note"].as_str().unwrap_or(""));
                        }
                    } else {
                        // ai-sandbox 模式：人类可读摘要（JSON 在下方合并输出）
                        if !json && r.get("scenario").is_some() {
                            println!("== 沙盘 spawn（从快照派生初始态）==");
                            println!("快照: {}  条目: {}", r["snap_id"].as_str().unwrap_or("-"), r["entries"]);
                            println!("初始态: {}", serde_json::to_string(&r["scenario"]["world"])?);
                        }
                    }
                    // 【规约适配】--ai-sandbox：从快照派生后，用已放行的 LLM 做一次 AI 引导推演。
                    // 默认只算不写；仍受三重锁约束（feature + WOGOV_ALLOW_REAL_LLM=yes + 本次调用）。
                    if *ai_sandbox {
                        if r.get("scenario").is_none() {
                            anyhow::bail!("ai-sandbox 需要可用的快照初始态（快照不存在或不可读）");
                        }
                        // 复用统一收口：把派生初始态打包成一步 AI 推演请求
                        use ne_agent::governance::{llm_config as lc, semantic_pipeline as sp};
                        let cfg = lc::load(lib_root)?;
                        let probe = serde_json::json!({
                            "kind": "ai_sandbox_step",
                            "snap_id": from_snap,
                            "initial_world": r["scenario"]["world"].clone(),
                            "meta_fingerprint": r["meta_fingerprint"].clone(),
                        });
                        let ph = sp::probe_hash(&probe);
                        let system = "你是《残响之河》世界观沙盘推演器。基于给定初始态走一步确定性推演，\
只输出一个 JSON 对象（键：step/state/verdict/note），禁止 markdown/解释文字。".to_string();
                        let user = format!("初始态：{}", serde_json::to_string(&r["scenario"]["world"])?);
                        let raw = sp::RawPrompt {
                            system, user, probe_hash: ph,
                            // 第 18 批修正：`step` 对「AI 沙箱一步推演」非语义核心——
                            // 模型在无明确步号时会合法地返回 `step:null`。原 schema 把 `step`
                            // 设为 required+integer → 非确定性误拒（GUARD_REJECTED/schema）。
                            // 修法：`step` 移出 required 且不约束类型；required 只保留语义核心
                            // （state/verdict）。state 允许扩展字段（additionalProperties 默认放行）。
                            schema: serde_json::json!({"type":"object","required":["state","verdict"],
                                "properties":{"step":{},"state":{"type":"object"},
                                "verdict":{"type":"string"},"note":{"type":"string"}}}),
                            ..Default::default()
                        };
                        // ai-sandbox 的真实性由命令行 `--ai-sandbox` 充当命令行锁
                        let outcome = sp::call_raw(&conn, lib_root, &cfg, true, &raw)?;
                        let oj = outcome.to_json();
                        if json {
                            println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                                "command": "sandbox spawn --ai-sandbox",
                                "spawn": r, "llm_gate": ne_agent::governance::llm_gate::lock_status(true),
                                "pipeline": oj, "dry_run": dry_run,
                            }))?);
                        } else {
                            println!("── AI 沙箱推演（--ai-sandbox）──");
                            println!("状态: {}", oj["status"].as_str().unwrap_or("-"));
                            match &outcome {
                                sp::Outcome::GateLocked(_) => println!("[三重锁拦截] 未解锁；未发出任何请求。"),
                                sp::Outcome::Success { content, .. } => println!("AI 步骤: {}", serde_json::to_string_pretty(content)?),
                                sp::Outcome::CacheHit { content, .. } => println!("[缓存命中] AI 步骤: {}", serde_json::to_string_pretty(content)?),
                                sp::Outcome::Malformed { attempts, .. } => println!("[格式非法] 重试 {attempts} 次仍非合法 JSON。"),
                                other => println!("{:?}", other),
                            }
                            if *dry_run { println!("[--dry-run] AI 引导结果未落库（沙盘与基准库强隔离）。"); }
                        }
                    }
                }
                SandboxCmd::Trace { cmd } => match cmd {
                    SandboxTraceCmd::List { run } => {
                        let r = sb::trace(&conn, *run)?;
                        if json { println!("{}", serde_json::to_string_pretty(&r)?); }
                        else if r["status"] == "NOT_FOUND" {
                            println!("未找到推演记录 #{run}");
                        } else {
                            println!("== 沙盘轨迹 #{}（{} 步）==", run, r["steps"]);
                            if let Some(tr) = r["trace"].as_array() {
                                for s in tr {
                                    println!("  step {} chap {} [{}]  state={}",
                                        s["step"], s["chapter"], s["verdict"].as_str().unwrap_or("-"),
                                        serde_json::to_string(&s["state"]).unwrap_or_default());
                                }
                            }
                        }
                    }
                },
            }
        }
        WorldGovCmd::Status { lib: _ } => {
            use ne_agent::governance::overview;
            let r = overview::overview(&conn, lib_root)?;
            if json { println!("{}", serde_json::to_string_pretty(&r)?); }
            else {
                println!("== world-gov 终态总览（epoch {}）==", r["toolchain_epoch"]);
                println!("资料库: {}  卡数: {}", r["lib_root"], r["library"]["cards"]);
                println!("基线: {} 条 指纹 {}", r["baseline"]["entries"], r["baseline"]["fingerprint"]);
                println!("三重锁: {}", r["triple_lock"]["status"]);
                println!("── 隔离表（与主索引物理隔离）──");
                if let Some(m) = r["isolated_tables"].as_object() {
                    for (k, v) in m { println!("  {k:<16} {}", v); }
                }
                println!("── 关系网 ──  边 {}  悬空 {}  已建 {}",
                    r["graph"]["edges"], r["graph"]["dangling"], r["graph"]["meta"]["built_ts"].is_i64());
                println!("── 沙盘 ──  记录 {}  ok {}", r["sandbox"]["runs"], r["sandbox"]["check_ok"]);
                println!("── 闭环链 ──");
                if let Some(c) = r["chain"].as_array() {
                    for x in c {
                        println!("  [{}] {}{}", if x["ready"] == true { "✓" } else { "·" },
                            x["step"].as_str().unwrap_or(""),
                            if x["locked_by_default"] == true { "（默认锁死）" } else { "" });
                    }
                }
                println!("── 零污染证明 ── {}", if r["no_pollution"]["ok"] == true { "✅ 主索引未被污染" } else { "⚠ 见 --json 缺项" });
                println!("{}", r["note"].as_str().unwrap_or(""));
            }
        }
        WorldGovCmd::Llm { cmd, lib: _ } => {
            use ne_agent::governance::{inference_cache as ic, llm_adapter as la, llm_config as lc, llm_obs as lo, prompt_lib as pl, semantic_pipeline as sp};
            let cfg = lc::load(lib_root)?;
            match cmd {
                LlmCmd::Stats { reset_breaker } => {
                    if *reset_breaker { lo::reset_breaker(&conn)?; }
                    let s = lo::stats(&conn)?;
                    if json { println!("{}", serde_json::to_string_pretty(&s)?); }
                    else {
                        println!("== LLM 调用统计 ==");
                        println!("总调用: {}  失败: {}  缓存命中: {}  命中率 {:.1}%",
                            s["total_calls"], s["total_fail"], s["cache_hits"],
                            s["cache_hit_rate"].as_f64().unwrap_or(0.0) * 100.0);
                        println!("token 累计: in={} out={}", s["token_in_total"], s["token_out_total"]);
                        println!("平均耗时: {:.0} ms", s["avg_elapsed_ms"].as_f64().unwrap_or(0.0));
                        println!("熔断: {}", if s["breaker_tripped"] == true { "已触发（退化为静态模式）" } else { "正常" });
                        if *reset_breaker { println!("（已重置熔断计数）"); }
                    }
                }
                LlmCmd::Backends => {
                    let b = la::available_backends(&cfg);
                    if json { println!("{}", serde_json::to_string_pretty(&b)?); }
                    else {
                        println!("== LLM 后端 ==");
                        println!("默认: {}", b["default_backend"].as_str().unwrap_or("-"));
                        println!("真实后端: {}（feature {}）", b["real_backends"],
                            if b["feature_enabled"] == true { "已开" } else { "未开→死分支" });
                        println!("测试后端: {}", b["test_backend"].as_str().unwrap_or("-"));
                        println!("{}", b["note"].as_str().unwrap_or(""));
                    }
                }
                LlmCmd::Cache(c) => match c {
                    LlmCacheCmd::Status => {
                        let s = ic::status(lib_root, cfg.cache_ttl_days)?;
                        if json { println!("{}", serde_json::to_string_pretty(&s)?); }
                        else {
                            println!("== 推理缓存状态 ==");
                            println!("目录: {}", s["dir"]);
                            println!("条目: {}  总大小: {} bytes  过期: {}", s["entries"], s["total_bytes"], s["stale_entries"]);
                        }
                    }
                    LlmCacheCmd::List { limit } => {
                        let l = ic::list(lib_root, *limit)?;
                        if json { println!("{}", serde_json::to_string_pretty(&l)?); }
                        else {
                            println!("== 推理缓存（{} 条）==", l["shown"]);
                            if let Some(es) = l["entries"].as_array() {
                                for e in es {
                                    println!("  {} [{}] {} in={} out={}",
                                        e["key"].as_str().unwrap_or("").chars().take(12).collect::<String>(),
                                        e["backend"].as_str().unwrap_or(""), e["model"].as_str().unwrap_or(""),
                                        e["token_in"], e["token_out"]);
                                }
                            }
                        }
                    }
                    LlmCacheCmd::Gc { ttl_days } => {
                        let n = ic::gc(lib_root, *ttl_days)?;
                        if json { println!("{}", serde_json::json!({"removed": n, "ttl_days": ttl_days})); }
                        else { println!("已清理 {n} 条过期缓存（ttl={ttl_days} 天）"); }
                    }
                    LlmCacheCmd::Clear => {
                        let n = ic::clear(lib_root)?;
                        if json { println!("{}", serde_json::json!({"cleared": n})); }
                        else { println!("已清空 {n} 条缓存（不影响主库/校验逻辑）"); }
                    }
                },
                LlmCmd::Config(c) => match c {
                    LlmConfigCmd::Show => {
                        let s = lc::safe_snapshot(&cfg);
                        if json { println!("{}", serde_json::to_string_pretty(&s)?); }
                        else {
                            println!("== LLM 配置（安全快照）==");
                            println!("默认后端: {}", s["default_backend"].as_str().unwrap_or("-"));
                            if let Some(eps) = s["endpoints"].as_object() {
                                for (k, v) in eps {
                                    println!("  {k}: base={} key={} model={}",
                                        v["base_configured"], v["api_key_configured"], v["model_configured"]);
                                }
                            }
                            println!("{}", s["note"].as_str().unwrap_or(""));
                        }
                    }
                    LlmConfigCmd::Init { force } => {
                        let p = lc::init_template(lib_root, *force)?;
                        println!("配置模板: {}（{}）", p.display(), if *force { "已覆盖" } else { "已存在则不覆盖" });
                    }
                    LlmConfigCmd::Path => {
                        println!("{}", lib_root.join(lc::CONFIG_REL).display());
                    }
                    LlmConfigCmd::InitPrompts { force } => {
                        let w = pl::init_templates(lib_root, *force)?;
                        println!("已写提示词模板 {} 个：", w.len());
                        for p in w { println!("  {}", p.display()); }
                    }
                },
                LlmCmd::Preview { template, probe, out } => {
                    let probe_json = read_optional_json(probe)?;
                    let vars = build_prompt_vars(template, &probe_json);
                    let ph = if probe_json.is_null() { String::new() } else { sp::probe_hash(&probe_json) };
                    let req = sp::PipelineRequest { template: template.clone(), vars, probe_hash: ph, ..Default::default() };
                    let v = sp::preview(lib_root, &cfg, &req)?;
                    if let Some(path) = out {
                        std::fs::write(path, format!("== system ==\n{}\n\n== user ==\n{}",
                            v["system"].as_str().unwrap_or(""), v["user"].as_str().unwrap_or("")))?;
                        if json { println!("{}", serde_json::to_string_pretty(&v)?); }
                        else { println!("完整 prompt 已导出（未发起任何调用）：{path}"); }
                    } else if json { println!("{}", serde_json::to_string_pretty(&v)?); }
                    else {
                        println!("== prompt 预览（只渲染不发送）==");
                        println!("模板: {}  后端: {}  模型: {}",
                            v["template"].as_str().unwrap_or("-"),
                            v["backend"].as_str().unwrap_or("-"),
                            v["model"].as_str().unwrap_or("-"));
                        println!("cache_key: {}", v["cache_key"].as_str().unwrap_or("-"));
                        println!("── system ──\n{}", v["system"].as_str().unwrap_or(""));
                        println!("── user ──\n{}", v["user"].as_str().unwrap_or(""));
                    }
                }
                LlmCmd::Run { template, probe, real_semantic, mock, bypass_cache } => {
                    let probe_json = read_optional_json(probe)?;
                    let vars = build_prompt_vars(template, &probe_json);
                    let ph = if probe_json.is_null() { String::new() } else { sp::probe_hash(&probe_json) };
                    let req = sp::PipelineRequest {
                        template: template.clone(), vars, probe_hash: ph,
                        bypass_cache: *bypass_cache, use_mock: *mock, ..Default::default()
                    };
                    let o = sp::call(&conn, lib_root, &cfg, *real_semantic, &req)?;
                    let oj = o.to_json();
                    if json { println!("{}", serde_json::to_string_pretty(&oj)?); }
                    else {
                        println!("== LLM 管线调用 ==");
                        println!("状态: {}  后端: {}  模型: {}",
                            oj["status"].as_str().unwrap_or("-"),
                            oj["backend"].as_str().unwrap_or("-"), oj["model"].as_str().unwrap_or("-"));
                        match &o {
                            sp::Outcome::GateLocked(_) => {
                                println!("[三重闸门锁死] 未发起任何请求。缺：feature `llm_integration` + WOGOV_ALLOW_REAL_LLM=yes + --real-semantic");
                            }
                            sp::Outcome::BreakerOpen => println!("[熔断] 连续失败触发熔断；退化为静态模式，稍后自动恢复。"),
                            sp::Outcome::Unavailable(m) => println!("[不可用] {m}"),
                            sp::Outcome::CacheHit { key, .. } => println!("[缓存命中] {key}（未外呼）"),
                            sp::Outcome::Success { content, guards, token_in, token_out, .. } => {
                                println!("token: in={token_in} out={token_out}");
                                println!("护栏: {}", serde_json::to_string(guards)?);
                                println!("输出: {}", serde_json::to_string_pretty(content)?);
                            }
                            sp::Outcome::GuardRejected { reason, guards } => {
                                println!("[护栏拒绝] {reason}: {}", serde_json::to_string(guards)?);
                            }
                            sp::Outcome::Malformed { attempts, last_raw_head } => {
                                println!("[格式非法] 重试 {attempts} 次仍非合法 JSON（已标记 malformed，未写缓存）。");
                                println!("模型原始输出前 200 字：{last_raw_head}");
                            }
                            sp::Outcome::BackendError { kind, message } => println!("[后端错误] {kind}: {message}"),
                        }
                    }
                }
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
        WorldGovCmd::ScanLibrary { semantic, slice, dry_run, enqueue, persist, all_layers, since, since_snap, write_snapshot, no_cache, invalidate_cache, with_tag, without_tag, lib: _ } => {
            if *enqueue {
                if !with_tag.is_empty() || !without_tag.is_empty() || since_snap.is_some() {
                    anyhow::bail!("--with-tag/--without-tag/--since-snap 仅适用于 scan（不加 --enqueue）；入队为整片聚合任务，无法按标签/快照细分");
                }
                let (sn, sm) = match slice.as_deref() {
                    Some(s) => world_gov::parse_slice_strict(s).map_err(|e| anyhow::anyhow!("{e}"))?,
                    None => (1, 1),
                };
                let probe = format!("07_勘误工坊/scan_probe_{sn}of{sm}.json");
                return gov_enqueue(&conn, ne_agent::governance::pipeline::JobKind::ScanLibrary,
                    Some((sn as u32, sm as u32)), None, probe, None, *persist, json,
                    &format!("scan-library slice={sn}/{sm}"));
            }
            // 标签筛选（第 10 批；空 = 不过滤）
            let tag_filter = ne_agent::governance::note_tag_store::TagFilter::build(with_tag, without_tag)?;
            // 版本快照增量（第 11 批）：--since-snap → 自该快照以来 增/改 的 key 白名单
            let mut since_snap_note: Option<String> = None;
            let key_filter: Option<std::collections::HashSet<String>> = match since_snap {
                Some(sid) => {
                    let cur = ne_agent::governance::diff_snapshot::collect_meta(&conn, lib_root)?;
                    let changed = ne_agent::governance::diff_snapshot::changed_since(&conn, sid, &cur)?;
                    since_snap_note = Some(format!("since-snap {sid} → {} 个改动条目", changed.len()));
                    Some(changed.into_iter().collect())
                }
                None => None,
            };
            // 解析 --since（支持 last；无快照时降级全扫）
            let (since_ts, since_note) = match since {
                Some(v) => {
                    let (t, note) = ne_agent::governance::scan_snapshot::parse_since(v, lib_root)?;
                    (t, Some(note))
                }
                None => (None, None),
            };
            // 进度：粗估（stderr；--quiet 静默）
            ne_agent::governance::progress::emit(quiet, "scan-library", 0, 1, None);
            let r = world_gov::scan_library(&conn, lib_root, slice.as_deref(), *semantic, *dry_run, *all_layers, since_ts, Some(&tag_filter), key_filter.as_ref())?;
            ne_agent::governance::progress::emit(quiet, "scan-library", 1, 1, Some(0));
            // 探针缓存：把本片 probe_items 写入缓存（默认开启；--no-cache 关闭；--invalidate-cache 强制重写）
            // 缓存 key = hash(layer+key+src_mtime+baseline_fingerprint)；命中即复用，不重算。
            let mut cache_hits = 0usize;
            let mut cache_miss = 0usize;
            let baseline_fp = world_gov::read_baseline_fingerprint(lib_root).unwrap_or_default();
            if !*no_cache {
                use ne_agent::governance::probe_cache as pc;
                if let Some(items) = r["probe_items"].as_array() {
                    for it in items {
                        let layer = it["layer"].as_str().unwrap_or("");
                        let key = it["key"].as_str().unwrap_or("");
                        let mtime = it["src_mtime"].as_i64().unwrap_or(0);
                        let source = it["source"].as_str().unwrap_or("");
                        if !*invalidate_cache && pc::get(lib_root, layer, key, mtime, &baseline_fp).is_some() {
                            cache_hits += 1;
                        } else {
                            let _ = pc::put(lib_root, layer, key, mtime, &baseline_fp, source, it);
                            cache_miss += 1;
                        }
                    }
                }
            }
            ne_agent::governance::structured_log::info("world-gov scan-library", "scan.done", serde_json::json!({
                "slice": format!("{}of{}", r["slice"]["n"], r["slice"]["m"]),
                "used": r["counts"]["used"],
                "cache_hits": cache_hits, "cache_miss": cache_miss,
                "is_incremental": r["counts"]["is_incremental"],
            }));
            // 写回快照（仅成功且非 dry-run 且显式 --write-snapshot）
            let snapshot_written = if *write_snapshot && !*dry_run {
                let used = r["counts"]["used"].as_u64().unwrap_or(0) as usize;
                let fp = r["counts"]["scan_fingerprint"].as_str().unwrap_or("").to_string();
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                let snap = ne_agent::governance::scan_snapshot::ScanSnapshot {
                    last_scan_at: now, last_used_count: used, fingerprint: fp,
                };
                let p = ne_agent::governance::scan_snapshot::write_snapshot(lib_root, &snap)?;
                Some(p.display().to_string())
            } else {
                None
            };
            if json {
                let mut out = r.clone();
                if !*no_cache {
                    out["cache"] = serde_json::json!({"hits": cache_hits, "miss": cache_miss});
                }
                if let Some(n) = &since_note { out["since_note"] = serde_json::json!(n); }
                if let Some(n) = &since_snap_note { out["since_snap_note"] = serde_json::json!(n); }
                if let Some(p) = &snapshot_written { out["snapshot_written"] = serde_json::json!(p); }
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                let s = &r["slice"];
                println!("== 探针分片清单（零模型骨架）==");
                if let Some(n) = &since_note { println!("增量: {n}"); }
                if let Some(n) = &since_snap_note { println!("版本增量: {n}"); }
                println!("分片 {}/{}：偏移 {}，本片 {} 条 / 扫描域 {} 条", s["n"], s["m"], s["offset"], s["items"], s["total"]);
                if let Some(c) = r.get("counts") {
                    println!("层过滤: {}  （全库 {} → 扫描域 {}，过滤掉 {}）",
                        c["layer_filter"], c["total_before"], c["used"], c["filtered_aux"]);
                    if c["is_incremental"].as_bool().unwrap_or(false) {
                        println!("增量标记: is_incremental=true  since={}  降级={}",
                            c["since_ts"], c["mtime_degraded"]);
                    }
                }
                println!("semantic: {}", r["semantic"]);
                println!("入库通路: {}", r["semantic_channel"]);
                println!("五查: {}", r["questions"].as_array().map(|a| a.len()).unwrap_or(0),);
                println!("待检条目（前 5）：");
                if let Some(items) = r["probe_items"].as_array() {
                    for it in items.iter().take(5) {
                        println!("  [{}] {} {}（{} 字）", it["layer"], it["key"],
                            it["title"].as_str().unwrap_or(""), it["chars"]);
                    }
                    if items.len() > 5 { println!("  … 共 {} 条", items.len()); }
                }
                if *dry_run { println!("\n[--dry-run] 未落盘清单文件（快照亦不写）"); }
                else { println!("\n清单已写: {}/07_勘误工坊/scan_probe_{}of{}.json",
                    lib_root.display(), s["n"], s["m"]); }
                if !*no_cache { println!("探针缓存: 命中 {} / 写入 {}", cache_hits, cache_miss); }
                if let Some(p) = &snapshot_written { println!("快照已写: {p}"); }
            }
        }
        WorldGovCmd::CheckOutline { volume, semantic, dry_run, enqueue, persist, lib: _ } => {
            if *enqueue {
                world_gov::validate_volume_name(volume).map_err(|e| anyhow::anyhow!("{e}"))?;
                let probe = format!("07_勘误工坊/outline_probe_{}.json",
                    volume.replace(|c: char| !c.is_ascii_alphanumeric(), "_"));
                return gov_enqueue(&conn, ne_agent::governance::pipeline::JobKind::CheckOutline,
                    None, Some(volume.clone()), probe, None, *persist, json,
                    &format!("check-outline volume={volume}"));
            }
            let r = world_gov::check_outline(&conn, lib_root, volume, *semantic, *dry_run)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                println!("== 卷细纲探针包（零模型骨架）==");
                println!("卷: {} | 定位到 L3 文件: {} 个", r["volume"], r["files_found"]);
                println!("semantic: {}", r["semantic"]);
                println!("上层法理基线: {} 条（摘要已附）",
                    r["upper_law_baseline"].as_array().map(|a| a.len()).unwrap_or(0));
                println!("评估三问: ①越界 ②自洽 ③L4支撑");
                if let Some(fs) = r["outline_probe_files"].as_array() {
                    for f in fs.iter().take(5) {
                        println!("  · {}（{} 字，头部读取: {}）", f["source"], f["chars"], f["head_present"]);
                    }
                }
                if *dry_run { println!("\n[--dry-run] 未落盘探针文件"); }
                else { println!("\n探针已写: {}/07_勘误工坊/outline_probe_{}.json（卷名已安全转写）",
                    lib_root.display(), volume.replace(|c: char| !c.is_ascii_alphanumeric(), "_")); }
            }
        }
        WorldGovCmd::SemanticVerify { seed, lib: _ } => {
            use ne_agent::governance::semantic_runtime as rt;
            // 只跑沙盘替身，不碰任何外部
            let resp = rt::deterministic_semantic_mock(*seed);
            // 序列化 → 反序列化（打通契约）
            let round = serde_json::from_str::<rt::SemanticResponse>(
                &serde_json::to_string(&resp)?)?;
            // findings → JSON → emit（走入库路由；OK/UNCERTAIN 不入库）
            let js = rt::findings_to_json(&round.findings);
            let n_risk = world_gov::emit_semantic_findings(&conn, "world-gov semantic-verify", &js)?;
            let cfg = rt::SemanticEndpointConfig::from_env();
            if json {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                    "command": "world-gov semantic-verify", "seed": seed,
                    "mock_note": resp.note, "findings": round.findings,
                    "risks_emitted": n_risk,
                    "endpoint": cfg.safe_snapshot(),
                    "note": "沙盘替身全链路；真实调用未实现",
                }))?);
            } else {
                println!("== semantic-verify（沙盘替身全链路）==");
                println!("seed={seed}  {}", resp.note);
                println!("序列化/反序列化往返：OK（{} 条 findings）", round.findings.len());
                for f in &round.findings {
                    println!("  [{}] {} conf={:.2} {}", f.verdict, f.item_id, f.confidence, f.note.clone().unwrap_or_default());
                }
                println!("入库路由：{} 条（OK/UNCERTAIN 按铁则不入库）", n_risk);
                println!("端点配置：{}", cfg.safe_snapshot());
            }
        }
        WorldGovCmd::PromptPreview { kind, key, probe_file, volume, export_probe, full, lib: _ } => {
            use ne_agent::governance::prompt_builder as pb;
            // ① 取探针 payload：优先文件；否则走真实探针产出（纯只读、零外呼）
            let payload: serde_json::Value = if let Some(pf) = probe_file {
                let raw = std::fs::read_to_string(pf)
                    .with_context(|| format!("读取探针文件失败: {}", pf.display()))?;
                serde_json::from_str(&raw)
                    .with_context(|| format!("探针文件不是合法 JSON: {}", pf.display()))?
            } else {
                match kind.as_str() {
                    "inspect-card" => {
                        let k = key.as_deref().ok_or_else(|| {
                            anyhow::anyhow!("inspect-card 预览需 --key 或 --probe-file")
                        })?;
                        let r = world_gov::inspect_card(&conn, lib_root, k)?;
                        if r["status"] == "NOT_FOUND" {
                            println!("未找到 key={k}（不做任何判定）");
                            return Ok(());
                        }
                        r["probe_pack"].clone()
                    }
                    "check-outline" => {
                        let v = volume.as_deref().unwrap_or("卷01");
                        world_gov::check_outline(&conn, lib_root, v, false, true)?
                    }
                    "scan-library" => anyhow::bail!(
                        "scan-library 预览请用 --probe-file 指向 scan_probe_*.json（或先落盘再预览）"
                    ),
                    other => anyhow::bail!(
                        "未知探针类型 '{other}'（inspect-card | scan-library | check-outline）"
                    ),
                }
            };
            // ② inspect-card 若传的是整包报告，卸出 probe_pack
            let payload = if kind == "inspect-card" {
                payload.get("probe_pack").cloned().unwrap_or(payload)
            } else {
                payload
            };
            // ③ 组装（纯规则层：零外呼、零推理、零落盘）
            let built = pb::build_by_kind(kind, &payload)?;
            // ③.5 人工注解（第 10 批）：预览时附带该条目已有标签/备注（只读，不改 prompt 正文）
            let ann = ne_agent::governance::note_tag_store::all_annotations(&conn).unwrap_or_default();
            let mut ann_keys: Vec<&String> = ann.keys().collect();
            ann_keys.sort_by(|a, b| b.len().cmp(&a.len()));
            // 参考文本：整个探针 JSON（含 key/正文引用），用于匹配注解 key
            let probe_text = serde_json::to_string(&payload).unwrap_or_default();
            let mut human_annotations: Vec<serde_json::Value> = Vec::new();
            for k in ann_keys {
                if probe_text.contains(k.as_str()) {
                    human_annotations.push(serde_json::json!({"key": k, "tags": ann[k]["tags"], "note": ann[k]["note"]}));
                }
            }
            // --export-probe DIR：把预览对应的探针 JSON 落盘（方便调试）
            if let Some(dir) = export_probe {
                std::fs::create_dir_all(dir)?;
                let safe_kind: String = kind.chars().map(|c| if c.is_ascii_alphanumeric() || c=='-' { c } else { '_' }).collect();
                let fp = dir.join(format!("probe_{safe_kind}.json"));
                std::fs::write(&fp, serde_json::to_string_pretty(&payload)?)?;
                if !quiet { eprintln!("[info] 探针已导出: {}", fp.display()); }
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                    "command": "world-gov prompt-preview",
                    "kind": kind,
                    "system_chars": built.system.chars().count(),
                    "user_chars": built.user.chars().count(),
                    "stats": built.stats,
                    "provenance": built.provenance_index(),
                    "system": built.system,
                    "user": if *full { built.user.clone() }
                            else { built.user.chars().take(800).collect::<String>() },
                    "human_annotations": human_annotations,
                    "note": "规则层预览：零外呼、不写库、不推理。human_annotations 仅展示人工标记，不注入 prompt 正文。",
                }))?);
            } else {
                println!("== prompt-preview（规则层预览；零外呼/零推理/零落盘）==");
                println!("类型: {kind}");
                if !human_annotations.is_empty() {
                    println!("── 人工标记（{} 条；仅展示，不注入 prompt）──", human_annotations.len());
                    for a in &human_annotations {
                        println!("  {} tags={} status={}",
                            a["key"].as_str().unwrap_or(""),
                            a["tags"], a["note"]["status"].as_str().unwrap_or("-"));
                    }
                }
                println!(
                    "片段数: {}  截断段: {}  user 总量: {} 字  总量降级: {}",
                    built.stats.section_count, built.stats.truncated_sections,
                    built.stats.total_chars, built.stats.truncated_total
                );
                println!(
                    "预算: 单段 ≤{} 字 / 总量 ≤{} 字",
                    built.stats.max_section_chars, built.stats.max_total_chars
                );
                println!("\n── system（{} 字）──\n{}", built.system.chars().count(), built.system);
                println!("── 溯源总表（{} 段）──", built.sections.len());
                for s in &built.sections {
                    println!(
                        "  [{}] {} ← {} @{}  {} 字{}",
                        s.layer, s.key, s.heading, s.found_in, s.text.chars().count(),
                        if s.truncated { "（已裁剪）" } else { "" }
                    );
                }
                println!(
                    "\n── user（{} 字{}）──",
                    built.user.chars().count(),
                    if *full { "" } else { "，仅示前 800 字" }
                );
                if *full {
                    println!("{}", built.user);
                } else {
                    println!("{}", built.user.chars().take(800).collect::<String>());
                }
            }
        }
        WorldGovCmd::AuditExport { cmd, lib: _ } => {
            use ne_agent::governance::report_builder as rb;
            let lf = rb::scan_layer_filter_text(false);
            match cmd {
                AuditExportCmd::New { format, dry_run, only_semantic, only_structural, law_hash, job_id, ignore_empty, with_tag, without_tag, snap } => {
                    let fmt = rb::ReportFormat::parse(format)?;
                    let filter = rb::ExportFilter {
                        only_semantic: *only_semantic,
                        only_structural: *only_structural,
                        law_hash: law_hash.clone(),
                        job_id: job_id.clone(),
                        with_tag: with_tag.clone(),
                        without_tag: without_tag.clone(),
                        snapshot_anchor: snap.clone(),
                    };
                    // 纯读组装（不落盘、不推理、不写库）
                    let rep = rb::build_report(&conn, lib_root, &lf, false, &filter)?;
                    // --ignore-empty：零风险时不报错（跳过空报告）
                    if *ignore_empty && rep.items.is_empty() && !*dry_run {
                        if json {
                            println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                                "command": "world-gov audit-export new", "skipped": true,
                                "reason": "无匹配风险（--ignore-empty：跳过空报告）",
                                "meta": rep.meta, "counts": rep.counts,
                            }))?);
                        } else {
                            println!("== audit-export new（--ignore-empty：无匹配风险，跳过）==");
                            println!("统计: {}", rep.counts);
                            println!("未写任何文件。");
                        }
                        return Ok(());
                    }
                    if *dry_run {
                        if json {
                            println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                                "command": "world-gov audit-export new", "dry_run": true,
                                "meta": rep.meta, "counts": rep.counts,
                                "items_exported": rep.items.len(),
                                "would_write_to": rb::lf_dir_display(lib_root),
                                "note": "只算不落地（reports/ 目录未创建）",
                            }))?);
                        } else {
                            println!("== audit-export new（--dry-run：只算不落地）==");
                            println!("run_id: {}", rep.meta.run_id);
                            println!("基线指纹: {}", rep.meta.baseline_fingerprint);
                            println!("统计: {}", rep.counts);
                            println!("将导出 {} 条 → {}", rep.items.len(), rb::lf_dir_display(lib_root));
                            println!("[--dry-run] 未写任何文件、未创建目录。");
                        }
                        return Ok(());
                    }
                    let path = rb::write_report(lib_root, &rep, fmt)?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                            "command": "world-gov audit-export new",
                            "format": format, "meta": rep.meta, "counts": rep.counts,
                            "items_exported": rep.items.len(),
                            "path": path.display().to_string(),
                        }))?);
                    } else {
                        println!("== audit-export new ==");
                        println!("格式: {}", format);
                        println!("run_id: {}", rep.meta.run_id);
                        println!("基线指纹: {}", rep.meta.baseline_fingerprint);
                        println!("统计: {}", rep.counts);
                        println!("已写: {}", path.display());
                    }
                }
                AuditExportCmd::List => {
                    let rows = rb::list_reports(lib_root)?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                            "command": "world-gov audit-export list",
                            "dir": rb::lf_dir_display(lib_root),
                            "count": rows.len(), "reports": rows,
                        }))?);
                    } else {
                        println!("== audit-export list（{} 份）==", rows.len());
                        println!("目录: {}", rb::lf_dir_display(lib_root));
                        for r in &rows {
                            println!("  {}  {} bytes  mtime={}", r["name"], r["size"], r["mtime"]);
                        }
                    }
                }
                AuditExportCmd::Show { run_id, format } => {
                    let fmt = rb::ReportFormat::parse(format)?;
                    let (p, content) = rb::read_report(lib_root, fmt.ext(), run_id)?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                            "command": "world-gov audit-export show",
                            "run_id": run_id, "path": p.display().to_string(),
                            "content": content,
                        }))?);
                    } else {
                        println!("== audit-export show: {} ==", p.display());
                        println!("{content}");
                    }
                }
            }
        }
        WorldGovCmd::InspectBatch { from_scan, id_list, since, slice, dry_run, list_keys, enqueue, persist, all_layers, with_tag, without_tag, real_semantic, lib: _ } => {
            use ne_agent::governance::inspect_batch as ib;
            use ne_agent::governance::progress;
            // 第 13 批：--real-semantic 是三重锁的命令行锁；未全开则明确拦截，绝不外呼。
            // 第 17 批：--dry-run 组合下只做**报文预览**（渲染 prompt/cache_key），连闸门都不需要开。
            let real_ok = if *real_semantic {
                let st = ne_agent::governance::llm_gate::lock_status(true);
                if !ne_agent::governance::llm_gate::real_llm_unlocked(true) && !*dry_run {
                    if json {
                        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                            "command": "world-gov inspect-batch",
                            "status": "REAL_SEMANTIC_LOCKED", "llm_gate": st,
                            "note": "三重锁未全开；本次未发出任何请求，未消耗 token。",
                        }))?);
                    } else {
                        println!("[三重锁拦截] inspect-batch --real-semantic 未解锁（缺以下条件）：");
                        if let Some(m) = st["missing"].as_array() {
                            for x in m { println!("  ✗ {}", x.as_str().unwrap_or("")); }
                        }
                        println!("本次未发出任何请求，未消耗 token。");
                    }
                    return Ok(());
                }
                if *dry_run {
                    if !quiet { eprintln!("[note] --dry-run：只渲染报文，不发送、不写缓存（无需开闸门）。"); }
                    true
                } else {
                    // 第 17.3 批：真实批量推理（逐卡走统一 semantic_pipeline；闸门已开）
                    if !quiet { eprintln!("[note] 三重锁已开；批量真实语义推理逐卡执行（每卡写 conflict_records）。"); }
                    true
                }
            } else {
                false
            };
            let _ = real_ok;
            // 分片解析（第 17 批）：--slice N/M 可选；给了就切
            let slice_nm: Option<(usize, usize)> = match slice.as_deref() {
                Some(s) => Some(ne_agent::world_gov::parse_slice_strict(s).map_err(|e| anyhow::anyhow!("{e}"))?),
                None => None,
            };
            // 三模式互斥；第 17 批：给了 --slice 而没给任何输入模式时，默认全库（all）
            let given = [from_scan.is_some(), id_list.is_some(), since.is_some()];
            let n_modes = given.iter().filter(|x| **x).count();
            if n_modes != 1 && !(n_modes == 0 && slice_nm.is_some()) {
                anyhow::bail!("inspect-batch 需且仅需一个输入模式：--from-scan | --id-list | --since（或仅给 --slice 走全库分片）");
            }
            let source = if let Some(p) = from_scan {
                ib::BatchSource::FromScan(p.clone())
            } else if let Some(p) = id_list {
                ib::BatchSource::IdList(p.clone())
            } else if let Some(v) = since {
                match ib::parse_since(v, lib_root)? {
                    (Some(ts), _) => ib::BatchSource::Since(ts),
                    (None, note) => {
                        if !quiet { eprintln!("[warn] {note}"); }
                        anyhow::bail!("inspect-batch --since 无有效锚点（无快照）；请给出时间戳或先 scan --write-snapshot");
                    }
                }
            } else {
                ib::BatchSource::All
            };
            progress::emit(quiet, "inspect-batch", 0, 1, None);
            let tag_filter = ne_agent::governance::note_tag_store::TagFilter::build(with_tag, without_tag)?;
            let report = ib::build_batch_report_sliced(&conn, lib_root, &source, *all_layers, *dry_run, Some(&tag_filter), slice_nm)?;
            let keys: Vec<String> = report["keys"].as_array().map(|a| {
                a.iter().filter_map(|v| v.as_str().map(String::from)).collect()
            }).unwrap_or_default();
            progress::emit(quiet, "inspect-batch", 1, 1, Some(0));
            // 【第 18.2 批】--list-keys：只输出 key 清单（每行一个，纯文本），不推理/不落库/不入队。
            // 供 shell 脚本逐卡消费（如受控试运行 scripts）。
            if *list_keys {
                for k in &keys { println!("{k}"); }
                return Ok(());
            }
            // 第 17 批：--dry-run + --real-semantic → 只渲染**报文预览**（不发送、不写缓存、不需闸门）。
            let mut preview: Option<serde_json::Value> = None;
            if *dry_run && *real_semantic {
                if let Some(k) = keys.first() {
                    use ne_agent::governance::{llm_config as lc, semantic_pipeline as sp};
                    let cfg = lc::load(lib_root)?;
                    let probe = serde_json::json!({"card_key": k});
                    let ph = sp::probe_hash(&probe);
                    let req = sp::PipelineRequest {
                        template: "check_world".into(),
                        vars: build_prompt_vars("check_world", &probe),
                        probe_hash: ph,
                        ..Default::default()
                    };
                    preview = Some(sp::preview(lib_root, &cfg, &req)?);
                }
            }
            // 入队（默认只预览；--enqueue 才入队；不自动 step）
            let mut enqueued = 0usize;
            if *enqueue && !*dry_run {
                for k in &keys {
                    let probe = format!("07_勘误工坊/inspect_{}.json(占位)", k.replace(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_', "_"));
                    gov_enqueue(&conn, ne_agent::governance::pipeline::JobKind::InspectCard,
                        None, None, probe, Some(k.as_str()), *persist, json,
                        &format!("inspect-batch key={k}"))?;
                    enqueued += 1;
                }
            }
            // 第 17.3 批：真实批量推理（--real-semantic 且非 --dry-run；逐卡走统一 semantic_pipeline）
            let mut infer: Vec<serde_json::Value> = Vec::new();
            if *real_semantic && !*dry_run {
                use ne_agent::governance::{conflict_store as cs, llm_config as ct, prompt_builder as pb, semantic_pipeline as sp};
                let cfg = ct::load(lib_root)?;
                let baseline_fp = world_gov::read_baseline_fingerprint(lib_root).unwrap_or_default();
                let snap_id = wg_latest_snapshot(&conn);
                for (i, k) in keys.iter().enumerate() {
                    progress::emit(quiet, "inspect-batch", i + 1, keys.len(), None);
                    let rec = world_gov::inspect_card(&conn, lib_root, k)?;
                    if rec["status"] == "NOT_FOUND" {
                        infer.push(serde_json::json!({"key": k, "status": "NOT_FOUND"}));
                        continue;
                    }
                    let built = pb::build_inspect_prompt(&rec["probe_pack"]);
                    let ph = wg_probe_hash(&rec["probe_pack"]);
                    let raw = sp::RawPrompt {
                        system: built.system.clone(), user: built.user.clone(), probe_hash: ph,
                        schema: pb::inspect_schema(),
                        entity_fields: vec!["referenced_ids".to_string()],
                        ..Default::default()
                    };
                    let outcome = sp::call_raw(&conn, lib_root, &cfg, true, &raw)?;
                    let oj = outcome.to_json();
                    let status = oj["status"].as_str().unwrap_or("?").to_string();
                    let mut conflict_id = None;
                    let mut parsed_verdict = serde_json::Value::Null;
                    let (content_opt, tok_in, tok_out) = match &outcome {
                        sp::Outcome::Success { content, token_in, token_out, .. } =>
                            (Some(content.clone()), *token_in, *token_out),
                        sp::Outcome::CacheHit { content, .. } => (Some(content.clone()), 0i64, 0i64),
                        _ => (None, 0i64, 0i64),
                    };
                    let is_cache_hit = matches!(outcome, sp::Outcome::CacheHit { .. });
                    let mut existing = None;
                    if is_cache_hit {
                        // 第 18.5 批：CacheHit 去重——已有同卡 pending 观点时复用，不重复 insert。
                        existing = cs::find_pending_by_probe(&conn, k)?;
                    }
                    if let Some(content) = content_opt {
                        // 宽容解析顶层 verdict（第 17.4 批统一契约）
                        let parsed = world_gov::parse_semantic_verdict(k, &content.to_string());
                        if parsed.get("verdict").is_some() {
                            match existing {
                                Some(eid) => { conflict_id = Some(eid); }
                                None => {
                                    conflict_id = Some(cs::insert(&conn, &cs::NewConflict {
                                        probe_key: k.clone(), snap_id: snap_id.clone(), baseline_fp: baseline_fp.clone(),
                                        verdict: parsed.clone(), raw_prompt: built.user.clone(),
                                        raw_response: content.to_string(),
                                        token_in: tok_in, token_out: tok_out,
                                    })?);
                                    let mut f = parsed.clone();
                                    if f.get("key").is_none() { f["key"] = serde_json::json!(k); }
                                    world_gov_emit(&conn, "inspect-batch", &[f])?;
                                }
                            }
                            parsed_verdict = parsed["verdict"].clone();
                        }
                    }
                    infer.push(serde_json::json!({
                        "key": k, "status": status, "conflict_id": conflict_id,
                        "verdict": parsed_verdict,
                    }));
                }
            }
            if json {
                let mut out = report.clone();
                out["enqueued"] = serde_json::json!(enqueued);
                if let Some(p) = &preview { out["llm_preview"] = p.clone(); }
                if !infer.is_empty() { out["inference"] = serde_json::json!(infer.clone()); }
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                println!("== inspect-batch（批量检视；{}；默认不推理）==", report["source_kind"].as_str().unwrap_or("-"));
                println!("批量条目数: {}", report["counts"]["batch_size"]);
                if let Some(s) = report["counts"]["slice"].as_object() {
                    println!("分片: {}/{}（切分前 {} 条）", s["n"], s["m"], s["selected_before_slice"]);
                }
                if report["counts"]["since_degraded"].as_bool().unwrap_or(false) {
                    println!("[warn] 增量降级：index 无有效 src_mtime（重跑 library-ctl init 可修复）");
                }
                println!("条目前 10：");
                for k in keys.iter().take(10) { println!("  - {k}"); }
                if keys.len() > 10 { println!("  … 共 {} 条", keys.len()); }
                if let Some(p) = &preview {
                    println!("── LLM 报文预览（--dry-run；只渲染不发送）──");
                    println!("模板: {}  后端: {}  模型: {}",
                        p["template"].as_str().unwrap_or("-"),
                        p["backend"].as_str().unwrap_or("-"),
                        p["model"].as_str().unwrap_or("-"));
                    println!("cache_key: {}", p["cache_key"].as_str().unwrap_or("-"));
                    println!("── system ──\n{}", p["system"].as_str().unwrap_or(""));
                    println!("── user ──\n{}", p["user"].as_str().unwrap_or(""));
                }
                if *dry_run {
                    println!("[--dry-run] 只算数量，未写任何任务/探针{}。",
                        if preview.is_some() { "，未发送、未写缓存" } else { "" });
                } else if !infer.is_empty() {
                    println!("── 真实语义推理（{} 条）──", infer.len());
                    let okn = infer.iter().filter(|x| x["status"] == "OK" || x["status"] == "CACHE_HIT").count();
                    println!("成功 {okn} / {}；其余看 --json 的 inference 段", infer.len());
                    for x in infer.iter().take(10) {
                        println!("  {:<28} [{}] verdict={} conflict={}",
                            x["key"].as_str().unwrap_or(""), x["status"].as_str().unwrap_or("-"),
                            x["verdict"].as_str().unwrap_or("-"),
                            x["conflict_id"].as_i64().map(|i| i.to_string()).unwrap_or_else(|| "-".into()));
                    }
                } else if *enqueue {
                    println!("已入队 {enqueued} 条（{}）；推进需手打 world-gov queue step（不自动跑）",
                        if *persist { "持久 task_jobs" } else { "内存队列" });
                } else {
                    println!("（预览模式：未入队。加 --enqueue 才入队。）");
                }
            }
        }
        WorldGovCmd::Cache { cmd, lib: _ } => {
            use ne_agent::governance::probe_cache as pc;
            match cmd {
                CacheCmd::Status { older_than } => {
                    let st = pc::status(lib_root, *older_than)?;
                    if json { println!("{}", serde_json::to_string_pretty(&st)?); }
                    else {
                        println!("== 探针缓存状态 ==");
                        println!("目录: {}", st["dir"]);
                        println!("条目数: {}  总大小: {} bytes", st["entries"], st["total_bytes"]);
                        println!("旧于 {} 天: {} 条", st["older_than_days"], st["stale_entries"]);
                    }
                }
                CacheCmd::Gc { older_than } => {
                    let n = pc::gc(lib_root, *older_than)?;
                    if json { println!("{}", serde_json::to_string_pretty(&serde_json::json!({"command":"world-gov cache gc","removed":n,"older_than":older_than}))?); }
                    else { println!("已清理 {n} 个超期缓存项（旧于 {older_than} 天）"); }
                }
                CacheCmd::Clear => {
                    let n = pc::clear(lib_root)?;
                    if json { println!("{}", serde_json::to_string_pretty(&serde_json::json!({"command":"world-gov cache clear","cleared":n}))?); }
                    else { println!("已清空 {n} 个缓存项（只动 07_勘误工坊/probe_cache/）"); }
                }
            }
        }
        WorldGovCmd::ProbeExport { cmd, lib: _ } => {
            use ne_agent::governance::probe_export as pe;
            match cmd {
                ProbeExportCmd::Create { out, only_layers, since, from_scan, job_id, with_tag, without_tag, dry_run } => {
                    // 收集探针来源：scan 探针文件 + 缓存项（world-gov 生成的结构化探针）
                    let tag_filter = ne_agent::governance::note_tag_store::TagFilter::build(with_tag, without_tag)?;
                    let tag_set = match tag_filter.is_empty() {
                        false => ne_agent::governance::note_tag_store::keys_matching(&conn, &tag_filter)?,
                        true => None,
                    };
                    let tag_ok = |key: &str| -> bool {
                        match &tag_set {
                            None => true,
                            Some(set) => set.contains(key),
                        }
                    };
                    let mut items: Vec<(String, serde_json::Value)> = Vec::new();
                    let mut tag_filtered = 0usize;
                    if let Some(src) = from_scan {
                        let p = if Path::new(src).is_absolute() { PathBuf::from(src) } else { lib_root.join(src) };
                        let raw = std::fs::read_to_string(&p).with_context(|| format!("读取 scan 探针失败: {}", p.display()))?;
                        let v: serde_json::Value = serde_json::from_str(&raw)?;
                        if let Some(arr) = v.get("probe_items").and_then(|x| x.as_array()) {
                            let lay_filter: Option<Vec<String>> = only_layers.as_ref().map(|s| s.split(',').map(|x| x.trim().to_string()).collect());
                            for it in arr {
                                let layer = it["layer"].as_str().unwrap_or("");
                                if let Some(lf) = &lay_filter { if !lf.iter().any(|x| x == layer) { continue; } }
                                let key = it["key"].as_str().unwrap_or("unknown");
                                if !tag_ok(key) { tag_filtered += 1; continue; }
                                let safe: String = key.chars().map(|c| if c.is_ascii_alphanumeric() || c=='-' || c=='_' { c } else { '_' }).collect();
                                items.push((format!("probe-{safe}.json"), it.clone()));
                            }
                        }
                    }
                    // 缓存项（可选，按 --since / --job-id 过滤在缓存侧无 job 概念，故仅按层）
                    let cache_dir = ne_agent::governance::probe_cache::cache_dir(lib_root);
                    if cache_dir.is_dir() {
                        for e in std::fs::read_dir(&cache_dir)? {
                            let e = e?; let p = e.path();
                            if p.extension().map(|x| x == "json").unwrap_or(false) {
                                if let Ok(txt) = std::fs::read_to_string(&p) {
                                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
                                        let ckey = v["key"].as_str().unwrap_or("");
                                        if !ckey.is_empty() && !tag_ok(ckey) { tag_filtered += 1; continue; }
                                        items.push((format!("probe-cache-{}.json", p.file_stem().unwrap().to_string_lossy()), v));
                                    }
                                }
                            }
                        }
                    }
                    let _ = (since, job_id); // 保留参数语义；缓存侧无 mtime/job 元数据过滤（暂无）
                    let manifest = serde_json::json!({
                        "version": pe::WG_PROBE_VERSION,
                        "created_at": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
                        "toolchain_epoch": 12,
                        "layers": only_layers.clone().unwrap_or_else(|| "all".into()),
                        "since_anchor": since,
                        "baseline_fingerprint": world_gov::read_baseline_fingerprint(lib_root).unwrap_or_else(|| "(none)".into()),
                        "item_count": items.len(),
                        "tag_filter": tag_filter.describe(),
                        "tag_filtered": tag_filtered,
                        "entries": items.iter().map(|(n,_)| n.clone()).collect::<Vec<_>>(),
                        "note": "只含 world-gov 生成的结构化探针；不含资料库原始源文件。",
                    });
                    if *dry_run {
                        if json { println!("{}", serde_json::to_string_pretty(&serde_json::json!({"command":"world-gov probe-export create","dry_run":true,"item_count":items.len(),"manifest":manifest}))?); }
                        else { println!("== probe-export create（--dry-run）==\n将打包 {} 个探针（未生成包）", items.len()); }
                        return Ok(());
                    }
                    // 输出路径白名单：默认 07_勘误工坊/；显式 --out 需在 lib_root 内或绝对路径
                    let out_path = match out {
                        Some(p) => {
                            if p.is_absolute() { p.clone() }
                            else {
                                // 防逃逸：相对路径不许含 ..
                                if p.to_string_lossy().contains("..") { anyhow::bail!("--out 相对路径不得含 .."); }
                                lib_root.join("07_勘误工坊").join(p)
                            }
                        }
                        None => {
                            let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
                            lib_root.join("07_勘误工坊").join(format!("probes_{ts}.wg-probe.tar.gz"))
                        }
                    };
                    let (_p, nbytes) = pe::create_archive(&out_path, &manifest, &items)?;
                    if json { println!("{}", serde_json::to_string_pretty(&serde_json::json!({"command":"world-gov probe-export create","path":out_path.display().to_string(),"bytes":nbytes,"item_count":items.len()}))?); }
                    else { println!("== probe-export create ==\n已写: {}（{} bytes，{} 探针）", out_path.display(), nbytes, items.len()); }
                }
                ProbeExportCmd::List { archive } => {
                    let p = if archive.is_absolute() { archive.clone() } else { lib_root.join(archive) };
                    let m = pe::read_manifest(&p)?;
                    if json { println!("{}", serde_json::to_string_pretty(&serde_json::json!({"command":"world-gov probe-export list","archive":p.display().to_string(),"manifest":m}))?); }
                    else {
                        println!("== probe-export list: {} ==", p.display());
                        println!("版本: {}  条目数: {}", m["version"], m["item_count"]);
                        println!("基线指纹: {}", m["baseline_fingerprint"]);
                        println!("层过滤: {}  增量锚点: {}", m["layers"], m["since_anchor"]);
                        if let Some(e) = m["entries"].as_array() { for x in e.iter().take(20) { println!("  - {}", x.as_str().unwrap_or("")); } }
                    }
                }
                ProbeExportCmd::Unpack { archive, to } => {
                    let p = if archive.is_absolute() { archive.clone() } else { lib_root.join(archive) };
                    let dst = match to {
                        Some(d) => if d.is_absolute() { d.clone() } else { lib_root.join("07_勘误工坊").join(d) },
                        None => lib_root.join("07_勘误工坊/probe_unpack"),
                    };
                    let n = pe::unpack_archive(&p, &dst)?;
                    if json { println!("{}", serde_json::to_string_pretty(&serde_json::json!({"command":"world-gov probe-export unpack","archive":p.display().to_string(),"to":dst.display().to_string(),"files":n}))?); }
                    else { println!("== probe-export unpack ==\n已解包 {} 个文件到 {}", n, dst.display()); }
                }
            }
        }
        WorldGovCmd::SelfCheck { cmd, lib: _ } => {
            use ne_agent::governance::self_check as sc;
            match cmd {
                SelfCheckCmd::Run { fix, older_than } => {
                    let r = sc::run(&conn, lib_root, *fix, *older_than)?;
                    if json { println!("{}", serde_json::to_string_pretty(&r)?); }
                    else {
                        println!("== world-gov self-check（{} 项检查，{} 失败）==", r["checks"], r["failed"]);
                        if let Some(items) = r["items"].as_array() {
                            for it in items {
                                let mark = if it["ok"] == true { if it["warn"].as_bool().unwrap_or(false) { "⚠" } else { "✅" } } else { "❌" };
                                println!("  {mark} {} : {}", it["name"], it["detail"]);
                            }
                        }
                        if *fix { println!("（--fix：只创建缺失目录，未 ALTER 表/未删任务）"); }
                    }
                }
            }
        }
        WorldGovCmd::Tag { cmd, lib: _ } => {
            use ne_agent::governance::note_tag_store as nts;
            match cmd {
                TagCmd::Add { key, tags } => {
                    let (ins, skip) = nts::add_tags(&conn, key, tags, "manual")?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                            "command": "world-gov tag add", "key": key,
                            "inserted": ins, "skipped_existing": skip, "source": "manual",
                        }))?);
                    } else {
                        println!("== tag add: {key} ==\n新增 {ins} 个标签，跳过已存在 {skip} 个");
                    }
                }
                TagCmd::Rm { key, tag } => {
                    let n = nts::rm_tag(&conn, key, tag)?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                            "command": "world-gov tag rm", "key": key, "tag": tag, "removed": n,
                        }))?);
                    } else if n == 0 {
                        println!("未找到 {key} 的标签 '{tag}'（无改动）");
                    } else {
                        println!("已删除 {key} 的标签 '{tag}'");
                    }
                }
                TagCmd::List { key, tag } => {
                    let rows = nts::list_tags(&conn, key.as_deref(), tag.as_deref())?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                            "command": "world-gov tag list", "count": rows.len(), "tags": rows,
                        }))?);
                    } else {
                        println!("== tag list（{} 条）==", rows.len());
                        for r in &rows {
                            println!("  {}  [{}]  source={}",
                                r["key"].as_str().unwrap_or(""),
                                r["tag"].as_str().unwrap_or(""),
                                r["source"].as_str().unwrap_or(""));
                        }
                    }
                }
                TagCmd::Filter { tag } => {
                    let f = nts::TagFilter::build(std::slice::from_ref(tag), &[])?;
                    let set = nts::keys_matching(&conn, &f)?.unwrap_or_default();
                    let mut keys: Vec<String> = set.into_iter().collect();
                    keys.sort();
                    if json {
                        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                            "command": "world-gov tag filter", "tag": tag,
                            "count": keys.len(), "keys": keys,
                        }))?);
                    } else {
                        // 纯行输出：可 `| xargs` / 管道给其他命令
                        for k in &keys { println!("{k}"); }
                    }
                }
                TagCmd::Export { to } => {
                    let dump = nts::export_tags(&conn)?;
                    // 路径防护：拒绝含 .. 或指向目录
                    let p = safe_export_path(lib_root, to)?;
                    if let Some(parent) = p.parent() { std::fs::create_dir_all(parent)?; }
                    std::fs::write(&p, serde_json::to_string_pretty(&dump)?)?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                            "command": "world-gov tag export", "path": p.display().to_string(),
                            "count": dump["count"],
                        }))?);
                    } else {
                        println!("== tag export ==\n已写: {}（{} 条标签）", p.display(), dump["count"]);
                    }
                }
                TagCmd::Import { path, dry_run, force } => {
                    let raw = std::fs::read_to_string(path)
                        .with_context(|| format!("读取导入文件失败: {}", path.display()))?;
                    let r = nts::import_tags(&conn, &raw, *dry_run, *force)?;
                    if json { println!("{}", serde_json::to_string_pretty(&r)?); }
                    else {
                        println!("== tag import{} ==", if *dry_run { "（--dry-run）" } else { "" });
                        println!("文件 {} 条 → 写入 {}，跳过 {}，拒绝 {}",
                            r["total_in_file"], r["inserted"], r["skipped_existing"],
                            r["rejected"].as_array().map(|a| a.len()).unwrap_or(0));
                        if let Some(rej) = r["rejected"].as_array() {
                            for x in rej.iter().take(10) { println!("  [拒] {}", x.as_str().unwrap_or("")); }
                        }
                    }
                }
            }
        }
        WorldGovCmd::Note { cmd, lib: _ } => {
            use ne_agent::governance::note_tag_store as nts;
            match cmd {
                NoteCmd::Set { key, status, content } => {
                    let r = nts::set_note(&conn, key, status.as_deref(), content.as_deref())?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                            "command": "world-gov note set", "key": key, "note": r,
                        }))?);
                    } else {
                        println!("== note set: {key} ==\n状态: {}  内容: {} 字",
                            r["status"].as_str().unwrap_or(""),
                            r["content"].as_str().map(|s| s.chars().count()).unwrap_or(0));
                    }
                }
                NoteCmd::Get { key } => {
                    match nts::get_note(&conn, key)? {
                        Some(n) => {
                            if json {
                                println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                                    "command": "world-gov note get", "key": key, "found": true, "note": n,
                                }))?);
                            } else {
                                println!("== note get: {key} ==");
                                println!("状态: {}", n["status"].as_str().unwrap_or(""));
                                println!("内容:\n{}", n["content"].as_str().unwrap_or(""));
                            }
                        }
                        None => {
                            if json {
                                println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                                    "command": "world-gov note get", "key": key, "found": false,
                                }))?);
                            } else {
                                println!("{key} 无备注");
                            }
                        }
                    }
                }
                NoteCmd::List { status } => {
                    let rows = nts::list_notes(&conn, status.as_deref())?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                            "command": "world-gov note list", "count": rows.len(), "notes": rows,
                        }))?);
                    } else {
                        println!("== note list（{} 条）==", rows.len());
                        for r in &rows {
                            let c = r["content"].as_str().unwrap_or("");
                            let shown: String = c.chars().take(40).collect();
                            println!("  {}  [{}]  {}", r["key"].as_str().unwrap_or(""),
                                r["status"].as_str().unwrap_or(""), shown);
                        }
                    }
                }
                NoteCmd::Export { to } => {
                    let dump = nts::export_notes(&conn)?;
                    let p = safe_export_path(lib_root, to)?;
                    if let Some(parent) = p.parent() { std::fs::create_dir_all(parent)?; }
                    std::fs::write(&p, serde_json::to_string_pretty(&dump)?)?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                            "command": "world-gov note export", "path": p.display().to_string(),
                            "count": dump["count"],
                        }))?);
                    } else {
                        println!("== note export ==\n已写: {}（{} 条备注）", p.display(), dump["count"]);
                    }
                }
                NoteCmd::Import { path, dry_run, force } => {
                    let raw = std::fs::read_to_string(path)
                        .with_context(|| format!("读取导入文件失败: {}", path.display()))?;
                    let r = nts::import_notes(&conn, &raw, *dry_run, *force)?;
                    if json { println!("{}", serde_json::to_string_pretty(&r)?); }
                    else {
                        println!("== note import{} ==", if *dry_run { "（--dry-run）" } else { "" });
                        println!("文件 {} 条 → 新增 {}，更新 {}，跳过 {}，拒绝 {}",
                            r["total_in_file"], r["inserted"], r["updated"], r["skipped_existing"],
                            r["rejected"].as_array().map(|a| a.len()).unwrap_or(0));
                        if let Some(rej) = r["rejected"].as_array() {
                            for x in rej.iter().take(10) { println!("  [拒] {}", x.as_str().unwrap_or("")); }
                        }
                    }
                }
            }
        }
        WorldGovCmd::Diff { cmd, lib: _ } => {
            use ne_agent::governance::diff_snapshot as ds;
            match cmd {
                DiffCmd::Snap { cmd } => match cmd {
                    DiffSnapCmd::Create { label } => {
                        let lab = label.clone().unwrap_or_default();
                        let (sid, n) = ds::create(&conn, lib_root, &lab)?;
                        if json {
                            println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                                "command": "world-gov diff snap create",
                                "snap_id": sid, "label": if lab.is_empty() { "(auto)" } else { &lab },
                                "entry_count": n,
                                "note": "逻辑快照：只存索引+元信息哈希，未复制任何正文。",
                            }))?);
                        } else {
                            println!("== diff snap create ==\n快照 id: {sid}\n条目数: {n}（只存哈希，未复制正文）");
                        }
                    }
                    DiffSnapCmd::List => {
                        let rows = ds::list(&conn)?;
                        if json {
                            println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                                "command": "world-gov diff snap list", "count": rows.len(), "snapshots": rows,
                            }))?);
                        } else {
                            println!("== diff snap list（{} 个）==", rows.len());
                            for r in &rows {
                                println!("  {}  {}  ts={}  entries_bytes={}",
                                    r["snap_id"].as_str().unwrap_or(""),
                                    r["label"].as_str().unwrap_or(""),
                                    r["created_ts"], r["meta_bytes"]);
                            }
                        }
                    }
                    DiffSnapCmd::Delete { snap_id } => {
                        let ok = ds::delete(&conn, snap_id)?;
                        if json {
                            println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                                "command": "world-gov diff snap delete", "snap_id": snap_id, "deleted": ok,
                            }))?);
                        } else if ok {
                            println!("已删除快照 {snap_id}（只删 lib_snapshot 行，不动业务数据）");
                        } else {
                            println!("未找到快照 {snap_id}（无改动）");
                        }
                    }
                    DiffSnapCmd::Export { snap_id, to } => {
                        use ne_agent::governance::probe_export as pe;
                        let meta = ds::read_meta(&conn, snap_id)?
                            .ok_or_else(|| anyhow::anyhow!("快照不存在：{snap_id}"))?;
                        let dir = safe_export_path(lib_root, to)?;
                        std::fs::create_dir_all(&dir)?;
                        // 快照对应探针集合：缓存内有则收，否则退化为元信息清单
                        let cache_dir = ne_agent::governance::probe_cache::cache_dir(lib_root);
                        let keys: std::collections::HashSet<String> = meta.iter().map(|m| m.key.clone()).collect();
                        let mut items: Vec<(String, serde_json::Value)> = Vec::new();
                        if cache_dir.is_dir() {
                            for e in std::fs::read_dir(&cache_dir)? {
                                let e = e?; let p = e.path();
                                if !p.extension().map(|x| x == "json").unwrap_or(false) { continue; }
                                if let Ok(txt) = std::fs::read_to_string(&p) {
                                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
                                        let k = v["key"].as_str().unwrap_or("");
                                        if keys.contains(k) {
                                            let safe: String = k.chars().map(|c| if c.is_ascii_alphanumeric() || c=='-' || c=='_' { c } else { '_' }).collect();
                                            items.push((format!("probe-{safe}.json"), v));
                                        }
                                    }
                                }
                            }
                        }
                        let manifest = serde_json::json!({
                            "version": pe::WG_PROBE_VERSION,
                            "kind": "snapshot-probe-set",
                            "snap_id": snap_id,
                            "snapshot_entry_count": meta.len(),
                            "probe_items_found": items.len(),
                            "note": "该快照对应探针集合（命中缓存中的结构化探针）；不含资料库原始源文件。",
                        });
                        let out = dir.join(format!("snap_{}.wg-probe.tar.gz", &snap_id[..snap_id.len().min(12)]));
                        let (_p, nbytes) = pe::create_archive(&out, &manifest, &items)?;
                        if json {
                            println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                                "command": "world-gov diff snap export",
                                "snap_id": snap_id, "path": out.display().to_string(),
                                "bytes": nbytes, "item_count": items.len(),
                            }))?);
                        } else {
                            println!("== diff snap export ==\n已写: {}（{} bytes，{} 探针）", out.display(), nbytes, items.len());
                        }
                    }
                },
                DiffCmd::Show { snap_a, snap_b } => {
                    let r = ds::diff_snapshots(&conn, snap_a, snap_b)?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&r)?);
                    } else {
                        println!("== diff show: {} → {} ==", snap_a, snap_b);
                        println!("added: {}  removed: {}  modified: {}",
                            r["counts"]["added"], r["counts"]["removed"], r["counts"]["modified"]);
                        for k in r["added"].as_array().map(|a| a.to_vec()).unwrap_or_default() {
                            println!("  + {}", k.as_str().unwrap_or(""));
                        }
                        for k in r["removed"].as_array().map(|a| a.to_vec()).unwrap_or_default() {
                            println!("  - {}", k.as_str().unwrap_or(""));
                        }
                        for m in r["modified"].as_array().map(|a| a.to_vec()).unwrap_or_default() {
                            println!("  ~ {}  {} → {}", m["key"].as_str().unwrap_or(""),
                                &m["hash_a"].as_str().unwrap_or("")[..8.min(m["hash_a"].as_str().unwrap_or("").len())],
                                &m["hash_b"].as_str().unwrap_or("")[..8.min(m["hash_b"].as_str().unwrap_or("").len())]);
                        }
                    }
                }
                DiffCmd::FilterChanged { since_snap, auto_tag_changed, tag } => {
                    let cur = ds::collect_meta(&conn, lib_root)?;
                    let changed = ds::changed_since(&conn, since_snap, &cur)?;
                    let mut tagged = 0usize;
                    if *auto_tag_changed && !changed.is_empty() {
                        use ne_agent::governance::note_tag_store as nts;
                        for k in &changed {
                            let (ins, _) = nts::add_tags(&conn, k, std::slice::from_ref(tag), "manual")?;
                            if ins > 0 { tagged += 1; }
                        }
                    }
                    if json {
                        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                            "command": "world-gov diff filter-changed",
                            "since_snap": since_snap, "count": changed.len(), "keys": changed,
                            "auto_tag_changed": auto_tag_changed, "tag": tag, "tagged": tagged,
                        }))?);
                    } else {
                        // 纯行输出：可管道进 scan/inspect-batch
                        for k in &changed { println!("{k}"); }
                        if *auto_tag_changed {
                            eprintln!("已给 {} 个改动条目打标签「{}」（仅打标签，不做任何自动校验）", tagged, tag);
                        }
                    }
                }
            }
        }
        WorldGovCmd::Web { cmd, lib: _ } => {
            match cmd {
                WebCmd::Serve { port, no_open, health_check_only } => {
                    let url = format!("http://127.0.0.1:{port}");
                    if *health_check_only {
                        // 【规约适配】只做一次健康探测后立即退出，不常驻。
                        // 复用 web_gateway 的路由做**进程内**探测（不起常驻服务、不占端口）。
                        let conn = ne_agent::governance::open_gov(lib_root)?;
                        let (code, _body) = ne_agent::governance::web_gateway::route_for_test(&conn, lib_root, "GET", "/");
                        let ok = code == 200;
                        if json {
                            println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                                "command": "world-gov web serve --health-check-only",
                                "url": url, "bind": "127.0.0.1", "health_ok": ok, "http_status": code,
                                "note": "仅进程内路由探测；未起常驻服务、未占端口。",
                            }))?);
                        } else {
                            println!("== web serve --health-check-only ==");
                            println!("面板健康: {}（GET / → HTTP {}）", if ok { "✅ 可用" } else { "❌ 异常" }, code);
                            println!("（仅进程内探测；未起常驻服务、未占端口）");
                        }
                        return Ok(());
                    }
                    if json {
                        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                            "command": "world-gov web serve",
                            "url": url,
                            "bind": "127.0.0.1",
                            "no_open": no_open,
                            "note": "仅本机可访问；前台阻塞；Ctrl-C 退出，不留后台进程。",
                        }))?);
                    } else {
                        println!("world-gov web 面板：{url}（仅 127.0.0.1；Ctrl-C 退出）");
                        println!("（--no-open 生效：本环境不自动打开浏览器）");
                    }
                    // 阻塞前提示（emit 到 stderr，保持 stdout 纯净）
                    ne_agent::governance::structured_log::info(
                        "world-gov web",
                        "serve.start",
                        serde_json::json!({"bind": "127.0.0.1", "port": port}),
                    );
                    ne_agent::governance::web_gateway::serve(lib_root, *port, quiet)?;
                }
            }
        }
        WorldGovCmd::Queue { cmd } => {
            use ne_agent::governance::pipeline as pipe;
            use ne_agent::governance::pipeline_store as store;
            match cmd {
                QueueCmd::List => {
                    // 持久层可用则一并展示（幂等 ensure，不写任务）
                    let mut persisted = Vec::new();
                    if store::ensure_task_table(&conn).is_ok() {
                        persisted = store::job_list(&conn, None).unwrap_or_default()
                            .iter().map(|j| serde_json::json!({
                                "id": j.id, "kind": j.kind.as_str(), "state": j.state.as_str(),
                                "slice": j.slice.map(|(n, m)| format!("{n}/{m}")),
                                "volume": j.volume, "probe_path": j.probe_path,
                            })).collect();
                    }
                    let rows: Vec<serde_json::Value> = pipe::with_queue(|q| {
                        q.list().iter().map(|j| serde_json::json!({
                            "id": j.id, "kind": j.kind.as_str(), "state": j.state.as_str(),
                            "slice": j.slice.map(|(n, m)| format!("{n}/{m}")),
                            "volume": j.volume, "probe_path": j.probe_path,
                        })).collect()
                    });
                    if json {
                        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                            "memory_count": rows.len(), "memory_jobs": rows,
                            "persisted_count": persisted.len(), "persisted_jobs": persisted,
                            "note": "memory=进程内存（退出即清空）；persisted=novel.db.task_jobs（--persist 写入）",
                        }))?);
                    } else {
                        println!("== 内存队列（{} 个；进程内存，退出即清空）==", rows.len());
                        for j in &rows {
                            println!("  {} [{}] {} slice={} vol={}",
                                j["id"], j["state"], j["kind"], j["slice"], j["volume"].as_str().unwrap_or("-"));
                        }
                        println!("== 持久队列 task_jobs（{} 个）==", persisted.len());
                        for j in &persisted {
                            println!("  {} [{}] {} slice={} vol={}",
                                j["id"], j["state"], j["kind"], j["slice"], j["volume"].as_str().unwrap_or("-"));
                        }
                    }
                }
                QueueCmd::Clear => {
                    let n = pipe::with_queue(|q| q.clear());
                    let cleared_db = if store::ensure_task_table(&conn).is_ok() {
                        let all = store::job_list(&conn, None).unwrap_or_default();
                        let mut c = 0;
                        for j in all { if store::job_delete(&conn, &j.id).unwrap_or(false) { c += 1; } }
                        c
                    } else { 0 };
                    if json {
                        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                            "cleared_memory": n, "cleared_persisted": cleared_db,
                        }))?);
                    } else {
                        println!("已清空：内存队列 {} 个 + 持久任务 {} 个（仅内存 + task_jobs，磁盘库/正文未动）", n, cleared_db);
                    }
                }
                QueueCmd::Peek { id } => {
                    // 先查持久层（跨进程可见），再查内存
                    let pj = store::ensure_task_table(&conn).ok().and_then(|_| store::job_load(&conn, id).ok().flatten());
                    let job = pj.or_else(|| pipe::with_queue(|q| q.peek(id).cloned()));
                    match job {
                        Some(j) => {
                            if json {
                                println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                                    "id": j.id, "kind": j.kind.as_str(), "state": j.state.as_str(),
                                    "slice": j.slice.map(|(n, m)| format!("{n}/{m}")),
                                    "volume": j.volume, "probe_path": j.probe_path, "created": j.created,
                                }))?);
                            } else {
                                println!("任务 {}｜{}｜状态 {}｜探针路径 {}", j.id, j.kind.as_str(), j.state.as_str(), j.probe_path);
                            }
                        }
                        None => {
                            if json {
                                println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                                    "id": id, "status": "NOT_FOUND",
                                    "note": "内存队列跨进程不保留；持久任务需 --persist 写入后可见。",
                                }))?);
                            } else {
                                println!("未找到任务 {id}（内存队列跨进程不保留；持久任务需 --persist 写入）");
                            }
                        }
                    }
                }
                QueueCmd::Step { limit, max_per_round, persist, prune, dump_stuck } => {
                    // 仅对**持久任务**推进（一步一推；不自动轮询）
                    store::ensure_task_table(&conn)?;
                    if *dump_stuck {
                        let stuck = ne_agent::governance::self_check::stalled_jobs(&conn, 24)?;
                        if json {
                            println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                                "command": "world-gov queue step --dump-stuck",
                                "threshold_hours": 24, "count": stuck.len(), "stuck": stuck,
                            }))?);
                        } else {
                            println!("== 长期 Pending（>24h）任务：{} 个 ==", stuck.len());
                            for s in &stuck {
                                println!("  {} [{}] created_at={}", s["id"], s["kind"], s["created_at"]);
                            }
                        }
                        return Ok(());
                    }
                    if *prune {
                        let removed = store::job_prune(&conn, 7)?;
                        if json {
                            println!("{}", serde_json::to_string_pretty(&serde_json::json!({"pruned": removed}))?);
                        } else {
                            println!("已清理 {} 个超过 7 天的旧任务（只动 task_jobs）", removed);
                        }
                        return Ok(());
                    }
                    let _ = persist;
                    // 门控收口：所有推理外呼必经 get_semantic_gate()；沙盘替身也不绕门控
                    let gate_open = get_semantic_gate(false);
                    // 取**任一可推进任务**（Pending/Probing/ReadyForInferring），按创建序；
                    // 每个任务本次**只前进一步**（不一口气跑到终态），limit 限制本次任务数。
                    let advanceable: Vec<_> = store::job_list(&conn, None)?
                        .into_iter()
                        .filter(|j| matches!(
                            j.state,
                            ne_agent::governance::pipeline::JobState::Pending
                                | ne_agent::governance::pipeline::JobState::Probing
                                | ne_agent::governance::pipeline::JobState::ReadyForInferring
                        ))
                        .collect();
                    if advanceable.is_empty() {
                        if json {
                            println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                                "stepped": 0, "note": "无可推进任务（全部 Done/Rejected 或队列为空）",
                            }))?);
                        } else {
                            println!("无可推进任务（队列为空或全部终态）");
                        }
                        return Ok(());
                    }
                    let take = (*limit).max(1).min((*max_per_round).max(1));
                    if !quiet { eprintln!("[info] queue step: 本轮上限 max_per_round={max_per_round}，实际推进 {take} 个"); }
                    let mut results = Vec::new();
                    let total_take = take;
                    let mut done = 0usize;
                    for j in advanceable.into_iter().take(take) {
                        // 进度：粗估（stderr；--quiet 静默）
                        ne_agent::governance::progress::emit(quiet, "queue step", done, total_take, None);
                        // dry_run=true：ReadyForInferring 步走沙盘替身（不碰外部）。
                        // 门控未开时，run_semantic_infer 仍返回 GateLocked → 任务 Rejected。
                        let (st, findings, fail) = store::job_step_forward(&conn, &j.id, gate_open, true)?;
                        let n_risk = if findings.is_empty() {
                            0
                        } else {
                            world_gov::emit_semantic_findings(&conn, "world-gov queue step", &findings)?
                        };
                        // 推理失败（非内容语义判定）→ 可选写一条**结构性**风险
                        if let Some(reason) = &fail {
                            let _ = ne_agent::governance::record_risk(
                                &conn, "pipeline", None, None, "world-gov queue step", "WARN",
                                &format!("任务 {} 推理失败：{reason}（结构性，非内容语义判定）", j.id),
                            );
                        }
                        results.push(serde_json::json!({
                            "job_id": j.id, "from": j.state.as_str(), "to": st.as_str(),
                            "findings": findings.len(), "risks_emitted": n_risk,
                            "fail": fail,
                        }));
                        done += 1;
                    }
                    ne_agent::governance::progress::emit(quiet, "queue step", done, total_take, Some(0));
                    if json {
                        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                            "stepped": results.len(), "gate_open": gate_open, "results": results,
                            "note": "每个任务本次只前进一步；需多次调用逐级推进 Pending→Probing→Ready→Done",
                        }))?);
                    } else {
                        println!("== queue step（推进 {} 个任务，各一步；gate_open={gate_open}）==", results.len());
                        for r in &results {
                            let f = r["fail"].as_str().map(|s| format!("  ⚠ {s}")).unwrap_or_default();
                            println!("  {} {} → {}（findings {}，入库 {}）{f}",
                                r["job_id"], r["from"], r["to"], r["findings"], r["risks_emitted"]);
                        }
                        println!("（沙盘替身；真实推理等放行令；step 不自动轮询、不一口气跑完）");
                    }
                }
            }
        }
    }
    Ok(())
}
fn run_compliance(vault: &Vault, lib_root: &Path, from: u32, to: Option<u32>, volume: &str, semantic: bool, json: bool) -> Result<()> {
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
