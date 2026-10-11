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
        matches!(self, Tool::Plan | Tool::Scratchpad)
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
}

impl ToolCtx {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        ToolCtx {
            root: root.into(),
            mcp: None,
            session: String::new(),
            context: None,
        }
    }

    /// 附加 MCP 注册表。
    pub fn with_mcp(mut self, reg: crate::mcp::McpRegistry) -> Self {
        self.mcp = Some(std::cell::RefCell::new(reg));
        self
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
    })
}

/// `plan`：多步骤计划管理（持久化）。契约对齐太墟 `AgentContextExecutor.executePlan`。
fn plan_tool(ctx: &ToolCtx, a: &ToolArgs) -> ToolResult {
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
fn scratchpad_tool(ctx: &ToolCtx, a: &ToolArgs) -> ToolResult {
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

/// `mcp__<server>__<tool>`：路由到外部 MCP 服务。
///
/// 限定名由 harness 放在 [`MCP_TOOL_KEY`] 里传入；其余键即该工具的 arguments。
fn mcp_call(ctx: &ToolCtx, a: &ToolArgs) -> ToolResult {
    let Some(reg) = ctx.mcp.as_ref() else {
        return ToolResult::err("未配置任何 MCP 服务（runtime.yaml 的 mcp.servers）");
    };
    let Some(qualified) = a.get(MCP_TOOL_KEY) else {
        return ToolResult::err("内部错误：缺少 MCP 限定工具名");
    };
    let Some((server, tool)) = crate::mcp::split_qualified(qualified) else {
        return ToolResult::err(format!(
            "MCP 工具名格式错误：`{qualified}`；应为 `mcp__<服务>__<工具>`"
        ));
    };
    // 除内部键外，其余都是该工具的实参
    let arguments = a.to_json_except(MCP_TOOL_KEY);
    match reg.borrow_mut().call(&server, &tool, arguments) {
        Ok(out) => ToolResult::ok(out),
        Err(e) => ToolResult::err(format!("MCP 调用失败（{server}/{tool}）：{e}")),
    }
}

/// `web_fetch`：抓网页 → 纯文本。
fn web_fetch(a: &ToolArgs) -> ToolResult {
    let url = match a.need("url") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let max_chars = a
        .get("max_chars")
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(8000)
        .clamp(200, 60_000);
    match crate::web::fetch_text(url, max_chars) {
        Ok(t) => ToolResult::ok(t),
        Err(e) => ToolResult::err(format!("抓取失败：{e}")),
    }
}

/// `web_search`：多引擎搜索（失败自动回退）。
fn web_search(a: &ToolArgs) -> ToolResult {
    let query = match a.need("query") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let engine = a.get("engine").unwrap_or("baidu");
    let limit = a
        .get("limit")
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(5)
        .clamp(1, 15);
    match crate::web::search(query, engine, limit) {
        Ok(hits) if hits.is_empty() => {
            ToolResult::err(format!("「{query}」无搜索结果"))
        }
        Ok(hits) => {
            let mut s = format!("搜索「{query}」（引擎 {engine}，{} 条）\n", hits.len());
            for (i, h) in hits.iter().enumerate() {
                s.push_str(&format!("\n{}. {}\n   {}", i + 1, h.title, h.url));
                if !h.snippet.is_empty() {
                    s.push_str(&format!("\n   {}", h.snippet));
                }
            }
            ToolResult::ok(s)
        }
        Err(e) => ToolResult::err(format!("搜索失败：{e}")),
    }
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
    fn need(&self, k: &str) -> Result<&str, ToolResult> {
        self.get(k).filter(|s| !s.is_empty()).ok_or_else(|| {
            ToolResult::err(format!("缺少参数 `{k}`"))
        })
    }
}

fn read(ctx: &ToolCtx, a: &ToolArgs) -> ToolResult {
    let rel = match a.need("path") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let p = match ctx.resolve(rel) {
        Ok(p) => p,
        Err(e) => return ToolResult::err(e),
    };
    if !p.is_file() {
        return ToolResult::err(format!("不是文件或不存在：{rel}"));
    }
    match std::fs::read_to_string(&p) {
        Ok(s) => {
            // 加行号，便于后续 edit 精确匹配
            let mut out = String::new();
            for (i, line) in s.lines().enumerate() {
                out.push_str(&format!("{:>5}| {}\n", i + 1, line));
            }
            ToolResult::ok(format!("{rel}（{} 行）\n{out}", s.lines().count()))
        }
        Err(e) => ToolResult::err(format!("读 {rel} 失败：{e}")),
    }
}

fn write(ctx: &ToolCtx, a: &ToolArgs) -> ToolResult {
    let rel = match a.need("path") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let content = a.get("content").unwrap_or("");
    let p = match ctx.resolve(rel) {
        Ok(p) => p,
        Err(e) => return ToolResult::err(e),
    };
    if let Some(dir) = p.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            return ToolResult::err(format!("建目录失败 {rel}：{e}"));
        }
    }
    match std::fs::write(&p, content) {
        Ok(()) => ToolResult::ok(format!("已写入 {rel}（{} 字节）", content.len())),
        Err(e) => ToolResult::err(format!("写 {rel} 失败：{e}")),
    }
}

