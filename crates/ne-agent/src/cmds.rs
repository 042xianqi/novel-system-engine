//! CLI 子命令处理器 —— 从 `main.rs` 拆出（架构门禁：单文件 ≤ 700 行）。
//!
//! 这里只放"把解析好的命令交给模块"的胶水；业务逻辑在 `ne_agent::task*` / `subagent`。

use anyhow::{Context, Result};
use ne_agent::Vault;
use std::path::Path;

use crate::cli::{MemoryCmd, SessionCmd, TaskCmd};
use crate::{build_runtime, truncate};

mod cmds_ctx;
mod inspect;
mod mcp;
mod pipeline_ops;
mod wg_trace;
pub(crate) use cmds_ctx::{run_compress, run_context, run_history};
pub(crate) use inspect::{
    run_checkpoint, run_compliance, run_estimate, run_health, run_layer_health, run_models,
    run_outline, run_refs, run_risk_list, run_sys_check,
};
pub(crate) use mcp::{run_library_ctl, run_mcp};
pub(crate) use wg_trace::run_wg_trace;
pub(crate) use pipeline_ops::{
    print_audit_batch, print_audit_one, run_fix, run_novel, run_pipeline, run_score,
};

/// 运行子智能体 Lane。
pub(crate) fn run_subagent(
    vault: &Vault,
    file: &Path,
    session: Option<&str>,
    max_tasks: usize,
    json: bool,
) -> Result<()> {
    use std::io::Read;
    let text = if file.as_os_str() == "-" {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        s
    } else {
        std::fs::read_to_string(file)
            .with_context(|| format!("读子任务文件失败：{}", file.display()))?
    };
    let spec: serde_json::Value =
        serde_json::from_str(&text).context("子任务 JSON 解析失败")?;
    let tasks = ne_agent::subagent::parse_subagents(&spec, max_tasks);
    if tasks.is_empty() {
        anyhow::bail!(
            "没有解析出任何子任务。每个子任务需要 prompt，且 role 或 (department+agentQuery) 至少一组"
        );
    }
    // 写租约冲突提前拒绝
    ne_agent::subagent::check_write_leases(&tasks)?;

    let h = build_runtime(vault)?;
    let sid = match session {
        Some(s) => s.to_string(),
        None => h
            .store
            .latest_session()?
            .map(|s| s.id)
            .ok_or_else(|| anyhow::anyhow!("没有会话可作父会话；先 `chat` 一次或用 --session"))?,
    };
    let parent = ne_agent::task::parent_ctx(
        &h.store,
        &sid,
        &h.cfg,
        h.llm,
        vault.workspace(),
        vault.path(ne_agent::vault::PROMPTS_DIR),
        h.enabled,
    );
    if !json {
        println!("父会话 {} · {} 个子任务（顺序 Lane）", sid, tasks.len());
    }
    let outcomes = ne_agent::task::run_subagents(&parent, &tasks, max_tasks)?;
    if json {
        let v: Vec<serde_json::Value> = outcomes
            .iter()
            .map(|o| {
                serde_json::json!({
                    "taskName": o.task_name, "session": o.session_id,
                    "ok": o.ok, "output": o.output,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        println!("{}", ne_agent::subagent::render_outcomes(&outcomes));
    }
    Ok(())
}

/// 管理持久化任务。
pub(crate) fn run_task(vault: &Vault, cmd: &TaskCmd, json: bool) -> Result<()> {
    vault.init()?;
    let store = ne_agent::task_state::TaskStore::open(vault.agent_db())?;
    match cmd {
        TaskCmd::List => {
            let tasks = store.list_all()?;
            if json {
                let v: Vec<serde_json::Value> = tasks
                    .iter()
                    .map(|t| serde_json::json!({
                        "id": t.id, "session": t.session_id, "title": t.title,
                        "status": t.status, "detail": t.status_detail,
                        "attempts": t.attempt_count, "maxAttempts": t.max_attempts,
                        "progress": t.progress,
                    }))
                    .collect();
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else if tasks.is_empty() {
                println!("（没有任务）");
            } else {
                for t in tasks {
                    println!(
                        "  {:<14} {:<17} {:>5.1}%  {}",
                        t.id,
                        t.status,
                        t.progress * 100.0,
                        truncate(&t.title, 40)
                    );
                }
            }
            Ok(())
        }
        TaskCmd::New { session, description, title, auto_resume, max_attempts } => {
            let id = format!("task-{}", ne_agent::session::now_ms());
            let t = store.create_queued(
                &id,
                session,
                title.as_deref().unwrap_or(""),
                description,
                *auto_resume,
                *max_attempts,
                ne_agent::session::now_ms(),
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                    "id": t.id, "status": t.status,
                }))?);
            } else {
                println!("已创建任务 {}（{}）", t.id, t.status);
            }
            Ok(())
        }
        TaskCmd::Show { id } => {
            let t = store.get(id)?.ok_or_else(|| anyhow::anyhow!("任务不存在：{id}"))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                    "id": t.id, "session": t.session_id, "title": t.title,
                    "description": t.description, "status": t.status,
                    "detail": t.status_detail, "attempts": t.attempt_count,
                    "maxAttempts": t.max_attempts, "progress": t.progress,
                    "lastRound": t.last_round, "maxRounds": t.max_rounds,
                    "error": t.error_message,
                }))?);
            } else {
                println!("任务 {}", t.id);
                println!("  标题     : {}", t.title);
                println!("  会话     : {}", t.session_id);
                println!("  状态     : {}（{}）", t.status, t.status_detail);
                println!("  尝试     : {}/{}", t.attempt_count, t.max_attempts);
                println!("  进度     : {:.1}%（第 {}/{} 轮）", t.progress * 100.0, t.last_round, t.max_rounds);
                if let Some(e) = &t.error_message {
                    println!("  错误     : {e}");
                }
            }
            Ok(())
        }
        TaskCmd::Set { id, status } => {
            // 手动转移：把任意"非终态"作为期望集（运维手势，宽松但走同一张表）
            let cur = store.get(id)?.ok_or_else(|| anyhow::anyhow!("任务不存在：{id}"))?;
            let next: &'static str = match status.as_str() {
                "running" => ne_agent::task_state::status::RUNNING,
                "waiting_approval" => ne_agent::task_state::status::WAITING_APPROVAL,
                "recovering" => ne_agent::task_state::status::RECOVERING,
                "suspended" => ne_agent::task_state::status::SUSPENDED,
                "failed" => ne_agent::task_state::status::FAILED,
                "completed" => ne_agent::task_state::status::COMPLETED,
                "cancelled" => ne_agent::task_state::status::CANCELLED,
                other => anyhow::bail!("未知状态：{other}"),
            };
            // 期望集 = 当前状态（CAS 语义：只有还是当前状态才转）
            let cur_static: &'static str = match cur.status.as_str() {
                "queued" => ne_agent::task_state::status::QUEUED,
                "running" => ne_agent::task_state::status::RUNNING,
                "waiting_approval" => ne_agent::task_state::status::WAITING_APPROVAL,
                "recovering" => ne_agent::task_state::status::RECOVERING,
                "suspended" => ne_agent::task_state::status::SUSPENDED,
                "failed" => ne_agent::task_state::status::FAILED,
                "completed" => ne_agent::task_state::status::COMPLETED,
                "cancelled" => ne_agent::task_state::status::CANCELLED,
                other => anyhow::bail!("库中出现未知状态：{other}"),
            };
            let ok = store.transition(&ne_agent::task_state::Transition {
                id: id.clone(),
                expected_statuses: vec![cur_static],
                next_status: next,
                status_detail: Some("手动设置".into()),
                updated_at: ne_agent::session::now_ms(),
                ..Default::default()
            })?;
            if json {
                println!("{}", serde_json::json!({"id": id, "changed": ok, "status": next}));
            } else if ok {
                println!("任务 {id} → {next}");
            } else {
                println!("任务 {id} 状态未变（CAS 拒绝：当前已不是 {}）", cur.status);
            }
            Ok(())
        }
    }
}


