//! 双智能体（Planner → Executor）—— **顺序版**。
//!
//! 契约对照太墟 `harness/dual/`：`DualAgentCoordinator.kt` + `DualAgentModels.kt` +
//! `PlannerProtocolParser.kt` + `PlannerPromptBuilder.kt`。
//!
//! # 与太墟的**关键差异**（如实说明，不粉饰）
//!
//! 太墟的 Executor 按 DAG 依赖**并发**执行就绪步骤（`MAX_CONCURRENT_STEPS=3`）。
//! ne-agent 运行时是 **std 阻塞单线程**、写小说是长链顺序任务，并发既无收益又会让
//! "同一章被两路写"。所以此处**顺序执行**：
//!   * 保留 DAG 拓扑（`dependencies` 就绪判定）——抽象不丢；
//!   * 每轮只挑**一个**就绪步骤执行（取计划中最靠前者），而非整批并发；
//!   * 其余语义（REPLAN 保留 COMPLETED、FAILED 可重试、级联失败、FINISH 前校验）
//!     全部对齐太墟。
//!
//! # 数据流
//!
//! ```text
//!   Planner(只规划,不碰工具)  --JSON-->  解析 decision
//!        ↑                                    |
//!        |                                    v
//!   feed 执行汇报  <-- 精炼结果 --  Executor(隔离子会话,真跑工具)
//! ```
//!
//! Planner 用 `system_override` 换成纯规划人格；Executor 复用 `task::run_subagents`
//! 的隔离子会话机制（一个步骤一个 Lane），只回传**精炼摘要**，不回传原生工具日志。

use anyhow::{bail, Result};

use crate::harness::{self, HarnessCtx};
use crate::prompt::TurnPrefix;

/// 默认最大规划轮数（太墟 `DEFAULT_MAX_STEPS`）。
pub const DEFAULT_MAX_STEPS: usize = 10;

/// 步骤状态（对齐太墟 `StepStatus`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

impl StepStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            StepStatus::Pending => "PENDING",
            StepStatus::Running => "RUNNING",
            StepStatus::Completed => "COMPLETED",
            StepStatus::Failed => "FAILED",
        }
    }
}

/// Planner 生成的单步工序（对齐太墟 `PlanStep`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanStep {
    pub id: String,
    pub title: String,
    pub instruction: String,
    pub expected_outcome: String,
    pub dependencies: Vec<String>,
    pub status: StepStatus,
    pub result_summary: Option<String>,
}

impl PlanStep {
    /// 依赖全部 COMPLETED 且自身 PENDING 时"就绪"（对齐太墟 `isReady`）。
    pub fn is_ready(&self, completed: &std::collections::HashSet<String>) -> bool {
        self.status == StepStatus::Pending
            && self.dependencies.iter().all(|d| completed.contains(d))
    }
}

/// Planner 的决策（对齐太墟 `PlannerDecision`）。
#[derive(Debug, Clone)]
pub enum PlannerDecision {
    InitializePlan { thought: String, plan: Vec<PlanStep> },
    ExecuteStep { step: PlanStep, updated_plan: Vec<PlanStep> },
    Replan { reason: String, new_steps: Vec<PlanStep> },
    Finish { final_report: String, completed_steps: Vec<PlanStep> },
}

/// Executor 单步汇报（对齐太墟 `StepExecutionResult`；不回传原生工具日志）。
#[derive(Debug, Clone)]
pub struct StepExecutionResult {
    pub step_id: String,
    pub success: bool,
    pub summary: String,
    pub tool_calls_count: usize,
}

/// 双智能体最终输出（对齐太墟 `DualAgentOutcome`）。
#[derive(Debug, Clone)]
pub enum DualAgentOutcome {
    Success {
        final_report: String,
        plan: Vec<PlanStep>,
        total_rounds: usize,
        total_tool_calls: usize,
    },
    Failed {
        message: String,
        plan: Vec<PlanStep>,
        failed_step: Option<PlanStep>,
    },
}