fn edit(ctx: &ToolCtx, a: &ToolArgs) -> ToolResult {
    let rel = match a.need("path") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let old = match a.need("old") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let new = a.get("new").unwrap_or("");
    let p = match ctx.resolve(rel) {
        Ok(p) => p,
        Err(e) => return ToolResult::err(e),
    };
    let text = match std::fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) => return ToolResult::err(format!("读 {rel} 失败：{e}")),
    };
    let n = text.matches(old).count();
    match n {
        0 => ToolResult::err(format!(
            "{rel} 中未找到要替换的文本。请先 read 该文件确认内容后再 edit，不要盲目猜测。"
        )),
        1 => {
            let out = text.replacen(old, new, 1);
            match std::fs::write(&p, out) {
                Ok(()) => ToolResult::ok(format!("已修改 {rel}")),
                Err(e) => ToolResult::err(format!("写 {rel} 失败：{e}")),
            }
        }
        k => ToolResult::err(format!(
            "{rel} 中 `old` 匹配到 {k} 处，不唯一。请提供更长的上下文使其唯一。"
        )),
    }
}

fn ls(ctx: &ToolCtx, a: &ToolArgs) -> ToolResult {
    let rel = a.get("path").unwrap_or(".");
    let p = match ctx.resolve(if rel.is_empty() { "." } else { rel }) {
        Ok(p) => p,
        Err(e) => return ToolResult::err(e),
    };
    if p.is_file() {
        return ToolResult::ok(format!("{rel} 是文件（{} 字节）", std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0)));
    }
    let rd = match std::fs::read_dir(&p) {
        Ok(r) => r,
        Err(e) => return ToolResult::err(format!("列目录失败 {rel}：{e}")),
    };
    let mut names = Vec::new();
    for e in rd.flatten() {
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        names.push(format!("{}{}", e.file_name().to_string_lossy(), if is_dir { "/" } else { "" }));
    }
    names.sort();
    ToolResult::ok(format!("{rel}/（{} 项）\n{}", names.len(), names.join("\n")))
}

