//! 能力代理工具 —— `use_capability`（学太墟 v0.22.0 的同名对外形态）。
//!
//! # 为什么需要它（**这是 MCP 可用性缺口的正解，不是把 schema 塞进 tools**）
//!
//! ne-agent 原先：MCP 工具名是动态的（`mcp__<服务>__<工具>`），既不进 provider 的
//! 原生 `tools` 数组，模型也无从得知某服务到底有哪些工具、参数长什么样——
//! 于是 NATIVE 协议下模型**根本不会输出**这个调用。"按需发现"的入口是缺的。
//!
//! 太墟 v0.22.0 的解法（`ProviderClient.kt` 原注释：「MCP 工具 schema 不再进入
//! provider 可见面：模型经 use_capability 的 list/inspect 发现能力、call 调用；
//! 服务器清单变化不再击穿前缀缓存」）正是补这个入口：
//!
//!   * 本工具 schema **静态**（只有 action/server/tool/arguments 四个键）→
//!     进 tools 数组不会因 MCP 服务清单变化而击穿 KV prefix cache；
//!   * 模型三步按需发现与调用：`list` → `inspect`（拿到工具全名 + 参数 schema）→ `call`；
//!   * 未连接的服务在 `call`/`inspect` 时**自动启动并发现**（与 [`crate::mcp`] 一致）。
//!
//! 与太墟的差异：太墟把宿主低频能力域（虚拟屏等）也并入同一代理；ne-agent 无宿主
//! 能力域，故 [`SERVER_ID`] 收到明确提示而非静默失败。

use super::*;

/// 宿主能力域 id（太墟 `HostCapabilityDirectory.SERVER_ID`）。
///
/// ne-agent 没有宿主能力域，收到该 id 时给出明确提示，而不是让模型对着
/// "未知服务"发呆。
pub const HOST_SERVER_ID: &str = "host";

/// `use_capability` 的三个动作。
const ACTIONS: [&str; 3] = ["list", "inspect", "call"];

pub(super) fn capability_tool(ctx: &ToolCtx, a: &ToolArgs) -> ToolResult {
    let action = a.get("action").unwrap_or("").trim();
    match action {
        "" => ToolResult::err(format!(
            "use_capability 缺少 action；可用：{}",
            ACTIONS.join(" / ")
        )),
        "list" => list(ctx),
        "inspect" => inspect(ctx, a),
        "call" => call(ctx, a),
        other => ToolResult::err(format!(
            "未知 action `{other}`；可用：{}",
            ACTIONS.join(" / ")
        )),
    }
}

/// `list`：列出已启用的 MCP 服务——**不启动任何进程**（太墟 list 同语义）。
fn list(ctx: &ToolCtx) -> ToolResult {
    let Some(reg) = ctx.mcp.as_ref() else {
        return ToolResult::ok(
            "当前没有已启用的 MCP 服务（在 runtime.yaml 的 mcp.servers 配置后可用）。",
        );
    };
    let reg = reg.borrow();
    if reg.is_empty() {
        return ToolResult::ok(
            "当前没有已启用的 MCP 服务（在 runtime.yaml 的 mcp.servers 配置后可用）。",
        );
    }
    let names = reg.server_names();
    let mut out = String::from("已启用的 MCP 服务（用 inspect + server 查看某服务的工具清单）：\n");
    for n in &names {
        out.push_str(&format!("- `{n}`\n"));
    }
    out.push_str("\n下一步：action=\"inspect\" + server=\"<服务名>\" 查看其工具名与参数，再 action=\"call\" 调用。");
    ToolResult::ok(out)
}

/// `inspect`：查看某服务的工具清单与参数说明——**未连接则按需连接一次**。
///
/// 太墟注释：这是唯一会拉起进程的 inspect 场景——模型无从得知未连接服务的工具名，
/// 必须给它完整清单才能构造 call。
fn inspect(ctx: &ToolCtx, a: &ToolArgs) -> ToolResult {
    let server = a.get("server").unwrap_or("").trim();
    if server.is_empty() {
        return ToolResult::err("inspect 需要 server 参数（先用 action=\"list\" 查看可用的服务名）");
    }
    if server == HOST_SERVER_ID {
        return ToolResult::ok(
            "`host` 是太墟的宿主能力域（设置/应用管理/屏幕自动化等），ne-agent 未提供该能力域。\
             请用 action=\"list\" 查看本机已启用的 MCP 服务。",
        );
    }
    let Some(reg) = ctx.mcp.as_ref() else {
        return ToolResult::err("未配置任何 MCP 服务（runtime.yaml 的 mcp.servers）");
    };
    let mut out = String::new();
    match reg.borrow_mut().discover(server) {
        Ok(()) => {}
        Err(e) => return ToolResult::err(format!("发现 MCP 服务 `{server}` 失败：{e}")),
    }
    // 借用分离：先取需要的数据，再拼接（避免 borrow 在格式化时仍存活）
    let tools = {
        let reg = reg.borrow();
        reg.tools_of(server)
    };
    if tools.is_empty() {
        return ToolResult::ok(format!("MCP 服务 `{server}` 没有可调用工具。"));
    }
    out.push_str(&format!(
        "MCP 服务 `{server}` 的工具清单（调用：action=\"call\" + server=\"{server}\" + tool=… + arguments={{…}}）：\n"
    ));
    for t in &tools {
        out.push_str(&format!("\n### {}\n", t.name));
        if !t.description.is_empty() {
            out.push_str(&format!("{}\n", t.description));
        }
        out.push_str(&format!("参数 schema：{}\n", t.input_schema));
    }
    ToolResult::ok(out)
}