/// 运行参数（从 ToolCtx 装配；`from_ctx` 见下）。
pub struct DualAgentParams {
    pub prompt: String,
    pub max_steps: usize,
    pub workspace: std::path::PathBuf,
    pub vault_root: std::path::PathBuf,
    pub db_path: std::path::PathBuf,
    pub cfg: std::sync::Arc<crate::config::RuntimeConfig>,
    pub prompt_dir: std::path::PathBuf,
    pub enabled: Vec<crate::tool::Tool>,
    pub llm: ne_engine::llm::LlmParams,
    /// 父会话 id（Executor 的 Lane 会挂在它下面，形成会话树）
    pub parent_session: String,
}

impl DualAgentParams {
    /// 从工具总线 ctx 装配。
    pub fn from_ctx(ctx: &crate::tool::ToolCtx, prompt: &str, max_steps: usize) -> Result<Self> {
        let db_path = ctx.db_path.clone().ok_or_else(|| anyhow::anyhow!("缺少 db_path"))?;
        let cfg = ctx.cfg.clone().ok_or_else(|| anyhow::anyhow!("缺少 cfg"))?;
        let prompt_dir = ctx.prompt_dir.clone().ok_or_else(|| anyhow::anyhow!("缺少 prompt_dir"))?;
        let llm = ctx.llm.clone().ok_or_else(|| anyhow::anyhow!("缺少 llm"))?;
        if ctx.session.trim().is_empty() {
            bail!("缺少会话上下文（session 为空）");
        }
        Ok(DualAgentParams {
            prompt: prompt.to_string(),
            max_steps: max_steps.clamp(1, 30),
            workspace: ctx.root.clone(),
            vault_root: ctx.vault_root.clone().unwrap_or_else(|| ctx.root.clone()),
            db_path,
            cfg,
            prompt_dir,
            enabled: ctx.enabled.clone(),
            llm,
            parent_session: ctx.session.clone(),
        })
    }
}

// ─────────────────────────────── 主循环

/// 跑一次完整双智能体流程（**顺序**）。
pub fn run(p: &DualAgentParams) -> Result<DualAgentOutcome> {
    let store = crate::session::SessionStore::open(&p.db_path)?;

    // Planner 的专属会话（只规划，不落工具结果）
    let planner_sid = format!("dual:planner:{}", p.parent_session);
    if store.get_session(&planner_sid)?.is_none() {
        store.create_child_session(&planner_sid, &p.parent_session, "双智能体·Planner")?;
    }
    // 首轮把用户目标落进去
    let head = store.head(&planner_sid)?;
    store.append(&planner_sid, head, "user", &p.prompt)?;

    let mut steps: Vec<PlanStep> = Vec::new();
    let mut total_tool_calls = 0usize;

    for round in 1..=p.max_steps {
        // Planner 决策（每轮都读最新会话历史）
        let decision = planner_decide(p, &store, &planner_sid)?;
        match decision {
            PlannerDecision::Finish { final_report, completed_steps } => {
                let all_done = !steps.is_empty()
                    && steps.iter().all(|s| s.status == StepStatus::Completed);
                if !all_done {
                    // 未全部完成就想 FINISH → 打回（对齐太墟"不得提前 FINISH"）
                    let incomplete: Vec<String> = steps
                        .iter()
                        .filter(|s| s.status != StepStatus::Completed)
                        .map(|s| format!("{}({})", s.id, s.status.as_str()))
                        .collect();
                    feed_planner(
                        &store,
                        &planner_sid,
                        &format!(
                            "计划仍有未完成工序：{}。不得提前 FINISH；请继续执行就绪工序或使用 REPLAN 修复依赖。",
                            incomplete.join("、")
                        ),
                    )?;
                    continue;
                }
                let _ = completed_steps;
                return Ok(DualAgentOutcome::Success {
                    final_report: if final_report.trim().is_empty() {
                        "全部工序已完成。".into()
                    } else {
                        final_report
                    },
                    plan: steps,
                    total_rounds: round,
                    total_tool_calls,
                });
            }
            PlannerDecision::Replan { reason, new_steps } => {
                // 保留 COMPLETED（防重复副作用）；FAILED 重置为 PENDING（重试语义）
                merge_replan(&mut steps, new_steps);
                feed_planner(&store, &planner_sid, &format!("Planner 调整了执行方案：{reason}"))?;
                total_tool_calls += execute_one_ready(p, &store, &planner_sid, &mut steps)?;
            }
            PlannerDecision::InitializePlan { plan, .. } => {
                steps = plan;
                total_tool_calls += execute_one_ready(p, &store, &planner_sid, &mut steps)?;
            }
            PlannerDecision::ExecuteStep { step, updated_plan } => {
                apply_execute_step(&mut steps, step, updated_plan);
                total_tool_calls += execute_one_ready(p, &store, &planner_sid, &mut steps)?;
            }
        }
    }

    Ok(DualAgentOutcome::Failed {
        message: format!("已达到最大规划步数上限（{} 步），未能全部完成", p.max_steps),
        failed_step: steps.iter().find(|s| s.status != StepStatus::Completed).cloned(),
        plan: steps,
    })
}

