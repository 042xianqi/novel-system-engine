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
use clap::{Parser, Subcommand};
use ne_agent::{RuntimeConfig, Vault};
use std::path::{Path, PathBuf};

#[derive(Parser, Debug)]
#[command(
    name = "ne-agent",
    about = "《残响之河》Agent Runtime —— 单二进制智能体",
    version,
    long_about = None
)]
struct Cli {
    /// vault 目录（默认：当前目录下的 .neagent，或 $NE_AGENT_VAULT）
    #[arg(long, global = true, env = "NE_AGENT_VAULT", value_name = "DIR")]
    vault: Option<PathBuf>,

    /// 输出 JSON（便于脚本消费）
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// 管理项目隔离库（vault）
    Vault {
        #[command(subcommand)]
        cmd: VaultCmd,
    },
    /// 查看 / 校验模型配置
    Models {
        #[command(subcommand)]
        cmd: ModelsCmd,
    },
    /// 交互式对话
    Chat {
        /// 单次提问（非交互，直接打印回复后退出）
        #[arg(short, long)]
        message: Option<String>,
        /// 恢复最近一次会话
        #[arg(long)]
        resume: bool,
        /// 指定会话 id
        #[arg(long, value_name = "ID")]
        session: Option<String>,
        /// 新建会话
        #[arg(long)]
        new: bool,
    },
    /// 管理会话
    Session {
        #[command(subcommand)]
        cmd: SessionCmd,
    },
    /// 列出内置工具
    Tools,
    /// 管理长期记忆（pinned 常驻 / recall 按需）
    Memory {
        #[command(subcommand)]
        cmd: MemoryCmd,
    },
    /// 生成一章正文（细纲 → 注入包 → 模型 → 门禁）
    Novel {
        /// 章号（第几章）
        #[arg(long, default_value_t = 1)]
        chap_id: u32,
        /// 卷号
        #[arg(long, default_value = "卷01")]
        volume: String,
        /// 资料库根（默认：vault 的 workspace 或 $NE_AGENT_LIB）
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
        /// 只生成不落盘
        #[arg(long)]
        dry_run: bool,
        /// 只组装 prompt，不调模型（调试用；打印 prompt 长度与前 500 字）
        #[arg(long)]
        prompt_only: bool,
    },
}

#[derive(Subcommand, Debug)]
enum MemoryCmd {
    /// 列出记忆
    List {
        /// 只看 pinned
        #[arg(long)]
        pinned: bool,
    },
    /// 保存一条记忆（同 scope+owner+key 覆盖）
    Save {
        /// 键
        key: String,
        /// 值
        value: String,
        /// 作用域：global | project | session
        #[arg(long, default_value = "project")]
        scope: String,
        /// 类型：preference | rule | fact | project_info
        #[arg(long, default_value = "fact")]
        kind: String,
        /// 钉选到 system prompt 稳定前缀（始终注入）
        #[arg(long)]
        pinned: bool,
    },
    /// 删除一条记忆
    Forget {
        key: String,
        #[arg(long, default_value = "project")]
        scope: String,
    },
    /// 试跑召回：给一段查询，看会召回哪些（不发模型请求）
    Recall {
        /// 查询文本
        query: String,
    },
}

