//! M6 审批门禁的端到端测试：验证 harness 工具循环真的按策略拦截。
//!
//! 用假 LLM 让模型"请求调用工具"，观察结果里是不是"被审批拦下"。
//! 不依赖任何真实模型/网络。

use ne_agent::approval::{ApprovalMode, Decision};
use ne_agent::config::RuntimeConfig;
use ne_agent::session::SessionStore;
use std::path::PathBuf;

fn tmp(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("ne-agent-approval-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// 启动一个假 LLM：第一次回**原生 tool_calls**，其后（看到 role=tool 结果）回纯文本收尾。
/// 返回端口。
///
/// 用原生 `tool_calls` 而非文本标记：这正是真实 provider 的形态，
/// 也让测试覆盖「结构化通道优先」的新协议（对齐太墟 NATIVE 模式）。
fn start_fake_llm(port: u16, tool_name: &str, tool_args: &str) -> std::process::Child {
    let script = format!(
        r#"
import http.server, json
TOOL = {tool_name:?}
ARGS = json.loads({tool_args:?})
class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get('Content-Length', 0))
        body = json.loads(self.rfile.read(n) or b'{{}}')
        msgs = body.get('messages', [])
        has_tool_result = any(m.get('role') == 'tool' for m in msgs)
        if has_tool_result:
            last = [m for m in msgs if m.get('role') == 'tool'][-1].get('content', '')
            msg = {{"role": "assistant", "content": "收尾：" + last[:200]}}
            resp = {{"choices": [{{"message": msg, "finish_reason": "stop"}}]}}
        else:
            msg = {{"role": "assistant", "content": "",
                    "tool_calls": [{{"id": "call_fake_1", "type": "function",
                                     "function": {{"name": TOOL, "arguments": json.dumps(ARGS)}}}}]}}
            resp = {{"choices": [{{"message": msg, "finish_reason": "tool_calls"}}]}}
        data = json.dumps(resp).encode()
        self.send_response(200); self.send_header('Content-Type','application/json')
        self.send_header('Content-Length', str(len(data))); self.end_headers(); self.wfile.write(data)
    def log_message(self, *a): pass
http.server.HTTPServer(('127.0.0.1', {port}), H).serve_forever()
"#,
        tool_name = tool_name,
        tool_args = tool_args,
        port = port,
    );
    std::process::Command::new("python3")
        .arg("-c")
        .arg(script)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("启动假 LLM 失败")
}

fn wait_port(port: u16) {
    for _ in 0..50 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(60));
    }
}

fn llm_for(port: u16, cfg: &RuntimeConfig, dir: &std::path::Path) -> ne_engine::llm::LlmParams {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("models.yml"),
        format!(
            "providers:\n  demo:\n    baseUrl: http://127.0.0.1:{port}/v1\n    api: openai-completions\n    task:\n      keyPool:\n        - \"sk-fake-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n    models:\n      - id: demo-chat-1\n"
        ),
    )
    .unwrap();
    std::fs::write(dir.join("config.yml"), "modelRoles:\n  default: demo/demo-chat-1\n").unwrap();
    std::env::set_var("TAIXU_LLM_DIR", dir);
    cfg.llm_params(dir).expect("构造 llm params")
}

/// 一次 chat，返回（模型最终文本）。审批回调由调用方给。
fn do_chat(
    work: &std::path::Path,
    port: u16,
    tool: &str,
    args: &str,
    mode: ApprovalMode,
    plan_only: bool,
    approve: Option<bool>,
) -> String {
    let mut cfg = RuntimeConfig::default();
    cfg.tools.enabled = vec![tool.into()];
    cfg.approval.mode = mode.id().into();
    cfg.approval.plan_only = plan_only;
    let llm_dir = work.join("llm");
    let llm = llm_for(port, &cfg, &llm_dir);
    let store = SessionStore::open(work.join("agent.db")).unwrap();
    let sid = store.create_session("t").unwrap();
    let enabled = ne_agent::resolve_enabled_tools(&cfg).unwrap();
    let h = ne_agent::task::make_ctx_for_test_unlocked(
        &store, &sid, &cfg, llm, work.to_path_buf(), work.to_path_buf(), enabled,
        String::new(),
        match approve {
            Some(v) => Some(Box::new(move |_: &Decision| v)),
            None => None,
        },
    );
    ne_agent::task::run_one_for_test(&h, args).unwrap_or_else(|e| format!("ERR: {e}"))
}

