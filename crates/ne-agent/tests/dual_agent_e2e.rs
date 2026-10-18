//! 双智能体（顺序版 Planner→Executor）端到端 —— 用脚本化假 LLM。
//!
//! 假 LLM 行为：
//!   * 看到"执行汇报"类反馈 → Planner 返回 FINISH；
//!   * 否则 → Planner 返回 INIT_PLAN（一个步骤）；
//!   * Executor 的会话里最后一条是单步指令（含"请严格执行"）→ 回固定成果文本（无工具调用 → 正常收尾）。
//!
//! 断言：整个流程跑通、产出 Success、计划收敛、Execut 会话确实被创建。

use ne_agent::config::RuntimeConfig;
use ne_agent::dual_agent::{self, DualAgentOutcome, DualAgentParams, StepStatus};
use ne_agent::session::SessionStore;
use ne_agent::tool::ToolCtx;
use std::path::PathBuf;

fn tmp(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("ne-agent-dual-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// 脚本化假 LLM：根据最后一条 user 内容决定 Planner 的 action。
fn start_scripted_llm(port: u16) -> std::process::Child {
    let script = format!(
        r#"
import http.server, json
PLAN = {{"thought":"拆成一步","action":"INIT_PLAN","plan":[{{"id":"step_1","title":"唯一工序","instruction":"完成目标","expectedOutcome":"目标达成","dependencies":[]}}]}}
FINISH = {{"thought":"已完成","action":"FINISH","finalReport":"双智能体交付完成","plan":[]}}
class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get('Content-Length', 0))
        body = json.loads(self.rfile.read(n) or b'{{}}')
        msgs = body.get('messages', [])
        last = msgs[-1].get('content','') if msgs else ''
        if '执行汇报' in last or '全部完成' in last:
            content = '```json\n' + json.dumps(FINISH, ensure_ascii=False) + '\n```'
        elif '请严格执行' in last or '单步工序' in last:
            content = 'EXECUTOR_DONE: 已按要求完成本步'
        else:
            content = '```json\n' + json.dumps(PLAN, ensure_ascii=False) + '\n```'
        resp = {{"choices":[{{"message":{{"role":"assistant","content":content}},"finish_reason":"stop"}}]}}
        data = json.dumps(resp, ensure_ascii=False).encode()
        self.send_response(200); self.send_header('Content-Type','application/json')
        self.send_header('Content-Length', str(len(data))); self.end_headers(); self.wfile.write(data)
    def log_message(self, *a): pass
http.server.HTTPServer(('127.0.0.1', {port}), H).serve_forever()
"#,
        port = port
    );
    std::process::Command::new("python3")
        .arg("-c").arg(script)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("启动脚本化假 LLM 失败")
}

fn wait_port(port: u16) {
    for _ in 0..50 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(60));
    }
}

fn llm_for(port: u16, dir: &std::path::Path) -> ne_engine::llm::LlmParams {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("models.yml"),
        format!(
            "providers:\n  demo:\n    baseUrl: http://127.0.0.1:{port}/v1\n    api: openai-completions\n    task:\n      keyPool:\n        - \"[REDACTED]\"\n    models:\n      - id: demo-chat-1\n"
        ),
    )
    .unwrap();
    std::fs::write(dir.join("config.yml"), "modelRoles:\n  default: demo/demo-chat-1\n").unwrap();
    std::env::set_var("TAIXU_LLM_DIR", dir);
    RuntimeConfig::default().llm_params(dir).unwrap()
}

#[test]
fn dual_agent_sequential_success() {
    let work = tmp("run");
    let port = 9401;
    let mut child = start_scripted_llm(port);
    wait_port(port);

    let cfg = std::sync::Arc::new(RuntimeConfig::default());
    let llm = llm_for(port, &work.join("llm"));
    let db = work.join("agent.db");
    let store = SessionStore::open(&db).unwrap();
    let sid = store.create_session("父").unwrap();
    drop(store);

    let ctx = ToolCtx::new(&work)
        .with_vault_root(&work)
        .with_runtime(db.clone(), cfg, work.clone(), vec![ne_agent::tool::Tool::Read])
        .with_llm(llm)
        .with_session(&sid, ne_agent::context_tools::ContextStore::open(&db).unwrap());

    let params = DualAgentParams::from_ctx(&ctx, "把目标做完并验收", 6).unwrap();
    // 【2026-10-18】注入测试传输层：直连 127.0.0.1 假 LLM，不经三重锁。
    // 生产装配里该字段恒为 None（见 DualAgentParams::from_ctx）。
    let mut params = params;
    params.llm_transport = Some(std::sync::Arc::new(|p, msgs| ne_engine::llm::call(p, msgs)));
    let outcome = dual_agent::run(&params).unwrap();
    let _ = child.kill();

    match outcome {
        DualAgentOutcome::Success { final_report, plan, total_rounds, .. } => {
            assert!(final_report.contains("交付完成"), "final_report={final_report}");
            assert_eq!(plan.len(), 1);
            assert_eq!(plan[0].status, StepStatus::Completed, "唯一步骤应完成");
            assert!(total_rounds >= 2, "至少 INIT_PLAN + FINISH 两轮，实得 {total_rounds}");
        }
        other => panic!("应成功：{other:?}"),
    }
    // Executor 会话确实建在库里（lane id 会去掉 `:` 等字符）
    let store = SessionStore::open(&db).unwrap();
    let all = store.list_sessions().unwrap();
    assert!(
        all.iter().any(|s| s.id.contains("lane0") && s.id.contains("executor")),
        "应存在 Executor 子会话；实得：{:?}",
        all.iter().map(|s| s.id.clone()).collect::<Vec<_>>()
    );
    assert!(all.iter().any(|s| s.id.contains("dual:planner")), "应存在 Planner 子会话");
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn dual_agent_from_ctx_requires_runtime() {
    let work = tmp("nort");
    let ctx = ToolCtx::new(&work);
    assert!(DualAgentParams::from_ctx(&ctx, "x", 5).is_err(), "缺运行时上下文应报错");
    let _ = std::fs::remove_dir_all(&work);
}
