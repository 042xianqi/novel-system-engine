//! Agent 主循环 —— 多轮工具循环 + 停止语义 + 出口配对修复。
//!
//! 参考（**边做边对照太墟**）：
//!   * `HarnessLoop.kt:1211` `runLoopRounds` —— 主循环、`RoundBudget`、
//!     `maxConsecutiveFailures` 熔断（连续失败即停，防风暴）
//!   * `ProviderClient.kt:711` `sanitizeApiTranscript` —— **出口最后一道防线**：
//!     `role=tool` 必须紧随含对应 tool_call_id 的 assistant(tool_calls)，
//!     错位/孤立的转写为 user 文本，缺失的补占位，否则服务端 400。
//!
//! # 本轮相对初版的修正
//!
//! 初版想用「输出长度是否顶到 max_tokens」来猜 `finish_reason`，据此决定要不要
//! 执行工具。查了太墟后确认**这是错的**：
//!   * 太墟的 OpenAI 兼容路径**不看 finish_reason**，只按 `toolCalls.isNotEmpty()`
//!     分流（`HarnessLoop.kt:1309`）。
//!   * 猜长度既不可靠（CJK/ASCII 比例、推理内容都影响），又有误伤风险。
//! 现在照太墟：**有工具调用就执行，没有就收尾**。
//!
//! # 结构
//!
//! ```text
//!   run_session(用户输入) {
//!       for round in 0..budget {                 // RoundBudget（太墟式轮次预算）
//!           组装消息 → 调模型 → 解析
//!           无工具调用           → Stop，收尾
//!           有工具调用           → 执行 → 回注 → 下一轮
//!           连续失败 >= 上限     → 熔断，收尾（含提示）
//!       }
//!   }
//! ```
//!
//! # M1 的简化（诚实标注）
//!
//! v1 用**文本协议**解析工具调用（模型输出 `<tool_call>{json}</tool_call>`），
//! 不走 provider 的结构化 `tools/tool_calls` 通道（那需要 provider 侧配合，
//! 留到 M4）。因此内部把工具结果存为 `role=tool` 只是**自用记录**，
//! 真正发给模型前由 [`sanitize_for_provider`] 转写成合法形状——
//! 这正是太墟 `sanitizeApiTranscript` 的思路。

use crate::config::RuntimeConfig;
use crate::prompt::{self, TurnPrefix};
use crate::protocol::{resolve, sanitize_for_provider, Reply};
use crate::session::{MsgRow, SessionStore};
use crate::tool::{self, Tool, ToolCtx};
use anyhow::Result;
use std::collections::BTreeMap;

pub use crate::protocol::Call;

/// 一次会话推进的结束原因（对齐太墟的 RunResult 语义）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// 模型正常说完（无工具调用）
    Stop,
    /// 工具轮预算耗尽
    BudgetExhausted,
    /// 连续失败熔断（太墟 `maxConsecutiveFailures`）
    FailureCircuit,
    /// 模型调用失败（本轮直接终止）
    Error,
}

/// 整体运行结果。
#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub stop: StopReason,
    /// 最终给用户看的文本（最后一段无工具调用的 assistant 文本）
    pub final_text: String,
    /// 总共跑了几轮
    pub rounds: usize,
    /// 全部工具结果（按顺序）
    pub tool_results: Vec<tool::ToolResult>,
    pub warning: Option<String>,
}