#[derive(Subcommand, Debug)]
enum SessionCmd {
    /// 列出会话
    List,
    /// 显示某会话的历史消息
    Show {
        /// 会话 id（缺省用最新）
        id: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum VaultCmd {
    /// 初始化 vault（建目录 + 写缺省配置）
    Init,
    /// 显示 vault 状态（路径、文件、是否就绪）
    Show,
    /// 从全局 OMP 配置导入 models.yml / config.yml
    ImportLlm,
}

#[derive(Subcommand, Debug)]
enum ModelsCmd {
    /// 列出 provider / key 数量 / 模型 / 角色（不打印 key）
    List,
    /// 校验配置能否组装出可用的 LlmParams（不发请求）
    Check,
}

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
        Cmd::Chat { message, resume, session, new } => {
            run_chat(&vault, message.as_deref(), *resume, session.as_deref(), *new, cli.json)
        }
        Cmd::Session { cmd } => run_session(&vault, cmd, cli.json),
        Cmd::Memory { cmd } => run_memory(&vault, cmd, cli.json),
        Cmd::Novel { chap_id, volume, lib, dry_run, prompt_only } => {
            run_novel(&vault, *chap_id, volume, lib.as_deref(), *dry_run, *prompt_only, cli.json)
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
    }
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
    let llm = cfg.llm_params(&vault.llm_dir())?;
    let enabled = ne_agent::resolve_enabled_tools(&cfg)?;
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
) -> Result<String> {
    if let Some(id) = explicit {
        if store.get_session(id)?.is_none() {
            anyhow::bail!("会话不存在：{id}");
        }
        return Ok(id.to_string());
    }
    if resume && !new {
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
    json: bool,
) -> Result<()> {
    let h = build_runtime(vault)?;
    let title = match message {
        Some(m) => format!("cli: {}", truncate(m, 30)),
        None => "交互对话".to_string(),
    };
    let sid = pick_session(&h.store, session, resume, new, &title)?;

    if let Some(msg) = message {
        let out = ne_agent::task::chat_once(
            &h.store,
            &h.memory,
            &sid,
            &h.cfg,
            h.llm,
            vault.workspace(),
            vault.path(ne_agent::vault::PROMPTS_DIR),
            h.enabled,
            msg,
        )?;
        if json {
            println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                "session": sid, "reply": out,
            }))?);
        } else {
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
        vault.workspace(),
        vault.path(ne_agent::vault::PROMPTS_DIR),
        h.enabled,
    )
}