#[test]
fn plan_only_blocks_write_even_in_full_access() {
    let work = tmp("plan");
    let port = 9101;
    let mut child = start_fake_llm(port, "write", r#"{"path":"out.txt","content":"x"}"#);
    wait_port(port);
    // full_access + plan_only → 仍应被拦
    let out = do_chat(&work, port, "write", r#"{"path":"out.txt","content":"x"}"#,
                      ApprovalMode::FullAccess, true, Some(true));
    let _ = child.kill();
    assert!(
        out.contains("只读规划") || out.contains("拦截"),
        "plan_only 应拦住写操作，实际：{out}"
    );
    assert!(!work.join("out.txt").exists(), "文件不该被创建");
}

#[test]
fn non_interactive_defaults_to_deny() {
    let work = tmp("deny");
    let port = 9102;
    let mut child = start_fake_llm(port, "write", r#"{"path":"out.txt","content":"x"}"#);
    wait_port(port);
    // assisted 模式 + 写工作区内 → 不需审批，会放行。
    // 这里改用 request 模式：所有可变操作需审批；无回调 → 默认拒绝
    let out = do_chat(&work, port, "write", r#"{"path":"out.txt","content":"x"}"#,
                      ApprovalMode::Request, false, None);
    let _ = child.kill();
    assert!(out.contains("审批未通过") || out.contains("默认拒绝"), "应默认拒绝：{out}");
    assert!(!work.join("out.txt").exists(), "无审批回调时不该写文件");
}

#[test]
fn approved_call_proceeds() {
    let work = tmp("approve");
    let port = 9103;
    let mut child = start_fake_llm(port, "write", r#"{"path":"ok.txt","content":"hi"}"#);
    wait_port(port);
    let out = do_chat(&work, port, "write", r#"{"path":"ok.txt","content":"hi"}"#,
                      ApprovalMode::Request, false, Some(true));
    let _ = child.kill();
    assert!(work.join("ok.txt").exists(), "批准后应写入，out={out}");
    assert_eq!(std::fs::read_to_string(work.join("ok.txt")).unwrap(), "hi");
}

#[test]
fn read_tools_never_blocked() {
    let work = tmp("read");
    std::fs::write(work.join("a.txt"), "内容").unwrap();
    let port = 9104;
    let mut child = start_fake_llm(port, "read", r#"{"path":"a.txt"}"#);
    wait_port(port);
    // request 模式 + plan_only，但 read 是只读 → 应放行
    let out = do_chat(&work, port, "read", r#"{"path":"a.txt"}"#,
                      ApprovalMode::Request, true, None);
    let _ = child.kill();
    assert!(out.contains("内容"), "只读工具应能读到内容：{out}");
}

// ═══════════════════════════════════════════════════════════════════
// 安全回归：生产装配**必须**被三重锁拦住（2026-10-18 新增）
// ═══════════════════════════════════════════════════════════════════
//
// 背景：把 8 处 `llm::call` 统一收口到 `governed_call` 之后，
// 本文件的旧测试全挂了。当时有两种修法：
//
//   ❌ 错误修法：让 `governed_call` 对 127.0.0.1 放行
//      → 用户的 ofm 网关就在 127.0.0.1，锁直接形同虚设。
//   ✅ 正确修法：给 `HarnessCtx` 加 `llm_transport` 注入位，
//      生产恒为 None（受锁），测试显式注入（不受锁）。
//
// 但"能注入"本身也带来新风险：万一某天有人在生产路径构造了 Some(...)，
// 锁就被绕过了。所以这里补一条**看门狗测试**：
// 用**生产装配** `make_ctx_for_test`（llm_transport = None）跑一轮，
// 断言它被锁拦住、且**没有向假 LLM 发出任何请求**。
//
// 这条测试是「注入通道没被滥用」的保险丝，改坏了会立刻红。
#[test]
fn production_ctx_is_locked_and_makes_no_request() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let work = tmp("locked");
    let port = 9105;
    // 假 LLM 若被请求会自增计数
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_srv = hits.clone();
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let srv = std::thread::spawn(move || {
        // 只数连接，不实现协议 —— 一旦有连接就说明出网了
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            if let Ok((_s, _)) = listener.accept() {
                hits_srv.fetch_add(1, Ordering::SeqCst);
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    });

    let mut cfg = RuntimeConfig::default();
    cfg.tools.enabled = vec!["write".into()];
    cfg.approval.mode = ApprovalMode::FullAccess.id().into();
    let llm_dir = work.join("llm");
    let llm = llm_for(port, &cfg, &llm_dir);
    let store = SessionStore::open(work.join("agent.db")).unwrap();
    let sid = store.create_session("t").unwrap();
    let enabled = ne_agent::resolve_enabled_tools(&cfg).unwrap();

    // ⚠️ 生产装配：llm_transport = None
    let h = ne_agent::task::make_ctx_for_test(
        &store, &sid, &cfg, llm, work.to_path_buf(), work.to_path_buf(), enabled,
        String::new(), None,
    );
    let out = ne_agent::task::run_one_for_test(&h, "写个文件").unwrap_or_default();
    srv.join().unwrap();

    assert!(
        out.contains("三重锁") || out.contains("模型调用失败"),
        "生产装配应被三重锁拦住，实际输出：{out}"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst), 0,
        "锁必须让它**连一个 TCP 连接都不发**，实际连了 {} 次",
        hits.load(Ordering::SeqCst)
    );
    assert!(!work.join("out.txt").exists(), "被拦后不应写入任何文件");
}