/// `call`：调用某服务的某工具。未连接的服务在此按需启动并发现。
fn call(ctx: &ToolCtx, a: &ToolArgs) -> ToolResult {
    let server = a.get("server").unwrap_or("").trim();
    let tool = a.get("tool").unwrap_or("").trim();
    if server.is_empty() {
        return ToolResult::err("call 需要 server 参数（先用 action=\"list\" 查看服务名）");
    }
    if tool.is_empty() {
        return ToolResult::err(
            "call 需要 tool 参数（先用 action=\"inspect\" + server 查看该服务的工具名）",
        );
    }
    if server == HOST_SERVER_ID {
        return ToolResult::err(
            "`host` 是太墟的宿主能力域，ne-agent 未提供；请用本机已启用的 MCP 服务。",
        );
    }
    let Some(reg) = ctx.mcp.as_ref() else {
        return ToolResult::err("未配置任何 MCP 服务（runtime.yaml 的 mcp.servers）");
    };
    // `arguments` 以字符串形存入（见 protocol::resolve），解析回 JSON 对象。
    let arguments = parse_arguments(a.get("arguments"));
    match reg.borrow_mut().call(server, tool, arguments) {
        Ok(out) => ToolResult::ok(out),
        Err(e) => ToolResult::err(format!("MCP 调用失败（{server}/{tool}）：{e}")),
    }
}

/// 把 `arguments` 字符串解析成 JSON 对象；缺省/非法 → 空对象。
///
/// 容忍模型把 arguments 写成一个 JSON 字符串（双重编码）——解得开就再解一层。
fn parse_arguments(raw: Option<&str>) -> serde_json::Value {
    let Some(s) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return serde_json::json!({});
    };
    match serde_json::from_str::<serde_json::Value>(s) {
        Ok(serde_json::Value::Object(o)) => serde_json::Value::Object(o),
        Ok(serde_json::Value::String(inner)) => {
            // 双重编码：内层还是 JSON 字符串
            serde_json::from_str::<serde_json::Value>(&inner)
                .ok()
                .filter(|v| v.is_object())
                .unwrap_or_else(|| serde_json::json!({}))
        }
        _ => serde_json::json!({}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_action_reports_available() {
        let ctx = ToolCtx::new(std::env::temp_dir());
        let r = capability_tool(&ctx, &ToolArgs::new());
        assert!(!r.ok);
        assert!(r.output.contains("action"), "{}", r.output);
        assert!(r.output.contains("list"), "{}", r.output);
    }

    #[test]
    fn unknown_action_lists_actions() {
        let ctx = ToolCtx::new(std::env::temp_dir());
        let r = capability_tool(&ctx, &ToolArgs::new().set("action", "bogus"));
        assert!(!r.ok);
        assert!(r.output.contains("inspect"), "{}", r.output);
    }

    #[test]
    fn list_without_registry_is_ok() {
        // 未装配 MCP 注册表：list 应成功并说明"没有已启用服务"，而不是报错
        let ctx = ToolCtx::new(std::env::temp_dir());
        let r = capability_tool(&ctx, &ToolArgs::new().set("action", "list"));
        assert!(r.ok, "{}", r.output);
        assert!(r.output.contains("没有已启用"), "{}", r.output);
    }

    #[test]
    fn inspect_requires_server() {
        let ctx = ToolCtx::new(std::env::temp_dir());
        let r = capability_tool(&ctx, &ToolArgs::new().set("action", "inspect"));
        assert!(!r.ok);
        assert!(r.output.contains("server"), "{}", r.output);
    }

    #[test]
    fn call_requires_tool() {
        let ctx = ToolCtx::new(std::env::temp_dir());
        let r = capability_tool(
            &ctx,
            &ToolArgs::new().set("action", "call").set("server", "srv"),
        );
        assert!(!r.ok);
        assert!(r.output.contains("tool"), "{}", r.output);
    }

    #[test]
    fn host_domain_is_explained() {
        let ctx = ToolCtx::new(std::env::temp_dir());
        let r = capability_tool(
            &ctx,
            &ToolArgs::new().set("action", "inspect").set("server", "host"),
        );
        assert!(r.ok);
        assert!(r.output.contains("host"), "{}", r.output);
    }

    #[test]
    fn parse_arguments_handles_objects_and_double_encoding() {
        assert_eq!(parse_arguments(Some(r#"{"a":1}"#)), serde_json::json!({"a": 1}));
        assert_eq!(parse_arguments(None), serde_json::json!({}));
        assert_eq!(parse_arguments(Some("")), serde_json::json!({}));
        assert_eq!(parse_arguments(Some("not json")), serde_json::json!({}));
        // 双重编码：内层仍是 JSON 字符串
        let inner = serde_json::to_string(r#"{"b":2}"#).unwrap();
        assert_eq!(parse_arguments(Some(&inner)), serde_json::json!({"b": 2}));
    }
}
