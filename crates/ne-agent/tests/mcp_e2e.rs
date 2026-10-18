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

/// `use_capability` 端到端：list → inspect → call 三步按需发现与调用。
///
/// 这是 MCP 可用性缺口的正解——模型不必预先知道工具名，就能经代理发现并调用。
#[test]
fn e2e_use_capability_discover_and_call() {
    if !have_python() {
        return;
    }
    use ne_agent::tool::{self, Tool, ToolArgs, ToolCtx};
    let specs = vec![McpServerCfg {
        name: "fake".into(),
        command: "python3".into(),
        args: vec![script()],
        cwd: None,
        enabled: true,
    }];
    let ctx = ToolCtx::new(std::env::temp_dir()).with_mcp(McpRegistry::new(specs));

    // 名字解析：use_capability → Tool::Capability（无 registry 也可解析）
    assert_eq!(Tool::from_name("use_capability"), Some(Tool::Capability));

    // 1) list：不启动进程也要能列出服务名
    let r = tool::execute(&ctx, Tool::Capability, &ToolArgs::new().set("action", "list")).unwrap();
    assert!(r.ok, "{}", r.output);
    assert!(r.output.contains("fake"), "list 应含服务名：{}", r.output);

    // 2) inspect：按需连接并列出工具名与参数 schema
    let r = tool::execute(
        &ctx,
        Tool::Capability,
        &ToolArgs::new().set("action", "inspect").set("server", "fake"),
    )
    .unwrap();
    assert!(r.ok, "{}", r.output);
    assert!(r.output.contains("echo"), "inspect 应含工具名：{}", r.output);

    // 3) call：直传实参，落到服务
    let r = tool::execute(
        &ctx,
        Tool::Capability,
        &ToolArgs::new()
            .set("action", "call")
            .set("server", "fake")
            .set("tool", "echo")
            .set("arguments", r#"{"msg":"代理调用"}"#),
    )
    .unwrap();
    assert!(r.ok, "{}", r.output);
    assert!(r.output.contains("代理调用"), "{}", r.output);
}

/// `use_capability` 的只读发现动作（list/inspect）在 request 模式下也应免审，
/// 只有 call 才要求批准——避免"看一眼清单"就弹审批卡。
#[test]
fn e2e_use_capability_approval_matrix() {
    use ne_agent::approval::{decide, ApprovalMode, plan_block};
    use ne_agent::tool::{Tool, ToolArgs};
    let ws = std::path::Path::new("/tmp");

    let list = ToolArgs::new().set("action", "list");
    assert!(!decide(ApprovalMode::Request, Tool::Capability, &list, ws, None).required);

    let inspect = ToolArgs::new().set("action", "inspect").set("server", "fake");
    assert!(!decide(ApprovalMode::Request, Tool::Capability, &inspect, ws, None).required);

    let call = ToolArgs::new()
        .set("action", "call")
        .set("server", "fake")
        .set("tool", "echo");
    assert!(decide(ApprovalMode::Request, Tool::Capability, &call, ws, None).required);
    assert!(decide(ApprovalMode::Assisted, Tool::Capability, &call, ws, None).required);

    // 只读规划：list/inspect 放行，call 硬拒
    assert!(plan_block(Tool::Capability, &list).is_none());
    assert!(plan_block(Tool::Capability, &inspect).is_none());
    assert!(plan_block(Tool::Capability, &call).is_some());
}

/// ── 全链路：假 LLM → harness 循环 → use_capability 代理 → MCP 服务 ──
///
/// 这条覆盖的是**可用性缺口的完整闭环**：模型不必预知 MCP 工具名，只发一个
/// `use_capability` 调用，经 harness 的工具循环落到代理、代理再连上 MCP 服务并
/// 真正执行 `echo`。任何一环断掉（工具未启用 / 没接 registry / 代理没分发）都会红。
#[test]
fn e2e_harness_model_calls_use_capability_to_reach_mcp() {
    if !have_python() {
        eprintln!("跳过：无 python3");
        return;
    }
    use ne_agent::config::RuntimeConfig;
    use ne_agent::session::SessionStore;

    // 1) 假 LLM：只声明调用内置工具 use_capability（不去猜 mcp__fake__echo）
    let port = 9155;
    let llm_script = format!(
        r#"
import http.server, json
ARGS = {{"action": "call", "server": "fake", "tool": "echo", "arguments": "{{\"msg\":\"残响之河\"}}"}}
class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get('Content-Length', 0))
        body = json.loads(self.rfile.read(n) or b'{{}}')
        msgs = body.get('messages', [])
        has_tool_result = any(m.get('role') == 'tool' for m in msgs)
        if has_tool_result:
            last = [m for m in msgs if m.get('role') == 'tool'][-1].get('content', '')
            msg = {{"role": "assistant", "content": "最终答案：" + last[:200]}}
            resp = {{"choices": [{{"message": msg, "finish_reason": "stop"}}]}}
        else:
            msg = {{"role": "assistant", "content": "",
                    "tool_calls": [{{"id": "call_cap_1", "type": "function",
                                     "function": {{"name": "use_capability",
                                                  "arguments": json.dumps(ARGS)}}}}]}}
            resp = {{"choices": [{{"message": msg, "finish_reason": "tool_calls"}}]}}
        data = json.dumps(resp).encode()
        self.send_response(200); self.send_header('Content-Type','application/json')
        self.send_header('Content-Length', str(len(data))); self.end_headers(); self.wfile.write(data)
    def log_message(self, *a): pass
http.server.HTTPServer(('127.0.0.1', {port}), H).serve_forever()
"#
    );
    let mut llm = std::process::Command::new("python3")
        .arg("-c")
        .arg(&llm_script)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("启动假 LLM 失败");
    for _ in 0..50 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(60));
    }

    // 2) 真 harness：配 fake MCP 服务 + 默认 enabled（**不含** use_capability）
    let work = std::env::temp_dir().join(format!("ne-agent-mcp-harness-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    let mut cfg = RuntimeConfig::default();
    cfg.mcp.servers.push(McpServerCfg {
        name: "fake".into(),
        command: "python3".into(),
        args: vec![script()],
        cwd: None,
        enabled: true,
    });
    cfg.approval.mode = "full_access".into();

    let llm_dir = work.join("llm");
    std::fs::create_dir_all(&llm_dir).unwrap();
    std::fs::write(
        llm_dir.join("models.yml"),
        format!(
            "providers:\n  demo:\n    baseUrl: http://127.0.0.1:{port}/v1\n    api: openai-completions\n    task:\n      keyPool:\n        - \"[REDACTED]\"\n    models:\n      - id: demo-chat-1\n"
        ),
    )
    .unwrap();
    std::fs::write(llm_dir.join("config.yml"), "modelRoles:\n  default: demo/demo-chat-1\n").unwrap();
    std::env::set_var("TAIXU_LLM_DIR", &llm_dir);
    let llm_params = cfg.llm_params(&llm_dir).expect("构造 llm params");

    // 3) 关键断言：由**配置自动接线**出的工具面必须含 use_capability
    let enabled = ne_agent::resolve_enabled_tools(&cfg).unwrap();
    assert!(
        enabled.contains(&ne_agent::tool::Tool::Capability),
        "配了 MCP 服务就应自动启用 use_capability（否则模型根本调不到 MCP）"
    );

    let store = SessionStore::open(work.join("agent.db")).unwrap();
    let sid = store.create_session("t").unwrap();
    let h = ne_agent::task::make_ctx_for_test_unlocked(
        &store,
        &sid,
        &cfg,
        llm_params,
        work.clone(),
        work.clone(),
        enabled,
        String::new(),
        None,
    );
    let out = ne_agent::task::run_one_for_test(&h, "用 echo 回显 残响之河").unwrap();

    let _ = llm.kill();
    let _ = std::fs::remove_dir_all(&work);

    // 4) 模型最终答案里应出现 MCP 服务的真实回显
    assert!(
        out.contains("echo: 残响之河"),
        "模型经 use_capability 应真正打到 MCP echo，实际：{out}"
    );
}
