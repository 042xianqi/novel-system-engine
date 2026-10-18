//! 会话局部协作工具 —— `plan` / `scratchpad`（契约对齐太墟 AgentContextExecutor）。
//!
//! 从 tool.rs 拆出（架构门禁：单文件 ≤ 700 行）。

use super::*;

/// `plan`：多步骤计划管理（持久化）。契约对齐太墟 `AgentContextExecutor.executePlan`。
pub(super) fn plan_tool(ctx: &ToolCtx, a: &ToolArgs) -> ToolResult {
    let Some(cs) = ctx.context.as_ref() else {
        return ToolResult::err("plan 需要会话上下文（未装配 ContextStore）");
    };
    let session = &ctx.session;
    let action = a.get("action").unwrap_or("get_active").to_ascii_lowercase();
    let now = crate::session::now_ms();
    let cs = cs.borrow();
    match action.as_str() {
        "replace_active" | "set_active" | "create" => {
            let Some(goal) = a.get("goal") else {
                return ToolResult::err("plan replace_active 必须提供 goal 目标描述");
            };
            let steps = a.get("steps").unwrap_or("[]");
            match cs.save_plan(session, goal, steps, crate::context_tools::PlanStatus::Active, now) {
                Ok(p) => ToolResult::ok(format!(
                    "已创建/更新任务规划：\n目标：{}\n步骤：{}\n\n提示：用 plan(action=\"advance\", steps=...) 更新进度；\
                     遇阻用 replace_active 调整；完成时 clear_active。",
                    p.goal, p.steps_json
                )),
                Err(e) => ToolResult::err(format!("保存计划失败：{e}")),
            }
        }
        "get_active" | "get" => match cs.get_plan(session) {
            Ok(Some(p)) => ToolResult::ok(format!(
                "当前活跃计划：\n目标：{}\n步骤：{}\n状态：{}",
                p.goal, p.steps_json, p.status
            )),
            Ok(None) => ToolResult::ok("当前会话暂无活跃任务计划"),
            Err(e) => ToolResult::err(format!("读取计划失败：{e}")),
        },
        "advance" | "update_steps" => {
            let steps = a.get("steps");
            // ⚠ 只接受生命周期枚举；模型传了阶段说明 → 保留原状态（太墟教训）
            let new_status = a.get("status").and_then(crate::context_tools::PlanStatus::parse);
            match cs.advance_plan(session, steps, new_status, now) {
                Ok(Some(p)) => ToolResult::ok(format!(
                    "【规划进度已同步更新】\n目标：{}\n状态：{}\n当前步骤：{}",
                    p.goal, p.status, p.steps_json
                )),
                Ok(None) => ToolResult::err(
                    "当前会话没有可推进的活跃计划，请先用 plan(action=\"replace_active\", goal=...) 创建",
                ),
                Err(e) => ToolResult::err(format!("更新计划失败：{e}")),
            }
        }
        "clear_active" | "clear" => match cs.clear_plan(session) {
            Ok(()) => ToolResult::ok("已清空当前会话的任务规划"),
            Err(e) => ToolResult::err(format!("清空计划失败：{e}")),
        },
        other => ToolResult::err(format!(
            "未知的 plan 动作：{other}（可用：replace_active/get_active/advance/clear_active）"
        )),
    }
}

