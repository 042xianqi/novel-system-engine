//! 工具总线 —— **enum 工具** + 结构化结果 + 路径沙箱。
//!
//! # 为什么是 enum 而不是 trait 注册表
//!
//! 见 docs/ref_taixu_harness_notes.md（太墟用 `enum HarnessTool` + `match` 分派）。
//! 理由：
//!   * 静态、可穷举 —— `match` 漏分支编译期即报错
//!   * 零动态分发、零运行时注册 —— 没有"这个工具从哪来的"这种问题
//!   * schema 与执行体写在一起，改一处不会漏另一处
//!
//! 代价是要改工具就得重编译。对**单二进制**而言这是优点不是缺点。
//!
//! # 结果结构化（不抛异常）
//!
//! 工具失败**不返回 `Err`**，而是 [`ToolResult { ok: false, output }`](ToolResult)。
//! 原因（学太墟/oh-my-pi）：失败信息要**回注给模型**让它自己纠错，
//! 而不是炸掉整个循环。`Err` 只保留给"框架级故障"（如会话库打不开）。

use anyhow::Result;
use std::path::{Component, Path, PathBuf};

// ── 子模块（从单文件拆出，过架构门禁 ≤700 行）──
mod external_io;
mod fs;
mod session_local;
#[cfg(test)]
mod tests;

use external_io::{mcp_call, web_fetch, web_search};
use fs::{base, edit, ls, read, write};
use session_local::{ask_user, dual_agent_tool, load_rule_tool, load_skill_tool, plan_tool, scratchpad_tool, subagents_tool};

/// 工具权限档（M6 会接审批策略；M1 只做标注 + exec 类拒绝写盘外）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// 只读，无副作用
    Read,
    /// 写工作区内文件
    Write,
    /// 执行外部命令
    Exec,
}

/// 结构化工具结果。**失败也走这里**，不进 `Err`。
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub ok: bool,
    pub output: String,
}

impl ToolResult {
    pub fn ok(output: impl Into<String>) -> Self {
        ToolResult { ok: true, output: output.into() }
    }
    pub fn err(output: impl Into<String>) -> Self {
        ToolResult { ok: false, output: output.into() }
    }
    /// 给模型看的文本（含成功/失败标记，便于模型识别）。
    pub fn render(&self) -> String {
        let tag = if self.ok { "OK" } else { "ERROR" };
        format!("[{tag}] {}", self.output)
    }
}

/// 内置工具集合。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    /// 读文件
    Read,
    /// 写文件（覆盖）
    Write,
    /// 精确文本替换（要求 old 唯一匹配）
    Edit,
    /// 列目录
    Ls,
    /// 执行 shell 命令（危险）
    Base,
    /// 抓取网页 → 纯文本
    WebFetch,
    /// 联网搜索
    WebSearch,
    /// 调用外部 MCP 服务（名字形如 `mcp__<server>__<tool>`）
    Mcp,
    /// 多步骤计划管理（持久化到会话库）
    Plan,
    /// 会话局部工作草稿（持久化到会话库）
    Scratchpad,
    /// 向用户提结构化问题（关键决策点；复用交互回调通道）
    AskUser,
    /// 按需加载规则块（正文不常驻，命中后拉取）
    LoadRule,
    /// 按需加载技能（目录常驻，正文/子资源按需拉取）
    LoadSkill,
    /// 顺序子智能体 Lane（隔离子会话）
    Subagents,
    /// 双智能体（Planner→Executor，顺序版）
    DualAgent,
}