/// 运行配置。
pub struct HarnessCtx<'a> {
    pub store: &'a SessionStore,
    pub session: String,
    pub cfg: &'a RuntimeConfig,
    pub llm: ne_engine::llm::LlmParams,
    pub work_root: std::path::PathBuf,
    pub prompt_dir: std::path::PathBuf,
    pub enabled: Vec<Tool>,
    /// 单轮最多几个工具调用（太墟默认 12）
    pub max_tool_calls_per_turn: usize,
    /// 最多几轮（太墟 RoundBudget）
    pub max_rounds: usize,
    /// 连续失败熔断阈值（太墟默认 8）
    pub max_consecutive_failures: usize,
    /// pinned 记忆段：**常驻 system prompt 稳定前缀**。
    ///
    /// 在 ctx 构造时**取一次快照**；会话进行中记忆库变化不影响本次运行——
    /// 这是 prefix cache 稳定性的要求（见 memory.rs 模块头）。
    pub pinned_section: String,
    /// 外部 MCP 服务清单（None = 未配置；有则惰性连接）。
    pub mcp_servers: Option<Vec<crate::config::McpServerCfg>>,
    /// 审批模式（M6）。默认 `Assisted`。
    pub approval_mode: crate::approval::ApprovalMode,
    /// 只读规划模式（M6）：与审批模式**正交**，为真时禁止一切副作用工具。
    pub plan_only: bool,
    /// 审批回调：返回 true = 批准执行。`None` = 无法交互（非交互式 CLI），
    /// 此时需审批的调用**默认拒绝**（安全默认，不静默放行）。
    pub approver: Option<Box<dyn Fn(&crate::approval::Decision) -> bool + 'a>>,
    /// system prompt 覆盖（双智能体的 Planner 用专用规划提示词）。
    ///
    /// `None` = 走平常的 [`system_prompt_text`] 组装；`Some` = 直接用给定文本作 system
    /// （仍拼 pinned_section）。用于 Planner 这类需要**纯规划人格**的子智能体。
    pub system_override: Option<String>,
    /// vault 根（load_rule/load_skill 读 `<vault>/rules`、`<vault>/skills`）。
    pub vault_root: std::path::PathBuf,
    /// ask_user 的交互回调（`None` = 非交互 → ask_user 明确报错）。
    pub asker: Option<crate::tool::AskerFn>,
}

impl<'a> HarnessCtx<'a> {
    pub fn max_rounds(&self) -> usize {
        if self.max_rounds == 0 { 16 } else { self.max_rounds }
    }
    pub fn max_tool_calls(&self) -> usize {
        if self.max_tool_calls_per_turn == 0 { 12 } else { self.max_tool_calls_per_turn }
    }
    pub fn max_consecutive_failures(&self) -> usize {
        if self.max_consecutive_failures == 0 { 8 } else { self.max_consecutive_failures }
    }
}

/// 组装 system prompt（**稳定段**，不含逐轮变化内容）。
///
/// pinned 记忆段拼在末尾——它同样是**稳定**的（排序内容确定，见 memory.rs），
/// 因此可以安全地进 system prompt；逐轮变的 recall 走 [`TurnPrefix`]。
pub fn system_prompt_text(h: &HarnessCtx) -> Result<String> {
    if let Some(ov) = &h.system_override {
        // 覆盖模式（如双智能体 Planner）：直接用既定文本，仅拼 pinned 段
        let mut s = ov.clone();
        if !h.pinned_section.is_empty() {
            s.push_str(&h.pinned_section);
        }
        return Ok(s);
    }
    let tools_desc = h
        .enabled
        .iter()
        .map(|t| format!("- `{}`：{}", t.name(), t.description()))
        .chain(mcp_tool_lines(h))
        .collect::<Vec<_>>()
        .join("\n");
    let vars = agent_vars(&tools_desc, &h.work_root);
    prompt::validate_vars(&vars)?;
    let mut s = prompt::build_system_prompt(&h.prompt_dir, &vars)?.text;
    s.push_str(&resources_catalog(h));
    if !h.pinned_section.is_empty() {
        s.push_str(&h.pinned_section);
    }
    Ok(s)
}