/// `scratchpad`：会话局部草稿（持久化）。对齐太墟 `executeScratchpad`。
pub(super) fn scratchpad_tool(ctx: &ToolCtx, a: &ToolArgs) -> ToolResult {
    let Some(cs) = ctx.context.as_ref() else {
        return ToolResult::err("scratchpad 需要会话上下文（未装配 ContextStore）");
    };
    let session = &ctx.session;
    let action = a.get("action").unwrap_or("list").to_ascii_lowercase();
    let now = crate::session::now_ms();
    let cs = cs.borrow();
    match action.as_str() {
        "save" | "set" => {
            let (Some(k), Some(v)) = (a.get("key"), a.get("value")) else {
                return ToolResult::err("scratchpad save 必须提供 key 与 value");
            };
            match cs.save_scratch(session, k, v, now) {
                Ok(()) => ToolResult::ok(format!("已记录工作草稿 [{k}] = {v}")),
                Err(e) => ToolResult::err(format!("保存草稿失败：{e}")),
            }
        }
        "get" => {
            let Some(k) = a.get("key") else {
                return ToolResult::err("scratchpad get 必须提供 key");
            };
            match cs.get_scratch(session, k) {
                Ok(Some(s)) => ToolResult::ok(format!("草稿 [{k}]：{}", s.value)),
                Ok(None) => ToolResult::ok(format!("草稿 [{k}] 不存在")),
                Err(e) => ToolResult::err(format!("读取草稿失败：{e}")),
            }
        }
        "list" => match cs.list_scratch(session) {
            Ok(list) if list.is_empty() => ToolResult::ok("当前无工作草稿记录"),
            Ok(list) => {
                let s = list
                    .iter()
                    .map(|x| format!("- [{}]: {}", x.key, x.value))
                    .collect::<Vec<_>>()
                    .join("\n");
                ToolResult::ok(format!("当前工作草稿：\n{s}"))
            }
            Err(e) => ToolResult::err(format!("列出草稿失败：{e}")),
        },
        "delete" => {
            let Some(k) = a.get("key") else {
                return ToolResult::err("scratchpad delete 必须提供 key");
            };
            match cs.delete_scratch(session, k) {
                Ok(()) => ToolResult::ok(format!("已删除草稿 [{k}]")),
                Err(e) => ToolResult::err(format!("删除草稿失败：{e}")),
            }
        }
        "clear" => match cs.clear_scratch(session) {
            Ok(()) => ToolResult::ok("已清空当前会话的所有工作草稿"),
            Err(e) => ToolResult::err(format!("清空草稿失败：{e}")),
        },
        other => ToolResult::err(format!(
            "未知的 scratchpad 动作：{other}（可用：save/get/list/delete/clear）"
        )),
    }
}

// ───────────────────────────── M8 新增：ask_user / load_rule / load_skill / 子智能体

/// `ask_user`：向用户提结构化问题（复用交互回调通道）。
///
/// 契约对照太墟 `AskUserModels`：1~4 个问题，每题可带候选项（label + 说明）并允许自定义输入。
/// `args["questions"]` 是一个 JSON 数组（harness 从 JSON 取值时作为字符串透传）。
pub(super) fn ask_user(ctx: &ToolCtx, a: &ToolArgs) -> ToolResult {
    let Some(raw) = a.get("questions") else {
        return ToolResult::err("ask_user 必须提供 questions 数组");
    };
    let qs = match crate::ask_user::parse(raw) {
        Ok(q) => q,
        Err(e) => return ToolResult::err(format!("questions 解析失败：{e}")),
    };
    let Some(asker) = ctx.asker.as_ref() else {
        return ToolResult::err(
            "ask_user 需要交互通道，但当前为非交互环境（子智能体 Lane 或脚本调用）。\
             请把问题交接给主智能体，由主会话重新发起 ask_user。",
        );
    };
    let rendered = crate::ask_user::render_questions(&qs);
    match asker(&rendered) {
        Ok(answer) if !answer.trim().is_empty() => {
            ToolResult::ok(format!("用户已回答 ask_user 提问：\n{answer}"))
        }
        Ok(_) => ToolResult::err("用户未提供回答（空输入）。"),
        Err(e) => ToolResult::err(format!("获取用户回答失败：{e}")),
    }
}

/// `load_rule`：按需加载规则块完整正文（目录常驻，正文懒加载）。
pub(super) fn load_rule_tool(ctx: &ToolCtx, a: &ToolArgs) -> ToolResult {
    let Some(vault) = ctx.vault_root.as_deref() else {
        return ToolResult::err("load_rule 需要 vault 上下文（未装配 vault_root）");
    };
    let Some(rule) = a.get("rule").filter(|s| !s.trim().is_empty()) else {
        return ToolResult::err("load_rule 必须提供 rule 名称");
    };
    match crate::resources::load_rule(vault, rule) {
        Some(content) => ToolResult::ok(format!("【规则块：{rule}】\n{}", content.trim())),
        None => ToolResult::err(format!(
            "未知规则块：{rule}。可用：{}",
            crate::resources::available_rule_names(vault)
        )),
    }
}