/// 记忆管理。
pub(crate) fn run_memory(vault: &Vault, cmd: &MemoryCmd, json: bool) -> Result<()> {
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


pub(crate) fn run_session(vault: &Vault, cmd: &SessionCmd, json: bool) -> Result<()> {
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
        SessionCmd::Rewind { id, to, back, last_turn } => {
            let sid = match id {
                Some(i) => i.clone(),
                None => match store.latest_session()? {
                    Some(s) => s.id,
                    None => {
                        println!("（无会话可回退）");
                        return Ok(());
                    }
                },
            };
            let before = store.history(&sid)?.len();
            let new_head = if let Some(m) = to {
                store.rewind_to(&sid, *m)?;
                Some(*m)
            } else if let Some(n) = back {
                store.rewind_back(&sid, *n)?
            } else if *last_turn {
                store.rewind_last_turn(&sid)?
            } else {
                anyhow::bail!("需指定 --to <消息id> / --back <N> / --last-turn 之一");
            };
            let after = store.history(&sid)?.len();
            let total = store.message_count(&sid)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "session": sid, "before": before, "after": after,
                        "headId": new_head, "totalMessages": total,
                    }))?
                );
            } else {
                println!("会话 {sid} 已回退：分支 {before} → {after} 条（库中共 {total} 条，未删）");
                match new_head {
                    Some(h) => println!("  head = 消息 #{h}"),
                    None => println!("  head = 空分支"),
                }
            }
            Ok(())
        }
    }
}