/// 按需资源的**目录段**（只列名，正文靠 load_rule/load_skill 拉）。
///
/// 与 tools 清单同理：常驻的只有"存在什么"，不是"内容是什么"——避免正文常驻
/// 击穿 KV cache 并挤压真实上下文（见 [crate::resources] 的模块说明）。
///
/// 仅当对应工具**已启用**时才列出，避免模型看到用不了的能力。
fn resources_catalog(h: &HarnessCtx) -> String {
    let has_rule = h.enabled.contains(&Tool::LoadRule);
    let has_skill = h.enabled.contains(&Tool::LoadSkill);
    if !has_rule && !has_skill {
        return String::new();
    }
    let mut out = String::new();
    if has_rule {
        let names = crate::resources::available_rule_names(&h.vault_root);
        if !names.is_empty() {
            out.push_str(&format!(
                "\n## 可加载的规则块（用 `load_rule` 取正文）\n- {names}\n"
            ));
        }
    }
    if has_skill {
        let catalog = crate::resources::skill_catalog(&h.vault_root);
        if !catalog.is_empty() {
            out.push_str("\n## 可加载的技能（用 `load_skill` 取正文）\n");
            for s in &catalog {
                if s.description.is_empty() {
                    out.push_str(&format!("- `{}`\n", s.name));
                } else {
                    out.push_str(&format!("- `{}`：{}\n", s.name, s.description));
                }
            }
        }
    }
    out
}

/// MCP 工具清单行（供 prompt 告知模型）。**已连接的**才列出（避免启动全部服务）。
///
/// ⚠ 这里不主动 `discover_all()`（那会 spawn 所有服务）；只列出**显式启用的服务名**，
/// 让模型知道"存在这些服务"，**并走 `use_capability` 代理按需发现**。
///
/// 学太墟 v0.22.0：MCP 工具 schema 不进 provider 可见面（避免服务清单变化击穿
/// KV prefix cache），模型经 `use_capability` 的 list/inspect 发现、call 调用。
/// 具体工具清单在服务首次 inspect/call 时由模型按需发现（或 `ne-agent mcp list`）。
fn mcp_tool_lines(h: &HarnessCtx) -> Vec<String> {
    let Some(specs) = &h.mcp_servers else { return Vec::new() };
    let mut out = Vec::new();
    for s in specs.iter().filter(|s| s.enabled) {
        out.push(format!(
            "- MCP 服务 `{}`：用 `use_capability`（action=\"inspect\" + server=\"{}\"）查看其工具清单，\
             再用 action=\"call\" + server=\"{}\" + tool=… + arguments={{…}} 调用",
            s.name, s.name, s.name
        ));
    }
    out
}

fn agent_vars<'v>(tools_desc: &'v str, work_root: &'v std::path::Path) -> BTreeMap<&'v str, String> {
    let mut vars = BTreeMap::new();
    vars.insert("AGENT_NAME", "ne-agent".to_string());
    vars.insert("TOOLS", tools_desc.to_string());
    vars.insert("VAULT_ROOT", work_root.display().to_string());
    vars
}

/// 从会话树取出要发给模型的完整消息序列（已应用压缩投影 + 出口修复）。
///
/// 装配顺序（M9 起）：
///   1. system（稳定字节）
///   2. **压缩投影**（摘要条 + 保留区；见 [`crate::compaction`]）—— 会话若从未压缩过，
///      投影等同于全量历史，行为与 M8 完全一致
///   3. 逐轮变化前缀只挂最新 user 轮
///   4. 老工具结果截断 + 预算折叠（M5）
///   5. 出口修复（[`crate::protocol::sanitize_for_provider`]：保配对、补占位、错位转写）
pub fn build_messages(h: &HarnessCtx, turn_prefix: &TurnPrefix) -> Result<Vec<ne_engine::llm::Message>> {
    let mut out = Vec::new();
    out.push(ne_engine::llm::Message::system(system_prompt_text(h)?));

    // M9：压缩投影是"实际上下文"的唯一来源（发模型与 UI 同口径）
    let proj = crate::compaction::project(h.store, &h.session)?;
    let hist: Vec<MsgRow> = proj.retained;

    // 摘要条本身是 user 角色；逐轮前缀必须挂在**最新 user 轮**，不能挂到摘要上 →
    // 先推摘要条，再对保留区定位最后一条 user。
    if let Some(s) = &proj.summary {
        out.push(ne_engine::llm::Message::user(format!(
            "{}\n{s}",
            crate::compaction::SUMMARY_HEADER
        )));
    }
    let last_user_idx = hist.iter().rposition(|m| m.role == "user");
    for (i, m) in hist.iter().enumerate() {
        // 逐轮变化的前缀只挂在**最新** user 轮上（不污染 system，也不改历史轮）
        if Some(i) == last_user_idx && m.role == "user" && !turn_prefix.is_empty() {
            out.push(ne_engine::llm::Message {
                role: m.role.clone(),
                content: turn_prefix.apply_to(&m.content),
                ..Default::default()
            });
        } else {
            out.push(m.to_llm());
        }
    }

    // ── M5：老轮次工具结果截断（预算线**之前**就生效）
    out = crate::ctx_window::truncate_context(&h.cfg.context, out);

    Ok(sanitize_for_provider(&out))
}

