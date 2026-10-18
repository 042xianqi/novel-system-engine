//! 子智能体 Lane 的端到端测试：验证顺序执行 + 子会话隔离 + 写租约。
//!
//! 用假 LLM 让每个子任务回固定文本；不依赖真实模型/网络。

use ne_agent::subagent::{check_write_leases, parse_subagents, SubagentTask, DEFAULT_MAX_TASKS};
use ne_agent::config::RuntimeConfig;
use ne_agent::session::SessionStore;
use serde_json::json;
use std::path::PathBuf;

fn tmp(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "ne-agent-subagent-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// 假 LLM：把最后一条 user 消息回显（带前缀），便于断言"哪个子任务跑了"。
fn start_fake_llm(port: u16) -> std::process::Child {
    let script = format!(
        r#"
import http.server, json
class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get('Content-Length', 0))
        body = json.loads(self.rfile.read(n) or b'{{}}')
        msgs = body.get('messages', [])
        last = msgs[-1].get('content','') if msgs else ''
        # 回显最后一条 user 内容的前 120 字
        resp = {{"choices":[{{"message":{{"role":"assistant","content":"SUBRESULT: "+last[:120]}},"finish_reason":"stop"}}]}}
        data = json.dumps(resp).encode()
        self.send_response(200); self.send_header('Content-Type','application/json')
        self.send_header('Content-Length', str(len(data))); self.end_headers(); self.wfile.write(data)
    def log_message(self, *a): pass
http.server.HTTPServer(('127.0.0.1', {port}), H).serve_forever()
"#,
        port = port
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

fn llm(port: u16, dir: &std::path::Path) -> ne_engine::llm::LlmParams {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("models.yml"),
        format!(
            "providers:\n  demo:\n    baseUrl: http://127.0.0.1:{port}/v1\n    api: openai-completions\n    task:\n      keyPool:\n        - \"sk-fake-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n    models:\n      - id: demo-chat-1\n"
        ),
    )
    .unwrap();
    std::fs::write(dir.join("config.yml"), "modelRoles:\n  default: demo/demo-chat-1\n").unwrap();
    let mut cfg = RuntimeConfig::default();
    cfg.tools.enabled = vec!["read".into()];
    std::env::set_var("TAIXU_LLM_DIR", dir);
    cfg.llm_params(dir).unwrap()
}

#[test]
fn run_subagents_sequentially_in_isolated_sessions() {
    let work = tmp("run");
    let port = 9201;
    let mut child = start_fake_llm(port);
    wait_port(port);

    let cfg = RuntimeConfig::default();
    let params = llm(port, &work.join("llm"));
    let store = SessionStore::open(work.join("agent.db")).unwrap();
    let parent_sid = store.create_session("父").unwrap();
    let enabled = ne_agent::resolve_enabled_tools(&cfg).unwrap();
    let parent = ne_agent::task::parent_ctx(
        &store, &parent_sid, &cfg, params, work.clone(), work.clone(), enabled,
    );
    // 【2026-10-18】注入测试传输层：子 Lane 主循环直连 127.0.0.1 假 LLM。
    // 生产 parent_ctx 恒为 None → 仍受三重锁。
    let mut parent = parent;
    parent.llm_transport = Some(std::sync::Arc::new(|p, m| ne_engine::llm::call(p, m)));

    // 两个子任务（只读，避免审批）
    let spec = json!({"subagents": [
        {"taskName": "甲", "role": "dev", "prompt": "第一个子任务"},
        {"taskName": "乙", "role": "dev", "prompt": "第二个子任务"}
    ]});
    let tasks = parse_subagents(&spec, DEFAULT_MAX_TASKS);
    assert_eq!(tasks.len(), 2);
    let outcomes = ne_agent::task::run_subagents(&parent, &tasks, DEFAULT_MAX_TASKS).unwrap();
    let _ = child.kill();

    assert_eq!(outcomes.len(), 2, "两个子任务都应执行");
    assert!(outcomes[0].ok, "甲：{}", outcomes[0].output);
    assert!(outcomes[1].ok, "乙：{}", outcomes[1].output);
    assert!(outcomes[0].output.contains("第一个子任务"), "{}", outcomes[0].output);
    assert!(outcomes[1].output.contains("第二个子任务"), "{}", outcomes[1].output);

    // 隔离：两个子会话不同，且都不是父会话
    assert_ne!(outcomes[0].session_id, outcomes[1].session_id);
    assert_ne!(outcomes[0].session_id, parent_sid);
    // 父会话不应被污染（子任务的消息在自己的会话里）
    let parent_hist = store.history(&parent_sid).unwrap();
    assert!(
        parent_hist.iter().all(|m| !m.content.contains("第一个子任务")),
        "父会话不应看到子任务内容"
    );
    // 子会话在库中确实存在（树结构）
    assert!(store.get_session(&outcomes[0].session_id).unwrap().is_some());
}

#[test]
fn wildcard_lease_rejected_before_execution() {
    // 写租约冲突应在"进入执行"前就被拒（不需要 LLM）
    let tasks = vec![
        SubagentTask { task_name: "A".into(), role: "r".into(), prompt: "p".into(),
            write_paths: vec!["*".into()], ..Default::default() },
        SubagentTask { task_name: "B".into(), role: "r".into(), prompt: "p".into(),
            write_paths: vec!["x.txt".into()], ..Default::default() },
    ];
    assert!(check_write_leases(&tasks).is_err());
}