/// Planner 决策（一轮）。
fn planner_decide(
    p: &DualAgentParams,
    store: &crate::session::SessionStore,
    planner_sid: &str,
) -> Result<PlannerDecision> {
    let h = HarnessCtx {
        store,
        session: planner_sid.to_string(),
        cfg: &p.cfg,
        llm: p.llm.clone(),
        work_root: p.workspace.clone(),
        prompt_dir: p.prompt_dir.clone(),
        enabled: Vec::new(), // Planner 无工具
        max_tool_calls_per_turn: 0,
        max_rounds: 0,
        max_consecutive_failures: 0,
        pinned_section: String::new(),
        mcp_servers: None,
        approval_mode: crate::approval::ApprovalMode::FullAccess,
        plan_only: false,
        approver: None,
        system_override: Some(planner_system_prompt(&p.workspace)),
        vault_root: p.vault_root.clone(),
        asker: None,
    };
    let msgs = harness::build_messages(&h, &TurnPrefix::default())?;
    let out = ne_engine::llm::call(&p.llm, &msgs)
        .map_err(|e| anyhow::anyhow!("Planner 模型调用失败：{e}"))?;
    let decision = parse_planner_decision(&out.content);
    // 落 Planner 原文（含 JSON），供后续轮次参考
    let head = store.head(planner_sid)?;
    store.append(planner_sid, head, "assistant", &out.content)?;
    Ok(decision)
}

/// 执行当前**一个**就绪步骤（顺序版核心差异：每轮只跑一步）。
fn execute_one_ready(
    p: &DualAgentParams,
    store: &crate::session::SessionStore,
    planner_sid: &str,
    steps: &mut [PlanStep],
) -> Result<usize> {
    cascade_failures(steps);
    let completed: std::collections::HashSet<String> = steps
        .iter()
        .filter(|s| s.status == StepStatus::Completed)
        .map(|s| s.id.clone())
        .collect();
    let Some(idx) = steps.iter().position(|s| s.is_ready(&completed)) else {
        // 无可执行工序：回注提示，让 Planner 用 REPLAN/FINISH 打破僵局
        feed_planner(
            store,
            planner_sid,
            "【提示】当前计划中暂无满足前置依赖的可执行工序。请审视步骤状态并使用 REPLAN 或 FINISH 调整。",
        )?;
        return Ok(0);
    };
    steps[idx].status = StepStatus::Running;
    let step = steps[idx].clone();

    // Executor：隔离子会话真跑工具（复用子智能体 Lane 机制）
    let lane_name = format!("dual:executor:{}", step.id);
    let lane_sid = crate::subagent::lane_session_id(&p.parent_session, idx, &lane_name);
    if store.get_session(&lane_sid)?.is_none() {
        store.create_child_session(&lane_sid, &p.parent_session, &lane_name)?;
    }
    let exec_prompt = build_executor_prompt(&step);
    let mut sub = HarnessCtx {
        store,
        session: lane_sid.clone(),
        cfg: &p.cfg,
        llm: p.llm.clone(),
        work_root: p.workspace.clone(),
        prompt_dir: p.prompt_dir.clone(),
        enabled: p.enabled.clone(),
        max_tool_calls_per_turn: 0,
        max_rounds: 0,
        max_consecutive_failures: 0,
        pinned_section: String::new(),
        mcp_servers: if p.cfg.mcp.servers.is_empty() {
            None
        } else {
            Some(p.cfg.mcp.servers.clone())
        },
        approval_mode: crate::approval::ApprovalMode::from_id(&p.cfg.approval.mode),
        plan_only: p.cfg.approval.plan_only,
        approver: None, // Executor 不带交互回调（需审批的默认拒绝，安全）
        system_override: None,
        vault_root: p.vault_root.clone(),
        asker: None,
    };
    // 先把单步指令落进 Executor 会话（模型从会话历史组装上下文）
    harness::persist_user(&sub, &exec_prompt)?;
    let run = harness::run_session(&sub, &TurnPrefix::default());

    let (ok, summary) = match run.as_ref() {
        Ok(o) => {
            let text = o.final_text.trim();
            let ok = !matches!(o.stop, harness::StopReason::Error)
                && !matches!(o.stop, harness::StopReason::FailureCircuit);
            (
                ok,
                if text.is_empty() {
                    format!("（无文本输出；stop={:?}）", o.stop)
                } else {
                    truncate_summary(text)
                },
            )
        }
        Err(e) => (false, format!("执行失败：{e}")),
    };
    let tool_calls = match run.as_ref() {
        Ok(o) => o.tool_results.len(),
        Err(_) => 0,
    };
    let _ = &mut sub;

    steps[idx].status = if ok { StepStatus::Completed } else { StepStatus::Failed };
    steps[idx].result_summary = Some(summary.clone());

    // 回注精炼汇报（绝不回传原生工具日志）
    let res = StepExecutionResult {
        step_id: step.id.clone(),
        success: ok,
        summary,
        tool_calls_count: tool_calls,
    };
    feed_batch(store, planner_sid, &[res], steps)?;
    Ok(tool_calls)
}