/// 把本轮调用编码成 assistant 消息的 meta JSON（`{"tool_calls":[...]}`）。
///
/// 只有带 id 的调用才入 meta（文本回退的调用没有 id，无配对可言）。
/// `arguments` 以**字符串**存（OpenAI 线格式如此），复用 `ToolCall` 的嵌套序列化。
fn assistant_meta(calls: &[Call]) -> String {
    let arr: Vec<ne_engine::llm::ToolCall> = calls
        .iter()
        .filter_map(|c| {
            let id = c.id.as_ref()?;
            Some(ne_engine::llm::ToolCall {
                id: id.clone(),
                name: c.name.clone(),
                arguments: c.args.to_json_except("").to_string(),
            })
        })
        .collect();
    serde_json::json!({ "tool_calls": arr }).to_string()
}

/// 执行一批工具调用。
fn run_calls(h: &HarnessCtx, ctx: &ToolCtx, calls: &[Call]) -> Vec<tool::ToolResult> {
    let mut results = Vec::new();
    for (i, c) in calls.iter().enumerate() {
        if i >= h.max_tool_calls() {
            results.push(tool::ToolResult::err(format!(
                "超出单轮工具调用上限（{}），未执行：{}",
                h.max_tool_calls(),
                c.name
            )));
            continue;
        }
        let Some(t) = Tool::from_name(&c.name) else {
            results.push(tool::ToolResult::err(format!(
                "未知工具 `{}`；可用：{}",
                c.name,
                Tool::all().iter().map(|t| t.name()).collect::<Vec<_>>().join(", ")
            )));
            continue;
        };
        if !h.enabled.contains(&t) {
            results.push(tool::ToolResult::err(format!("工具 `{}` 未启用", c.name)));
            continue;
        }

        // ── M6 审批门禁（在只读规划之上，两层都过才执行）
        // 1) 只读规划：硬拒（不进审批队列，弹审批卡只是噪音）
        if h.plan_only {
            if let Some(reason) = crate::approval::plan_block(t, &c.args) {
                results.push(tool::ToolResult::err(format!(
                    "已被只读规划模式拦截：{reason}（工具 `{}`）",
                    c.name
                )));
                continue;
            }
        }
        // 2) 审批策略：需要批准时问用户；无法交互则**默认拒绝**（安全默认）
        // MCP 限定名（`mcp__<服务>__<工具>`）由 harness 放进内部键，供路由与风险定级。
        let args = if t == Tool::Mcp && c.args.get(tool::MCP_TOOL_KEY).is_none() {
            c.args.clone().set(tool::MCP_TOOL_KEY, c.name.as_str())
        } else {
            c.args.clone()
        };
        let raw_mcp = if t == Tool::Mcp {
            args.get(tool::MCP_TOOL_KEY)
        } else {
            None
        };
        let decision =
            crate::approval::decide(h.approval_mode, t, &args, &h.work_root, raw_mcp);
        if decision.required {
            let approved = match &h.approver {
                Some(f) => f(&decision),
                None => false, // 非交互式：默认拒绝，绝不静默放行
            };
            if !approved {
                results.push(tool::ToolResult::err(format!(
                    "审批未通过（风险 {}）：{}{}",
                    decision.risk,
                    decision.summary,
                    if h.approver.is_none() {
                        "　[非交互环境，需审批的操作默认拒绝]"
                    } else {
                        "　[用户拒绝]"
                    }
                )));
                continue;
            }
        }

        match tool::execute(ctx, t, &args) {
            Ok(r) => results.push(r),
            Err(e) => results.push(tool::ToolResult::err(format!("执行 `{}` 内部错误：{e}", c.name))),
        }
    }
    results
}