impl Tool {
    pub fn name(&self) -> &'static str {
        match self {
            Tool::Read => "read",
            Tool::Write => "write",
            Tool::Edit => "edit",
            Tool::Ls => "ls",
            Tool::Base => "base",
            Tool::WebFetch => "web_fetch",
            Tool::WebSearch => "web_search",
            Tool::Mcp => "mcp",
            Tool::Plan => "plan",
            Tool::Scratchpad => "scratchpad",
            Tool::AskUser => "ask_user",
            Tool::LoadRule => "load_rule",
            Tool::LoadSkill => "load_skill",
            Tool::Subagents => "subagents",
            Tool::DualAgent => "dual_agent",
        }
    }

    pub fn tier(&self) -> Tier {
        match self {
            Tool::Read | Tool::Ls => Tier::Read,
            Tool::Write | Tool::Edit => Tier::Write,
            Tool::Base => Tier::Exec,
            Tool::WebFetch | Tool::WebSearch => Tier::Read,
            // MCP 外部服务能力未知 → 按"有副作用"算，保守
            Tool::Mcp => Tier::Exec,
            // 计划/草稿：只改会话局部状态，无文件/外部副作用
            Tool::Plan | Tool::Scratchpad => Tier::Read,
            // ask/load_*：读取或会话局部，无文件/外部副作用
            Tool::AskUser | Tool::LoadRule | Tool::LoadSkill => Tier::Read,
            // 子智能体/双智能体：内部会起隔离子会话并可能执行工具 → 保守按 Exec
            Tool::Subagents | Tool::DualAgent => Tier::Exec,
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            Tool::Read => "读取文件内容（相对工作区路径）",
            Tool::Write => "写入/覆盖文件（相对工作区路径）",
            Tool::Edit => "在文件中做精确文本替换；old 必须唯一匹配",
            Tool::Ls => "列出目录内容",
            Tool::Base => "在工作区内执行 shell 命令",
            Tool::WebFetch => "抓取网页并转为纯文本（http/https；自动跟随重定向）",
            Tool::WebSearch => "联网搜索（返回标题/摘要/链接）",
            Tool::Mcp => "调用外部 MCP 服务（工具名形如 mcp__<服务>__<工具>）",
            Tool::Plan => "多步骤任务计划管理（action: replace_active/get_active/advance/clear_active）",
            Tool::Scratchpad => "会话局部工作草稿（action: save/get/list/delete/clear）",
            Tool::AskUser => "在关键决策点向用户提出结构化问题（选择题或自由输入）",
            Tool::LoadRule => "按需加载规则块完整正文（rule: workflow/security/tools/…）",
            Tool::LoadSkill => "按需加载技能的完整指导（name，可选 path 读技能内子资源）",
            Tool::Subagents => "派发隔离子智能体执行独立子任务（顺序执行，写租约校验）",
            Tool::DualAgent => "用 Planner→Executor 顺序双智能体执行复杂多步骤任务",
        }
    }

    pub fn from_name(s: &str) -> Option<Tool> {
        // MCP 工具名是动态的（mcp__<server>__<tool>）→ 前缀识别
        if s.starts_with("mcp__") {
            return Some(Tool::Mcp);
        }
        Some(match s {
            "read" => Tool::Read,
            "write" => Tool::Write,
            "edit" => Tool::Edit,
            "ls" => Tool::Ls,
            "base" => Tool::Base,
            "web_fetch" => Tool::WebFetch,
            "web_search" => Tool::WebSearch,
            "plan" => Tool::Plan,
            "scratchpad" => Tool::Scratchpad,
            "ask_user" => Tool::AskUser,
            "load_rule" => Tool::LoadRule,
            "load_skill" => Tool::LoadSkill,
            "subagents" | "subagent" | "invoke_subagent" => Tool::Subagents,
            "dual_agent" | "invoke_dual_agent" => Tool::DualAgent,
            _ => return None,
        })
    }

    pub fn all() -> &'static [Tool] {
        &[
            Tool::Read,
            Tool::Write,
            Tool::Edit,
            Tool::Ls,
            Tool::Base,
            Tool::WebFetch,
            Tool::WebSearch,
            Tool::Plan,
            Tool::Scratchpad,
            Tool::AskUser,
            Tool::LoadRule,
            Tool::LoadSkill,
            Tool::Subagents,
            Tool::DualAgent,
        ]
    }

    /// 参数中本工具需要的键名（用于参数校验与提示）。
    pub fn params(&self) -> &'static [&'static str] {
        match self {
            Tool::Read => &["path"],
            Tool::Write => &["path", "content"],
            Tool::Edit => &["path", "old", "new"],
            Tool::Ls => &["path"],
            Tool::Base => &["command"],
            Tool::WebFetch => &["url", "max_chars"],
            Tool::WebSearch => &["query", "engine", "limit"],
            Tool::Mcp => &["（由具体 MCP 工具定义）"],
            Tool::Plan => &["action", "goal", "steps", "status"],
            Tool::Scratchpad => &["action", "key", "value"],
            Tool::AskUser => &["questions"],
            Tool::LoadRule => &["rule"],
            Tool::LoadSkill => &["name", "path"],
            Tool::Subagents => &["subagents", "max_tasks"],
            Tool::DualAgent => &["prompt", "max_steps"],
        }
    }

    /// 是否走网络（需要联网工具开关；M6 审批会用）。
    pub fn is_network(&self) -> bool {
        matches!(self, Tool::WebFetch | Tool::WebSearch | Tool::Mcp)
    }

    /// 是否属于"会话局部协作状态"（不需要审批、不受只读规划限制）。
    ///
    /// 依据：它们只改会话自己的计划/草稿，不碰文件、不跑命令、不出网。
    pub fn is_session_local(&self) -> bool {
        matches!(self, Tool::Plan | Tool::Scratchpad | Tool::AskUser | Tool::LoadRule | Tool::LoadSkill)
    }
}