// ─────────────────────────────── 计划维护（对齐太墟语义）

/// REPLAN：保留 COMPLETED 的状态与摘要；其余（含 FAILED）按新计划重置。
fn merge_replan(steps: &mut Vec<PlanStep>, new_steps: Vec<PlanStep>) {
    let prev_status: std::collections::HashMap<String, StepStatus> =
        steps.iter().map(|s| (s.id.clone(), s.status)).collect();
    let prev_summary: std::collections::HashMap<String, Option<String>> = steps
        .iter()
        .map(|s| (s.id.clone(), s.result_summary.clone()))
        .collect();
    *steps = new_steps
        .into_iter()
        .map(|mut s| {
            if prev_status.get(&s.id) == Some(&StepStatus::Completed) {
                s.status = StepStatus::Completed;
                s.result_summary = prev_summary.get(&s.id).cloned().flatten();
            }
            s
        })
        .collect();
}

/// EXECUTE_STEP：COMPLETED 不重跑（防重复副作用）；FAILED 可重试（重置 PENDING）。
fn apply_execute_step(steps: &mut Vec<PlanStep>, step: PlanStep, updated_plan: Vec<PlanStep>) {
    if !updated_plan.is_empty() {
        *steps = updated_plan;
    }
    if let Some(i) = steps.iter().position(|s| s.id == step.id) {
        let prev = steps[i].clone();
        // 对齐太墟：COMPLETED 保留旧状态与摘要（不重跑）；FAILED 重置为 PENDING（重试意图）；
        // 其余（PENDING/RUNNING）保留旧状态与摘要、仅更新指令文案。
        steps[i] = match prev.status {
            StepStatus::Completed => PlanStep {
                status: StepStatus::Completed,
                result_summary: prev.result_summary,
                ..step
            },
            StepStatus::Failed => PlanStep {
                status: StepStatus::Pending,
                result_summary: None,
                ..step
            },
            _ => PlanStep {
                status: prev.status,
                result_summary: prev.result_summary,
                ..step
            },
        };
    } else {
        steps.push(step);
    }
}