/// 跑一次完整会话推进（多轮工具循环）。
///
/// 前置：最新 user 消息**已在会话库**（调用 [`persist_user`]）。
pub fn run_session(h: &HarnessCtx, turn_prefix: &TurnPrefix) -> Result<RunOutcome> {
    let mut ctx = ToolCtx::new(&h.work_root);
    if let Some(specs) = &h.mcp_servers {
        ctx = ctx.with_mcp(crate::mcp::McpRegistry::new(specs.clone()));
    }
    // plan/scratchpad 需要会话 id + 存储（与记忆库同文件）
    if let Ok(cs) = crate::context_tools::ContextStore::open(h.store.path()) {
        ctx = ctx.with_session(h.session.clone(), cs);
    }
    // M8：load_rule/load_skill 需要 vault 根；ask_user 需要交互回调；双智能体需要运行时上下文
    ctx = ctx.with_vault_root(h.vault_root.clone());
    ctx = ctx.with_runtime(
        h.store.path().to_path_buf(),
        std::sync::Arc::new(h.cfg.clone()),
        h.prompt_dir.clone(),
        h.enabled.clone(),
    );
    ctx = ctx.with_llm(h.llm.clone());
    if let Some(asker) = h.asker {
        ctx = ctx.with_asker(asker);
    }
    let mut all_results = Vec::new();
    let mut consecutive_failures = 0usize;
    let mut last_text = String::new();
    let mut rounds = 0usize;

    // M9：自动压缩（策略开关；手动/命令压缩不受此约束）
    crate::ctx_window::maybe_auto_compact(&h.cfg.context, h.store, &h.session, &h.llm);

    while rounds < h.max_rounds() {
        rounds += 1;
        let msgs = build_messages(h, turn_prefix)?;
        let out = match ne_engine::llm::call(&h.llm, &msgs) {
            Ok(o) => o,
            Err(e) => {
                return Ok(RunOutcome {
                    stop: StopReason::Error,
                    final_text: last_text,
                    rounds,
                    tool_results: all_results,
                    warning: Some(format!("模型调用失败：{e}")),
                });
            }
        };

        // 决议顺序：原生 tool_calls 优先，文本标记回退（太墟 resolveCalls 同策略）
        match resolve(&out.tool_calls, &out.content) {
            Reply::ToolCalls(calls) if !calls.is_empty() => {
                // 记录 assistant 轮：正文（原生轮常为空）+ meta 里的结构化 tool_calls。
                // meta 是协议侧数据，用于下一轮投影时重建 assistant(tool_calls) 配对——
                // 缺它则工具结果变"孤儿"，模型看不到自己调用了什么。
                let meta = assistant_meta(&calls);
                h.store.append_meta(
                    &h.session,
                    h.store.head(&h.session)?,
                    "assistant",
                    out.content.trim(),
                    Some(&meta),
                )?;
                let results = run_calls(h, &ctx, &calls);
                // 落盘工具结果：与调用按序号配对，meta 记 tool_call_id
                let mut parent = h.store.head(&h.session)?;
                for (c, r) in calls.iter().zip(results.iter()) {
                    let rmeta = c
                        .id
                        .as_deref()
                        .map(|id| format!("{}", serde_json::json!({"tool_call_id": id})));
                    parent = Some(h.store.append_meta(
                        &h.session,
                        parent,
                        "tool",
                        &r.render(),
                        rmeta.as_deref(),
                    )?);
                }
                let failed = results.iter().filter(|r| !r.ok).count();
                all_results.extend(results);
                if failed == calls.len() {
                    consecutive_failures += 1;
                    // 把失败原因回注，给模型一次纠错机会（太墟同样回注错误）
                    let hint = format!(
                        "【全部 {} 个工具调用失败】请根据上面的错误信息调整参数或改用其他工具；\
                         不要重复完全相同的调用。",
                        failed
                    );
                    h.store.append(&h.session, h.store.head(&h.session)?, "user", &hint)?;
                    if consecutive_failures >= h.max_consecutive_failures() {
                        return Ok(RunOutcome {
                            stop: StopReason::FailureCircuit,
                            final_text: last_text,
                            rounds,
                            tool_results: all_results,
                            warning: Some(format!(
                                "连续 {} 轮工具调用全部失败，已熔断停止（太墟 maxConsecutiveFailures 同策略）",
                                consecutive_failures
                            )),
                        });
                    }
                } else {
                    consecutive_failures = 0;
                }
            }
            _ => {
                // 无工具调用 → 收尾
                last_text = match resolve(&out.tool_calls, &out.content) {
                    Reply::Text(t) => t,
                    _ => out.content.trim().to_string(),
                };
                return Ok(RunOutcome {
                    stop: StopReason::Stop,
                    final_text: last_text,
                    rounds,
                    tool_results: all_results,
                    warning: None,
                });
            }
        }
    }

    Ok(RunOutcome {
        stop: StopReason::BudgetExhausted,
        final_text: last_text,
        rounds,
        tool_results: all_results,
        warning: Some(format!("达到轮次上限 {}，已停止（太墟 RoundBudget 同策略）", h.max_rounds())),
    })
}