/// 记忆管理。
fn run_memory(vault: &Vault, cmd: &MemoryCmd, json: bool) -> Result<()> {
    vault.init()?;
    let mem = ne_agent::memory::MemoryStore::open(vault.agent_db())?;
    // owner 标识必须与 task.rs 用的 `work_root`（即 workspace 目录）一致，
    // 否则 CLI 存的记忆对话里召回不到。
    let owner = vault.workspace().display().to_string();

    match cmd {
        MemoryCmd::List { pinned } => {
            let rows = if *pinned { mem.pinned(&owner, "")? } else { mem.all()? };
            if json {
                let v: Vec<serde_json::Value> = rows
                    .iter()
                    .map(|m| serde_json::json!({
                        "scope": m.scope, "kind": m.kind, "key": m.key,
                        "value": m.value, "pinned": m.pinned,
                    }))
                    .collect();
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else if rows.is_empty() {
                println!("（无记忆）");
            } else {
                for m in rows {
                    println!(
                        "  [{}/{}]{} {} = {}",
                        m.scope, m.kind,
                        if m.pinned { " 📌" } else { "" },
                        m.key, truncate(&m.value, 70)
                    );
                }
            }
            Ok(())
        }
        MemoryCmd::Save { key, value, scope, kind, pinned } => {
            let valid_scope = ["global", "project", "session"];
            if !valid_scope.contains(&scope.as_str()) {
                anyhow::bail!("scope 必须是 global|project|session，收到 `{scope}`");
            }
            let owner_id = match scope.as_str() {
                "global" => String::new(),
                "session" => anyhow::bail!("session 作用域需指定会话，暂请用 project 或 global"),
                _ => owner.clone(),
            };
            let m = ne_agent::memory::Memory {
                id: String::new(),
                scope: scope.clone(),
                owner_id,
                kind: kind.clone(),
                key: key.clone(),
                value: value.clone(),
                pinned: *pinned,
                expires_at: None,
                created_at: 0,
                updated_at: 0,
            };
            mem.save(&m)?;
            if !json {
                println!(
                    "已保存 [{}/{}]{} {key}（owner={}）",
                    scope, kind,
                    if *pinned { " 📌" } else { "" },
                    truncate(&owner, 40)
                );
            }
            let _ = &owner;
            Ok(())
        }
        MemoryCmd::Forget { key, scope } => {
            let owner_id = if scope == "global" { "" } else { owner.as_str() };
            let done = mem.delete(key, scope, owner_id)?;
            if !json {
                println!("{}", if done { format!("已删除 {key}") } else { format!("未找到 {key}") });
            }
            Ok(())
        }
        MemoryCmd::Recall { query } => {
            let cands = mem.candidates(&owner, "", 64)?;
            let picked = ne_agent::memory::select_recall(query, &cands, ne_agent::memory::now_ms());
            if json {
                let v: Vec<serde_json::Value> = picked
                    .iter()
                    .map(|m| serde_json::json!({"key": m.key, "value": m.value, "scope": m.scope}))
                    .collect();
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else if picked.is_empty() {
                println!("（无召回命中——不相关就不注入）");
            } else {
                println!("召回 {} 条：", picked.len());
                for m in picked {
                    println!("  [{}/{}] {} = {}", m.scope, m.kind, m.key, truncate(&m.value, 70));
                }
            }
            Ok(())
        }
    }
}

fn run_session(vault: &Vault, cmd: &SessionCmd, json: bool) -> Result<()> {
    let vault_ready = vault.is_initialized();
    if !vault_ready {
        anyhow::bail!("vault 未初始化：{}（跑 `ne-agent vault init`）", vault.root().display());
    }
    let store = ne_agent::session::SessionStore::open(vault.agent_db())?;
    match cmd {
        SessionCmd::List => {
            let rows = store.list_sessions()?;
            if json {
                let v: Vec<serde_json::Value> = rows
                    .iter()
                    .map(|s| {
                        serde_json::json!({
                            "id": s.id, "title": s.title,
                            "updatedAt": s.updated_at,
                            "parent": s.parent_session_id,
                            "messages": store.message_count(&s.id).unwrap_or(0),
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else if rows.is_empty() {
                println!("（无会话）");
            } else {
                for s in rows {
                    let n = store.message_count(&s.id).unwrap_or(0);
                    println!("  {}  {:>3} 条  {}", s.id, n, truncate(&s.title, 40));
                }
            }
            Ok(())
        }
        SessionCmd::Show { id } => {
            let sid = match id {
                Some(i) => i.clone(),
                None => match store.latest_session()? {
                    Some(s) => s.id,
                    None => {
                        println!("（无会话）");
                        return Ok(());
                    }
                },
            };
            let hist = store.history(&sid)?;
            if json {
                let v: Vec<serde_json::Value> = hist
                    .iter()
                    .map(|m| serde_json::json!({"role": m.role, "content": m.content}))
                    .collect();
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                println!("会话 {sid}（{} 条）", hist.len());
                for m in hist {
                    println!("  [{}] {}", m.role, truncate(&m.content, 80));
                }
            }
            Ok(())
        }
    }
}

fn truncate(s: &str, n: usize) -> String {
    let one_line = s.lines().next().unwrap_or("");
    if one_line.chars().count() > n {
        format!("{}…", one_line.chars().take(n).collect::<String>())
    } else {
        one_line.to_string()
    }
}

/// 生成一章。
///
/// 库根解析顺序：显式 `--lib` / `$NE_AGENT_LIB` → vault 的 `workspace/`
/// （vault 里放一份库，或软链指向真实资料库）。
fn run_novel(
    vault: &Vault,
    chap_id: u32,
    volume: &str,
    lib: Option<&Path>,
    dry_run: bool,
    prompt_only: bool,
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

    let out = ne_agent::task::novel_chapter(
        &lib_root,
        volume,
        chap_id,
        &llm,
        &vault.workspace(),
        &prompt_dir,
        dry_run,
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