/// `load_skill`：按需加载技能（正文或子资源）。
///
/// 匹配/越界防护逻辑在 [`crate::resources`]，与太墟 `ToolExecutor.LOAD_SKILL` 同口径。
pub(super) fn load_skill_tool(ctx: &ToolCtx, a: &ToolArgs) -> ToolResult {
    let Some(vault) = ctx.vault_root.as_deref() else {
        return ToolResult::err("load_skill 需要 vault 上下文（未装配 vault_root）");
    };
    let Some(name) = a.get("name").filter(|s| !s.trim().is_empty()) else {
        return ToolResult::err("load_skill 必须提供 name");
    };
    let path = a.get("path");
    match crate::resources::load_skill(vault, name, path) {
        Ok(loaded) => ToolResult::ok(loaded.render()),
        Err(e) => ToolResult::err(e.to_string()),
    }
}

/// `subagents`：派发隔离子智能体（顺序执行，写租约校验）。
pub(super) fn subagents_tool(ctx: &ToolCtx, a: &ToolArgs) -> ToolResult {
    use crate::subagent::{self, DEFAULT_MAX_TASKS};
    let Some(raw) = a.get("subagents") else {
        return ToolResult::err("subagents 必须提供 subagents 数组");
    };
    let Ok(raw_json) = serde_json::from_str::<serde_json::Value>(raw) else {
        return ToolResult::err("subagents 参数不是合法 JSON（应为任务数组）");
    };
    let max_tasks = a
        .get("max_tasks")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_TASKS);
    // 工具参数里 subagents 是数组本身；parse_subagents 期望 {"subagents":[...]} 形状 → 包一层
    let wrapped = serde_json::json!({ "subagents": raw_json });
    let tasks = subagent::parse_subagents(&wrapped, max_tasks);
    if tasks.is_empty() {
        return ToolResult::err(
            "没有解析出任何子任务（每条需 prompt 非空，且 role 或 department+agentQuery 至少一组）",
        );
    }
    match crate::subagent_runner::run_subagents_in_ctx(ctx, &tasks, max_tasks) {
        Ok(outcomes) => ToolResult::ok(subagent::render_outcomes(&outcomes)),
        Err(e) => ToolResult::err(format!("子智能体执行失败：{e}")),
    }
}

/// `dual_agent`：Planner→Executor 顺序双智能体。
pub(super) fn dual_agent_tool(ctx: &ToolCtx, a: &ToolArgs) -> ToolResult {
    let Some(prompt) = a.get("prompt").filter(|s| !s.trim().is_empty()) else {
        return ToolResult::err("dual_agent 必须提供 prompt（任务与验收标准）");
    };
    let max_steps = a
        .get("max_steps")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(crate::dual_agent::DEFAULT_MAX_STEPS);
    match run_dual_agent(ctx, prompt, max_steps) {
        Ok(text) => ToolResult::ok(text),
        Err(e) => ToolResult::err(format!("双智能体执行失败：{e}")),
    }
}

/// 装配并运行双智能体（从 ctx 取运行时上下文）。
fn run_dual_agent(ctx: &ToolCtx, prompt: &str, max_steps: usize) -> anyhow::Result<String> {
    use crate::dual_agent::{DualAgentOutcome, DualAgentParams};
    let params = DualAgentParams::from_ctx(ctx, prompt, max_steps)?;
    match crate::dual_agent::run(&params)? {
        DualAgentOutcome::Success { final_report, plan, total_rounds, total_tool_calls } => Ok(format!(
            "{final_report}\n\n双智能体执行完成：{}/{} 步，{total_tool_calls} 次工具调用，{total_rounds} 轮规划。",
            plan.iter().filter(|s| s.status == crate::dual_agent::StepStatus::Completed).count(),
            plan.len()
        )),
        DualAgentOutcome::Failed { message, plan, .. } => Ok(format!(
            "{message}\n当前进度：{}/{} 步完成。",
            plan.iter().filter(|s| s.status == crate::dual_agent::StepStatus::Completed).count(),
            plan.len()
        )),
    }
}