/// 追加「用户输入」到会话库。
///
/// ⚠ 顺序：必须**先落 user 消息**，再跑模型——模型是从会话历史组装上下文的。
pub fn persist_user(h: &HarnessCtx, user_text: &str) -> Result<i64> {
    let parent = h.store.head(&h.session)?;
    h.store.append(&h.session, parent, "user", user_text)
}

/// 只在会话里留一条最终回复（工具轮已由 `run_session` 落盘）。
///
/// 返回落盘的消息数。
pub fn persist_final(h: &HarnessCtx, outcome: &RunOutcome) -> Result<usize> {
    if outcome.final_text.trim().is_empty() {
        return Ok(0);
    }
    let parent = h.store.head(&h.session)?;
    h.store.append(&h.session, parent, "assistant", &outcome.final_text)?;
    Ok(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::aborted_placeholders;
    use crate::tool::ToolArgs;

    #[test]
    fn aborted_placeholders_pair_every_call() {
        let calls = vec![
            Call { id: None, name: "read".into(), args: ToolArgs::new() },
            Call { id: None, name: "write".into(), args: ToolArgs::new() },
        ];
        let ph = aborted_placeholders(&calls, "被截断");
        assert_eq!(ph.len(), 2, "每个 call 必须有配对结果");
        assert!(ph.iter().all(|r| !r.ok));
    }

    #[test]
    fn assistant_meta_carries_ids_and_arguments() {
        let calls = vec![Call {
            id: Some("call_1".into()),
            name: "read".into(),
            args: ToolArgs::new().set("path", "a.txt"),
        }];
        let meta = assistant_meta(&calls);
        let v: serde_json::Value = serde_json::from_str(&meta).unwrap();
        let c = &v["tool_calls"][0];
        assert_eq!(c["id"], "call_1");
        assert_eq!(c["type"], "function");
        assert_eq!(c["function"]["name"], "read");
        // arguments 是 JSON **字符串**（OpenAI 线格式），内容为参数对象
        let args: serde_json::Value =
            serde_json::from_str(c["function"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args["path"], "a.txt");
    }

    #[test]
    fn assistant_meta_skips_idless_text_calls() {
        // 文本回退的调用没有 id → 无配对可言，不入 meta
        let calls = vec![Call { id: None, name: "read".into(), args: ToolArgs::new() }];
        let v: serde_json::Value = serde_json::from_str(&assistant_meta(&calls)).unwrap();
        assert_eq!(v["tool_calls"].as_array().unwrap().len(), 0);
    }
}