/// MCP 调用参数的内部键：由 harness 把原始限定名塞进来供路由。
pub const MCP_TOOL_KEY: &str = "__mcp_tool";

/// 执行上下文：工具只能在工作区内活动。
pub struct ToolCtx {
    pub root: PathBuf,
    /// 外部 MCP 服务注册表（惰性连接；None = 未配置）。
    ///
    /// 用 `RefCell` 做**内部可变性**：工具执行签名是 `&ToolCtx`（多轮循环里
    /// 不与会话状态纠缠），而 MCP 连接是有状态、需惰性建立的。
    /// ne-agent 是单线程（std 阻塞 IO，无 tokio），RefCell 无并发风险。
    pub mcp: Option<std::cell::RefCell<crate::mcp::McpRegistry>>,
    /// 当前会话 id（plan/scratchpad 按会话隔离）。
    pub session: String,
    /// 计划/草稿存储（None = 未装配 → 相关工具报错，不 panic）。
    pub context: Option<std::cell::RefCell<crate::context_tools::ContextStore>>,
    /// 会话库路径（子智能体/双智能体建隔离子会话用）。
    pub db_path: Option<PathBuf>,
    /// vault 根（load_rule/load_skill 读 `<vault>/rules`、`<vault>/skills`）。
    pub vault_root: Option<PathBuf>,
    /// 运行时配置（子智能体/双智能体派生 ctx 用）。
    pub cfg: Option<std::sync::Arc<crate::config::RuntimeConfig>>,
    /// prompt 目录（vault 的 prompts/；子 ctx 继承）。
    pub prompt_dir: Option<PathBuf>,
    /// 模型参数（双智能体的 Planner/Executor 调用用）。
    pub llm: Option<ne_engine::llm::LlmParams>,
    /// 已启用工具集（子 ctx 继承，防止越权）。
    pub enabled: Vec<Tool>,
    /// 对外交互回调（ask_user 用；与审批回调同一通道风格）。
    pub asker: Option<AskerFn>,
}

/// 对外交互回调签名（ask_user 用）。
pub type AskerFn = fn(&str) -> Result<String, String>;

impl ToolCtx {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        ToolCtx {
            root: root.into(),
            mcp: None,
            session: String::new(),
            context: None,
            db_path: None,
            cfg: None,
            prompt_dir: None,
            llm: None,
            enabled: Vec::new(),
            asker: None,
            vault_root: None,
        }
    }

    /// 附加会话 id + 计划/草稿存储。
    pub fn with_session(
        mut self,
        session: impl Into<String>,
        ctx: crate::context_tools::ContextStore,
    ) -> Self {
        self.session = session.into();
        self.context = Some(std::cell::RefCell::new(ctx));
        self
    }

    /// 附加 MCP 注册表。
    pub fn with_mcp(mut self, reg: crate::mcp::McpRegistry) -> Self {
        self.mcp = Some(std::cell::RefCell::new(reg));
        self
    }

    /// 附加子智能体/双智能体装配所需的运行时上下文。
    pub fn with_runtime(
        mut self,
        db_path: PathBuf,
        cfg: std::sync::Arc<crate::config::RuntimeConfig>,
        prompt_dir: PathBuf,
        enabled: Vec<Tool>,
    ) -> Self {
        self.db_path = Some(db_path);
        self.cfg = Some(cfg);
        self.prompt_dir = Some(prompt_dir);
        self.enabled = enabled;
        self
    }

    /// 附加 vault 根（load_rule/load_skill 需要）。
    pub fn with_vault_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.vault_root = Some(root.into());
        self
    }

    /// 附加模型参数（双智能体用）。
    pub fn with_llm(mut self, llm: ne_engine::llm::LlmParams) -> Self {
        self.llm = Some(llm);
        self
    }

    /// 挂上 ask_user 的交互回调（交互式入口用）。
    pub fn with_asker(mut self, f: AskerFn) -> Self {
        self.asker = Some(f);
        self
    }

    fn root_canon(&self) -> Result<PathBuf> {
        std::fs::create_dir_all(&self.root)?;
        Ok(std::fs::canonicalize(&self.root)?)
    }

    /// 把用户给的相对路径解析成**工作区内**的绝对路径。
    ///
    /// 安全规则（沙箱的关键）：
    ///   * 拒绝绝对路径、拒绝 `..` 逃逸、拒绝符号链接指到区外
    ///   * 目标不必存在（写新文件），此时校验其**最近的已存在祖先**
    pub fn resolve(&self, rel: &str) -> Result<PathBuf, String> {
        if rel.trim().is_empty() {
            return Err("路径为空".into());
        }
        let p = Path::new(rel);
        if p.is_absolute() {
            return Err(format!("禁止绝对路径：{rel}"));
        }
        for c in p.components() {
            if matches!(c, Component::ParentDir) {
                return Err(format!("禁止 `..`（越出工作区）：{rel}"));
            }
        }
        let root = self.root_canon().map_err(|e| format!("工作区不可用：{e}"))?;
        let joined = root.join(p);

        // 已存在 → 直接规范化后判前缀
        if joined.exists() {
            let c = std::fs::canonicalize(&joined)
                .map_err(|e| format!("路径解析失败 {rel}：{e}"))?;
            return if c.starts_with(&root) {
                Ok(c)
            } else {
                Err(format!("越出工作区：{rel}"))
            };
        }

        // 不存在 → 校验最近的已存在祖先仍在区内
        let mut anc = joined.clone();
        while !anc.exists() {
            match anc.parent() {
                Some(par) => anc = par.to_path_buf(),
                None => return Err(format!("无法解析路径：{rel}")),
            }
        }
        let anc_c = std::fs::canonicalize(&anc).map_err(|e| format!("祖先解析失败：{e}"))?;
        if !anc_c.starts_with(&root) {
            return Err(format!("越出工作区：{rel}"));
        }
        Ok(joined)
    }
}