// ───────────────────────────── M9 新增：compress / history_read / history_search

/// 打开会话库（compress / history_* 共用）。
fn open_store(ctx: &ToolCtx) -> Result<crate::session::SessionStore, ToolResult> {
    match &ctx.db_path {
        Some(p) => crate::session::SessionStore::open(p)
            .map_err(|e| ToolResult::err(format!("打开会话库失败：{e}"))),
        None => Err(ToolResult::err("该工具需要会话库上下文（未装配 db_path）")),
    }
}

/// `compress`：压缩上下文。**原文不删**，只记"折叠边界 + 摘要"。
///
/// 契约对齐太墟 `ToolExecutor.executeCompress`：`mode` = before/after，
/// `anchor` 必须原样、唯一地摘自某条用户消息（before 模式下定位边界）。
pub(super) fn compress_tool(ctx: &ToolCtx, a: &ToolArgs) -> ToolResult {
    let store = match open_store(ctx) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let mode = match a.get("mode") {
        Some(m) => match crate::compaction::CompressMode::parse(m) {
            Some(m) => m,
            None => {
                return ToolResult::err(
                    "mode 必须是 before（压缩锚点轮之前）或 after（压缩除当前轮外的全部已完成历史）",
                )
            }
        },
        None => crate::compaction::CompressMode::After,
    };
    let anchor = a.get("anchor").unwrap_or("");

    // 摘要器：有 LLM 上下文 → 用模型摘要；否则机械降级
    let llm = ctx.llm.clone();
    let outcome = match &llm {
        Some(p) => {
            let s = crate::compaction::LlmSummarizer { llm: p.clone() };
            crate::compaction::compact(&store, &ctx.session, mode, anchor, Some(&s))
        }
        None => crate::compaction::compact(&store, &ctx.session, mode, anchor, None),
    };
    match outcome {
        Ok(outcome) => ToolResult::ok(outcome.render(mode)),
        Err(e) => ToolResult::err(e.to_string()),
    }
}

/// `history_read`：按 id 回读一条历史原文（被折叠的内容也能读到）。
pub(super) fn history_read_tool(ctx: &ToolCtx, a: &ToolArgs) -> ToolResult {
    let store = match open_store(ctx) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let Some(id) = a.get("id").and_then(|s| s.trim().parse::<i64>().ok()) else {
        return ToolResult::err("history_read 必须提供数字 id");
    };
    match store.message_by_id(id) {
        Ok(Some(m)) => ToolResult::ok(format!(
            "【消息 #{id}】role={} 时间={}\n{}",
            m.role, m.created_at, m.content
        )),
        Ok(None) => ToolResult::err(format!("消息 #{id} 不存在")),
        Err(e) => ToolResult::err(format!("读取消息失败：{e}")),
    }
}

/// `history_search`：在本会话全部消息中模糊搜索（含被折叠的原文）。
pub(super) fn history_search_tool(ctx: &ToolCtx, a: &ToolArgs) -> ToolResult {
    let store = match open_store(ctx) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let Some(q) = a.get("query").filter(|s| !s.trim().is_empty()) else {
        return ToolResult::err("history_search 必须提供 query");
    };
    let limit = a
        .get("limit")
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(20)
        .clamp(1, 100);
    match store.search_messages(&ctx.session, q, limit) {
        Ok(hits) if hits.is_empty() => ToolResult::ok(format!("没有匹配「{q}」的消息")),
        Ok(hits) => {
            let mut s = format!("找到 {} 条匹配「{q}」的消息（含被压缩折叠的原文）：\n", hits.len());
            for (id, role, snip) in hits {
                s.push_str(&format!("- #{id} [{role}] {snip}\n"));
            }
            s.push_str("\n用 history_read(id=...) 读取完整原文。");
            ToolResult::ok(s)
        }
        Err(e) => ToolResult::err(format!("搜索失败：{e}")),
    }
}