/// 级联失败：某步 FAILED → 其（传递）依赖者标记 FAILED，避免活锁空转烧完 maxSteps。
fn cascade_failures(steps: &mut [PlanStep]) {
    let mut failed: std::collections::HashSet<String> = steps
        .iter()
        .filter(|s| s.status == StepStatus::Failed)
        .map(|s| s.id.clone())
        .collect();
    if failed.is_empty() {
        return;
    }
    loop {
        let mut changed = false;
        for s in steps.iter_mut() {
            if s.status == StepStatus::Pending && s.dependencies.iter().any(|d| failed.contains(d)) {
                s.status = StepStatus::Failed;
                s.result_summary = Some("前置工序失败，已级联标记为失败".into());
                failed.insert(s.id.clone());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}

// ─────────────────────────────── 回注

/// 落一条 user 消息给 Planner（进度反馈）。
fn feed_planner(store: &crate::session::SessionStore, sid: &str, text: &str) -> Result<()> {
    let head = store.head(sid)?;
    store.append(sid, head, "user", text)?;
    Ok(())
}

/// 把执行汇报回注给 Planner（格式对齐太墟 `feedBatchResultsToPlanner`）。
fn feed_batch(
    store: &crate::session::SessionStore,
    sid: &str,
    results: &[StepExecutionResult],
    all_steps: &[PlanStep],
) -> Result<()> {
    let mut text = String::new();
    if results.is_empty() {
        text.push_str("【提示】当前计划中暂无满足前置依赖的可执行工序。\n");
    } else {
        text.push_str(&format!("【本轮工序执行汇报（共 {} 项）】\n", results.len()));
        for r in results {
            text.push_str(&format!(
                "- 工序 {}：{}\n  成果摘要: {}\n",
                r.step_id,
                if r.success { "✅ 成功" } else { "❌ 失败" },
                r.summary
            ));
        }
    }
    let all_done = !all_steps.is_empty() && all_steps.iter().all(|s| s.status == StepStatus::Completed);
    if all_done {
        text.push_str("所有计划工序已全部完成！请评估成果并交付最终报告（FINISH）。\n");
    } else {
        let pending = all_steps.iter().filter(|s| s.status == StepStatus::Pending).count();
        let failed = all_steps.iter().filter(|s| s.status == StepStatus::Failed).count();
        text.push_str(&format!(
            "当前进度：剩余待执行 {pending} 项，失败 {failed} 项。\n请决定下一步：继续 EXECUTE_STEP、调整方案 REPLAN 或交付 FINISH。\n"
        ));
    }
    feed_planner(store, sid, &text)
}

// ─────────────────────────────── 解析（对齐太墟 PlannerProtocolParser）

/// 从 Planner 输出里解析 JSON 决策；解析不出来时**宽松降级**为 INIT_PLAN（若含 plan 数组）。
pub fn parse_planner_decision(text: &str) -> PlannerDecision {
    let json = extract_json_block(text);
    if let Some(v) = json.as_ref().and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok()) {
        let thought = v.get("thought").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let action = v.get("action").and_then(|x| x.as_str()).unwrap_or("").to_uppercase();
        let plan = v.get("plan").and_then(|x| x.as_array()).map(|a| parse_steps(a)).unwrap_or_default();
        let step = v.get("step").and_then(parse_step);
        let final_report =
            v.get("finalReport").and_then(|x| x.as_str()).unwrap_or("").to_string();
        match action.as_str() {
            "FINISH" => {
                return PlannerDecision::Finish { final_report, completed_steps: plan };
            }
            "REPLAN" => {
                return PlannerDecision::Replan { reason: thought, new_steps: plan };
            }
            "EXECUTE_STEP" => {
                if let Some(s) = step {
                    return PlannerDecision::ExecuteStep { step: s, updated_plan: plan };
                }
            }
            // INIT_PLAN 与未知 action 都走末尾的 plan 兜底
            _ => {}
        }
        if !plan.is_empty() {
            return PlannerDecision::InitializePlan { thought, plan };
        }
    }
    // 兜底：把整段当作一个单步（不空转，让 Executor 至少有活干）
    PlannerDecision::InitializePlan {
        thought: String::new(),
        plan: vec![PlanStep {
            id: "step_1".into(),
            title: "执行任务".into(),
            instruction: text.trim().to_string(),
            expected_outcome: String::new(),
            dependencies: Vec::new(),
            status: StepStatus::Pending,
            result_summary: None,
        }],
    }
}

fn parse_steps(arr: &[serde_json::Value]) -> Vec<PlanStep> {
    arr.iter().enumerate().filter_map(|(i, v)| parse_step_impl(v, i)).collect()
}

fn parse_step(v: &serde_json::Value) -> Option<PlanStep> {
    parse_step_impl(v, 0)
}

fn parse_step_impl(v: &serde_json::Value, idx: usize) -> Option<PlanStep> {
    let obj = v.as_object()?;
    let id = obj
        .get("id")
        .and_then(|x| x.as_str())
        .map(String::from)
        .unwrap_or_else(|| format!("step_{}", idx + 1));
    let title = obj.get("title").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let instruction = obj
        .get("instruction")
        .or_else(|| obj.get("prompt"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if instruction.trim().is_empty() && title.trim().is_empty() {
        return None;
    }
    let expected_outcome = obj
        .get("expectedOutcome")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let dependencies = obj
        .get("dependencies")
        .and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|d| d.as_str().map(String::from)).collect())
        .unwrap_or_default();
    Some(PlanStep {
        id,
        title,
        instruction,
        expected_outcome,
        dependencies,
        status: StepStatus::Pending,
        result_summary: None,
    })
}

/// 抽取 ```json ... ``` 块；无围栏则尝试 `{...}` 首尾配对。
fn extract_json_block(text: &str) -> Option<String> {
    if let Some(s) = text.find("```json") {
        let rest = &text[s + 7..];
        if let Some(e) = rest.find("```") {
            return Some(rest[..e].trim().to_string());
        }
    }
    if let Some(s) = text.find("```") {
        let rest = &text[s + 3..];
        if let Some(e) = rest.find("```") {
            let inner = rest[..e].trim();
            if inner.starts_with('{') {
                return Some(inner.to_string());
            }
        }
    }
    // 裸 JSON：取首个 { 到最后一个 }
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end > start {
        Some(text[start..=end].to_string())
    } else {
        None
    }
}

/// 截断摘要（Executor 只回传真精炼结果，防原生日志灌爆 Planner）。
fn truncate_summary(text: &str) -> String {
    const MAX: usize = 800;
    let t = text.trim();
    if t.chars().count() <= MAX {
        return t.to_string();
    }
    let head: String = t.chars().take(MAX).collect();
    format!("{head}…（摘要已截断）")
}

fn build_executor_prompt(step: &PlanStep) -> String {
    let mut s = String::new();
    s.push_str("请严格执行以下单步工序并汇报成果：\n");
    s.push_str(&format!("【步骤标题】: {}\n", step.title));
    s.push_str(&format!("【详细指令】: {}\n", step.instruction));
    if !step.expected_outcome.is_empty() {
        s.push_str(&format!("【预期成果】: {}\n", step.expected_outcome));
    }
    s
}

/// Planner 的 system prompt（对齐太墟 `PlannerPromptBuilder`；字节稳定、无时间戳）。
pub fn planner_system_prompt(workspace: &std::path::Path) -> String {
    let ws = workspace.display();
    format!(
        "你是系统顶层架构与规划智能体（Planner Agent）。\n\
         你的职责是分析用户的工程需求，制订精确、分步的执行计划，并逐步指挥执行智能体（Executor）完成任务。\n\n\
         ## 运行环境与工作区\n\
         - 工作区根目录: {ws}\n\n\
         ## 核心规则\n\
         1. 你没有直接的操作系统工具（如 read/write/base），所有底层操作均由 Executor 代为执行。\n\
         2. 每次只向 Executor 下发清晰、单一目标的单步指令，不要一次性派发多个混杂操作。\n\
         3. Executor 完成单步后会回传精炼结果，由你评估是否符合预期，并决定继续推进、微调计划或完工交付。\n\
         4. 当用户目标全部达成并验收通过后，生成最终交付总结。\n\n\
         ## 协议输出规范\n\
         在思考完成后，必须输出如下 JSON 块（用 ```json 包裹）：\n\
         {{\n  \"thought\": \"对当前进展的分析评估与规划考量\",\n  \"action\": \"INIT_PLAN | EXECUTE_STEP | REPLAN | FINISH\",\n  \"plan\": [\n    {{\n      \"id\": \"step_1\",\n      \"title\": \"简短步骤标题\",\n      \"instruction\": \"具体执行要求与目标\",\n      \"expectedOutcome\": \"预期交付成果\",\n      \"dependencies\": []\n    }}\n  ],\n  \"step\": {{\"id\":\"step_1\",\"title\":\"单步标题\",\"instruction\":\"具体要求\",\"expectedOutcome\":\"预期成果\",\"dependencies\":[]}},\n  \"finalReport\": \"仅在 action 为 FINISH 时提供完整总结\"\n}}\n\
         - 首轮规划可直接使用 INIT_PLAN 并给出 plan 列表；单步推进用 EXECUTE_STEP；全部完成用 FINISH。"
    )
}

#[cfg(test)]
#[path = "dual_agent_tests.rs"]
mod tests;

