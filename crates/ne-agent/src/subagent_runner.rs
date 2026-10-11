//! 子智能体 Lane 的**运行时装配层** —— 把「工具总线里的 ctx」翻译成「任务层的 HarnessCtx」。
//!
//! # 为什么单独一个文件
//!
//! 依赖方向是 `tool → subagent_runner → task`，而 `task::run_subagents` 需要 `&HarnessCtx`。
//! 如果让 `tool::session_local` 直接依赖 `task`，就会形成 `task → tool → task` 的循环。
//! 把"从 ToolCtx 重建 HarnessCtx"这件事隔离在这一层，循环即被切断：
//! `tool` 只依赖本模块的**窄函数**，本模块再依赖 `task`。
//!
//! # 重建的上下文来自哪里
//!
//! `ToolCtx` 在 [`crate::harness::run_session`] 里由 `HarnessCtx` 派生，携带了
//! `db_path / cfg / prompt_dir / vault_root / llm / enabled / session`。
//! 这里用它们重新打开会话库并组装父 ctx——**字段来源单一**，不会两处各写一份。

use anyhow::{bail, Result};

use crate::subagent::{LaneOutcome, SubagentTask};
use crate::tool::ToolCtx;

/// 用工具总线 ctx 里的运行时信息，执行一批子任务（顺序）。
pub fn run_subagents_in_ctx(
    ctx: &ToolCtx,
    tasks: &[SubagentTask],
    max_tasks: usize,
) -> Result<Vec<LaneOutcome>> {
    let Some(db_path) = &ctx.db_path else {
        bail!("subagents 需要会话库上下文（未装配 db_path）");
    };
    let Some(cfg) = &ctx.cfg else {
        bail!("subagents 需要运行时配置（未装配 cfg）");
    };
    let Some(prompt_dir) = &ctx.prompt_dir else {
        bail!("subagents 需要 prompt 目录（未装配 prompt_dir）");
    };
    let Some(llm) = &ctx.llm else {
        bail!("subagents 需要模型参数（未装配 llm）");
    };
    if ctx.session.trim().is_empty() {
        bail!("subagents 需要会话上下文（session 为空）");
    }

    let store = crate::session::SessionStore::open(db_path)?;
    // 父 ctx：用当前会话作为父会话，子 Lane 挂在它下面
    let parent = crate::harness::HarnessCtx {
        store: &store,
        session: ctx.session.clone(),
        cfg,
        llm: llm.clone(),
        work_root: ctx.root.clone(),
        prompt_dir: prompt_dir.clone(),
        enabled: ctx.enabled.clone(),
        max_tool_calls_per_turn: 0,
        max_rounds: 0,
        max_consecutive_failures: 0,
        pinned_section: String::new(),
        mcp_servers: if cfg.mcp.servers.is_empty() {
            None
        } else {
            Some(cfg.mcp.servers.clone())
        },
        approval_mode: crate::approval::ApprovalMode::from_id(&cfg.approval.mode),
        plan_only: cfg.approval.plan_only,
        approver: None,
        system_override: None,
        vault_root: ctx.vault_root.clone().unwrap_or_else(|| ctx.root.clone()),
        asker: None,
    };
    crate::task::run_subagents(&parent, tasks, max_tasks)
}