fn base(ctx: &ToolCtx, a: &ToolArgs) -> ToolResult {
    let cmd = match a.need("command") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let root = match ctx.root_canon() {
        Ok(r) => r,
        Err(e) => return ToolResult::err(format!("工作区不可用：{e}")),
    };
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(&root)
        .output();
    match out {
        Ok(o) => {
            let mut s = String::new();
            s.push_str(&String::from_utf8_lossy(&o.stdout));
            let err = String::from_utf8_lossy(&o.stderr);
            if !err.trim().is_empty() {
                s.push_str("\n[stderr]\n");
                s.push_str(&err);
            }
            let code = o.status.code().unwrap_or(-1);
            if o.status.success() {
                ToolResult::ok(if s.trim().is_empty() { format!("（无输出，退出码 {code}）") } else { s.trim_end().to_string() })
            } else {
                ToolResult::err(format!("退出码 {code}\n{}", s.trim_end()))
            }
        }
        Err(e) => ToolResult::err(format!("执行失败：{e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(tag: &str) -> (ToolCtx, PathBuf) {
        let d = std::env::temp_dir().join(format!("ne-agent-tool-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        (ToolCtx::new(&d), d)
    }

    #[test]
    fn write_then_read_roundtrip() {
        let (c, d) = ctx("rw");
        let r = execute(&c, Tool::Write, &ToolArgs::new().set("path", "a.txt").set("content", "你好\n第二行"))
            .unwrap();
        assert!(r.ok, "{}", r.output);
        let r = execute(&c, Tool::Read, &ToolArgs::new().set("path", "a.txt")).unwrap();
        assert!(r.ok);
        assert!(r.output.contains("第二行"));
        assert!(r.output.contains("1| "), "read 应带行号");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn edit_requires_unique_match() {
        let (c, d) = ctx("edit");
        execute(&c, Tool::Write, &ToolArgs::new().set("path", "b.txt").set("content", "x\nx\n")).unwrap();
        // 多处匹配 → 明确报错
        let r = execute(&c, Tool::Edit, &ToolArgs::new().set("path", "b.txt").set("old", "x").set("new", "y")).unwrap();
        assert!(!r.ok);
        assert!(r.output.contains("不唯一"), "{}", r.output);
        // 唯一匹配 → 成功
        let r = execute(&c, Tool::Edit, &ToolArgs::new().set("path", "b.txt").set("old", "x\nx").set("new", "z")).unwrap();
        assert!(r.ok, "{}", r.output);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn edit_missing_text_gives_actionable_error() {
        let (c, d) = ctx("editmiss");
        execute(&c, Tool::Write, &ToolArgs::new().set("path", "c.txt").set("content", "abc")).unwrap();
        let r = execute(&c, Tool::Edit, &ToolArgs::new().set("path", "c.txt").set("old", "不存在").set("new", "x")).unwrap();
        assert!(!r.ok);
        assert!(r.output.contains("read"), "错误信息应提示先 read：{}", r.output);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn sandbox_rejects_escape() {
        let (c, d) = ctx("sandbox");
        for bad in ["../outside", "a/../../b", "/etc/passwd"] {
            let r = execute(&c, Tool::Read, &ToolArgs::new().set("path", bad)).unwrap();
            assert!(!r.ok, "{bad} 应被拒绝");
        }
        assert!(c.resolve("../x").is_err());
        assert!(c.resolve("/tmp/x").is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ls_lists_and_marks_dirs() {
        let (c, d) = ctx("ls");
        execute(&c, Tool::Write, &ToolArgs::new().set("path", "sub/f.txt").set("content", "1")).unwrap();
        // 根目录：只应看到 sub/
        let r = execute(&c, Tool::Ls, &ToolArgs::new().set("path", ".")).unwrap();
        assert!(r.ok);
        assert!(r.output.contains("sub/"), "{}", r.output);
        assert!(!r.output.contains("f.txt"), "根目录不该直接列出子目录里的文件：{}", r.output);
        // 子目录：应看到 f.txt
        let r2 = execute(&c, Tool::Ls, &ToolArgs::new().set("path", "sub")).unwrap();
        assert!(r2.ok);
        assert!(r2.output.contains("f.txt"), "{}", r2.output);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn render_handles_cjk_without_corruption() {
        // 回归：早期实现按字节索引 push char，中文被打碎成乱码
        let (c, d) = ctx("cjk");
        execute(&c, Tool::Write, &ToolArgs::new().set("path", "z.txt").set("content", "第一章 残响之河")).unwrap();
        let r = execute(&c, Tool::Read, &ToolArgs::new().set("path", "z.txt")).unwrap();
        assert!(r.ok);
        assert!(r.output.contains("残响之河"), "中文不得乱码：{}", r.output);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_param_reports_clearly() {
        let (c, _d) = ctx("miss");
        let r = execute(&c, Tool::Read, &ToolArgs::new()).unwrap();
        assert!(!r.ok);
        assert!(r.output.contains("path"));
    }

    #[test]
    fn registry_names_are_stable() {
        assert_eq!(Tool::from_name("read"), Some(Tool::Read));
        assert_eq!(Tool::from_name("web_fetch"), Some(Tool::WebFetch));
        assert_eq!(Tool::from_name("plan"), Some(Tool::Plan));
        assert_eq!(Tool::from_name("scratchpad"), Some(Tool::Scratchpad));
        assert_eq!(Tool::from_name("nope"), None);
        let names: Vec<&str> = Tool::all().iter().map(|t| t.name()).collect();
        assert_eq!(
            names,
            vec![
                "read", "write", "edit", "ls", "base", "web_fetch", "web_search", "plan",
                "scratchpad"
            ]
        );
    }

    #[test]
    fn session_local_tools_need_no_approval() {
        // plan/scratchpad 只改会话局部状态
        assert!(Tool::Plan.is_session_local());
        assert!(Tool::Scratchpad.is_session_local());
        assert_eq!(Tool::Plan.tier(), Tier::Read);
        assert_eq!(Tool::Scratchpad.tier(), Tier::Read);
        assert!(!Tool::Plan.is_network());
    }

    #[test]
    fn plan_tool_roundtrip() {
        let (c, _d) = ctx("plan");
        let cs = crate::context_tools::ContextStore::open(std::env::temp_dir().join(format!(
            "ne-agent-plan-tool-{}.db",
            std::process::id()
        )))
        .unwrap();
        let c = c.with_session("S-plan", cs);
        // 创建
        let a = ToolArgs::new()
            .set("action", "replace_active")
            .set("goal", "写完整卷")
            .set("steps", r#"[{"id":"1"}]"#);
        let r = execute(&c, Tool::Plan, &a).unwrap();
        assert!(r.ok, "{}", r.output);
        assert!(r.output.contains("写完整卷"), "{}", r.output);
        // 读回
        let r = execute(&c, Tool::Plan, &ToolArgs::new().set("action", "get_active")).unwrap();
        assert!(r.ok);
        assert!(r.output.contains("写完整卷"), "{}", r.output);
        // 非法状态不写坏
        let a = ToolArgs::new().set("action", "advance").set("status", "第一阶段完成");
        let r = execute(&c, Tool::Plan, &a).unwrap();
        assert!(r.ok, "{}", r.output);
        assert!(r.output.contains("active"), "非法状态应保留 active：{}", r.output);
        // 清空
        let r = execute(&c, Tool::Plan, &ToolArgs::new().set("action", "clear_active")).unwrap();
        assert!(r.ok);
        let r = execute(&c, Tool::Plan, &ToolArgs::new().set("action", "get_active")).unwrap();
        assert!(r.output.contains("暂无"), "{}", r.output);
    }

    #[test]
    fn plan_tool_without_context_errors_cleanly() {
        let (c, _d) = ctx("plan-noctx");
        let a = ToolArgs::new().set("action", "get_active");
        let r = execute(&c, Tool::Plan, &a).unwrap();
        assert!(!r.ok);
        assert!(r.output.contains("plan"), "{}", r.output);
    }

    #[test]
    fn scratchpad_tool_roundtrip() {
        let (c, _d) = ctx("scratch");
        let cs = crate::context_tools::ContextStore::open(std::env::temp_dir().join(format!(
            "ne-agent-scratch-tool-{}.db",
            std::process::id()
        )))
        .unwrap();
        let c = c.with_session("S-scratch", cs);
        let r = execute(
            &c,
            Tool::Scratchpad,
            &ToolArgs::new().set("action", "save").set("key", "假说").set("value", "DNS 坏了"),
        )
        .unwrap();
        assert!(r.ok, "{}", r.output);
        let r = execute(
            &c,
            Tool::Scratchpad,
            &ToolArgs::new().set("action", "get").set("key", "假说"),
        )
        .unwrap();
        assert!(r.output.contains("DNS 坏了"), "{}", r.output);
        let r = execute(
            &c,
            Tool::Scratchpad,
            &ToolArgs::new().set("action", "list"),
        )
        .unwrap();
        assert!(r.output.contains("假说"), "{}", r.output);
    }

    #[test]
    fn scratchpad_missing_key_is_clear_error() {
        let (c, _d) = ctx("scratch-bad");
        let cs = crate::context_tools::ContextStore::open(std::env::temp_dir().join(format!(
            "ne-agent-scratch-bad-{}.db",
            std::process::id()
        )))
        .unwrap();
        let c = c.with_session("S", cs);
        let r = execute(
            &c,
            Tool::Scratchpad,
            &ToolArgs::new().set("action", "save").set("value", "v"),
        )
        .unwrap();
        assert!(!r.ok);
        assert!(r.output.contains("key"), "{}", r.output);
    }

    #[test]
    fn network_tools_are_read_tier_and_flagged() {
        // 网络工具是只读 tier，但被 is_network 标记（M6 审批用）
        assert_eq!(Tool::WebFetch.tier(), Tier::Read);
        assert_eq!(Tool::WebSearch.tier(), Tier::Read);
        assert!(Tool::WebFetch.is_network());
        assert!(Tool::WebSearch.is_network());
        assert!(!Tool::Read.is_network());
        assert!(!Tool::Base.is_network());
    }

    #[test]
    fn web_search_reports_missing_query() {
        let (c, _d) = ctx("ws");
        let r = execute(&c, Tool::WebSearch, &ToolArgs::new()).unwrap();
        assert!(!r.ok);
        assert!(r.output.contains("query"), "{}", r.output);
    }
}