/// 执行一个工具。参数用 `&str` 取值（由 harness 从 JSON 取出后传入）。
///
/// 返回 `Ok(ToolResult)` 表示"工具跑了"（成功或失败都在 output 里）；
/// 只有框架级故障才返回 `Err`。
    pub fn execute(ctx: &ToolCtx, tool: Tool, args: &ToolArgs) -> Result<ToolResult> {
    Ok(match tool {
        Tool::Read => read(ctx, args),
        Tool::Write => write(ctx, args),
        Tool::Edit => edit(ctx, args),
        Tool::Ls => ls(ctx, args),
        Tool::Base => base(ctx, args),
        Tool::WebFetch => web_fetch(args),
        Tool::WebSearch => web_search(args),
        Tool::Mcp => mcp_call(ctx, args),
        Tool::Plan => plan_tool(ctx, args),
        Tool::Scratchpad => scratchpad_tool(ctx, args),
        Tool::AskUser => ask_user(ctx, args),
        Tool::LoadRule => load_rule_tool(ctx, args),
        Tool::LoadSkill => load_skill_tool(ctx, args),
        Tool::Subagents => subagents_tool(ctx, args),
        Tool::DualAgent => dual_agent_tool(ctx, args),
    })
}


/// 轻量参数袋（避免工具签名依赖具体 JSON 库）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolArgs {
    map: std::collections::BTreeMap<String, String>,
}

impl ToolArgs {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn set(mut self, k: &str, v: impl Into<String>) -> Self {
        self.map.insert(k.to_string(), v.into());
        self
    }
    pub fn get(&self, k: &str) -> Option<&str> {
        self.map.get(k).map(|s| s.as_str())
    }
    /// 除某键外，其余转成 JSON 对象（MCP 透传实参用）。
    ///
    /// 值尝试按 JSON 解析（数字/布尔/数组/对象原样），失败则当字符串——
    /// 这样 MCP 工具能收到正确的类型，而不是全被当成字符串。
    pub fn to_json_except(&self, exclude: &str) -> serde_json::Value {
        let mut obj = serde_json::Map::new();
        for (k, v) in &self.map {
            if k == exclude {
                continue;
            }
            let val = serde_json::from_str::<serde_json::Value>(v)
                .unwrap_or_else(|_| serde_json::Value::String(v.clone()));
            obj.insert(k.clone(), val);
        }
        serde_json::Value::Object(obj)
    }
    pub(super) fn need(&self, k: &str) -> Result<&str, ToolResult> {
        self.get(k).filter(|s| !s.is_empty()).ok_or_else(|| {
            ToolResult::err(format!("缺少参数 `{k}`"))
        })
    }
}
