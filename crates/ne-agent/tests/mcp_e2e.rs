//! MCP 客户端端到端测试（用 tests/fake_mcp_server.py 驱动）。

use ne_agent::mcp::{split_qualified, McpConnection, McpRegistry};
use ne_agent::config::McpServerCfg;
use serde_json::json;

fn script() -> String {
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake_mcp_server.py").to_string()
}

fn have_python() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[test]
fn e2e_connect_list_and_call() {
    if !have_python() {
        eprintln!("跳过：无 python3");
        return;
    }
    let mut c = McpConnection::connect("fake", "python3", &[script()], None)
        .expect("应能连上假 MCP 服务");
    assert_eq!(c.name(), "fake");
    assert_eq!(c.server_info["name"], "fake");

    let names = c.qualified_tool_names();
    assert_eq!(names, vec!["mcp__fake__echo"]);

    let out = c.call_tool("echo", json!({"msg": "残响之河"})).unwrap();
    assert!(out.contains("残响之河"), "{out}");

    // 调不存在的工具 → 服务返回 error → 客户端应报错
    assert!(c.call_tool("nope", json!({})).is_err());
}

#[test]
fn e2e_registry_lazy_and_route() {
    if !have_python() {
        return;
    }
    let specs = vec![McpServerCfg {
        name: "fake".into(),
        command: "python3".into(),
        args: vec![script()],
        cwd: None,
        enabled: true,
    }];
    let mut reg = McpRegistry::new(specs);
    assert!(!reg.is_empty());

    // 惰性：构造时未连，discover_all 会连
    let tools = reg.discover_all();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].1.name, "echo");

    let out = reg.call("fake", "echo", json!({"msg": "hi"})).unwrap();
    assert!(out.contains("hi"), "{out}");

    // 未知服务
    assert!(reg.call("ghost", "echo", json!({})).is_err());
}

#[test]
fn e2e_tool_layer_routes_mcp() {
    if !have_python() {
        return;
    }
    use ne_agent::tool::{self, MCP_TOOL_KEY, Tool, ToolArgs, ToolCtx};
    let specs = vec![McpServerCfg {
        name: "fake".into(),
        command: "python3".into(),
        args: vec![script()],
        cwd: None,
        enabled: true,
    }];
    let ctx = ToolCtx::new(std::env::temp_dir()).with_mcp(McpRegistry::new(specs));

    // 名字解析：mcp__fake__echo → Tool::Mcp
    let t = Tool::from_name("mcp__fake__echo").unwrap();
    assert_eq!(t, Tool::Mcp);

    let args = ToolArgs::new()
        .set(MCP_TOOL_KEY, "mcp__fake__echo")
        .set("msg", "从工具层调用");
    let r = tool::execute(&ctx, Tool::Mcp, &args).unwrap();
    assert!(r.ok, "{}", r.output);
    assert!(r.output.contains("从工具层调用"), "{}", r.output);

    // 格式错误的名字 → 明确报错（不 panic）
    let bad = ToolArgs::new().set(MCP_TOOL_KEY, "mcp__broken");
    let r2 = tool::execute(&ctx, Tool::Mcp, &bad).unwrap();
    assert!(!r2.ok);
    assert!(r2.output.contains("格式"), "{}", r2.output);
}

#[test]
fn e2e_mcp_without_registry_errors_cleanly() {
    use ne_agent::tool::{self, MCP_TOOL_KEY, Tool, ToolArgs, ToolCtx};
    let ctx = ToolCtx::new(std::env::temp_dir()); // 无 registry
    let args = ToolArgs::new().set(MCP_TOOL_KEY, "mcp__x__y");
    let r = tool::execute(&ctx, Tool::Mcp, &args).unwrap();
    assert!(!r.ok);
    assert!(r.output.contains("MCP"), "{}", r.output);
}

#[test]
fn split_qualified_still_ok() {
    assert_eq!(
        split_qualified("mcp__srv__tool"),
        Some(("srv".into(), "tool".into()))
    );
}
