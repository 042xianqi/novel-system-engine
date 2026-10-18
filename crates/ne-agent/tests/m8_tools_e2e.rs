//! M8 新工具端到端 —— 走**真实工具总线**（`tool::execute`），非纯单测。
//!
//! 覆盖：ask_user（有/无回调）、load_rule（内置/override/未知）、load_skill（正文/子资源/越界）。
//! 参考 tests/approval_e2e.rs 的装配方式。

use ne_agent::tool::{Tool, ToolArgs, ToolCtx};

fn tmp(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("ne-agent-m8-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn args(pairs: &[(&str, &str)]) -> ToolArgs {
    let mut a = ToolArgs::new();
    for (k, v) in pairs {
        a = a.set(k, *v);
    }
    a
}

#[test]
fn ask_user_without_asker_reports_non_interactive() {
    let work = tmp("ask-nocb");
    let ctx = ToolCtx::new(&work).with_vault_root(&work);
    let a = args(&[("questions", r#"[{"question":"继续吗？"}]"#)]);
    let r = ne_agent::tool::execute(&ctx, Tool::AskUser, &a).unwrap();
    assert!(!r.ok, "无回调应失败");
    assert!(r.output.contains("非交互"), "错误应说明非交互：{}", r.output);
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn ask_user_with_asker_returns_answer() {
    let work = tmp("ask-cb");
    fn fake_asker(_q: &str) -> Result<String, String> {
        Ok("用户选了 A".into())
    }
    let ctx = ToolCtx::new(&work).with_vault_root(&work).with_asker(fake_asker);
    let a = args(&[("questions", r#"[{"question":"选哪个","options":[{"label":"A"},{"label":"B"}]}]"#)]);
    let r = ne_agent::tool::execute(&ctx, Tool::AskUser, &a).unwrap();
    assert!(r.ok, "有回调应成功：{}", r.output);
    assert!(r.output.contains("用户选了 A"));
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn ask_user_rejects_empty_answer() {
    let work = tmp("ask-empty");
    fn empty_asker(_q: &str) -> Result<String, String> {
        Ok("   ".into())
    }
    let ctx = ToolCtx::new(&work).with_vault_root(&work).with_asker(empty_asker);
    let a = args(&[("questions", r#"[{"question":"x"}]"#)]);
    let r = ne_agent::tool::execute(&ctx, Tool::AskUser, &a).unwrap();
    assert!(!r.ok);
    assert!(r.output.contains("未提供回答"), "{}", r.output);
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn load_rule_builtin_and_unknown() {
    let work = tmp("rule");
    let ctx = ToolCtx::new(&work).with_vault_root(&work);
    let good = ne_agent::tool::execute(&ctx, Tool::LoadRule, &args(&[("rule", "workflow")])).unwrap();
    assert!(good.ok, "内置 workflow 应可加载：{}", good.output);
    assert!(good.output.contains("规则块：workflow"));

    let bad = ne_agent::tool::execute(&ctx, Tool::LoadRule, &args(&[("rule", "no-such")])).unwrap();
    assert!(!bad.ok);
    assert!(bad.output.contains("未知规则块"), "{}", bad.output);
    assert!(bad.output.contains("workflow"), "错误应列可用清单：{}", bad.output);
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn load_skill_body_and_resource() {
    let work = tmp("skill");
    let dir = work.join("skills").join("demo").join("references");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        work.join("skills/demo/SKILL.md"),
        "---\ndescription: 演示技能\n---\n技能正文内容",
    )
    .unwrap();
    std::fs::write(dir.join("spec.md"), "子资源规格").unwrap();

    let ctx = ToolCtx::new(&work).with_vault_root(&work);
    let body = ne_agent::tool::execute(&ctx, Tool::LoadSkill, &args(&[("name", "demo")])).unwrap();
    assert!(body.ok, "{}", body.output);
    assert!(body.output.contains("技能正文内容"));
    assert!(!body.output.contains("description"), "frontmatter 应被剥离");

    let res = ne_agent::tool::execute(
        &ctx,
        Tool::LoadSkill,
        &args(&[("name", "demo"), ("path", "references/spec.md")]),
    )
    .unwrap();
    assert!(res.ok, "{}", res.output);
    assert!(res.output.contains("子资源规格"));

    let esc = ne_agent::tool::execute(
        &ctx,
        Tool::LoadSkill,
        &args(&[("name", "demo"), ("path", "../secret")]),
    )
    .unwrap();
    assert!(!esc.ok, "越界路径应被拒");
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn subagents_without_runtime_reports_missing() {
    let work = tmp("sub-nort");
    let ctx = ToolCtx::new(&work).with_vault_root(&work);
    let a = args(&[("subagents", r#"[{"taskName":"t","role":"engineering","prompt":"做事"}]"#)]);
    let r = ne_agent::tool::execute(&ctx, Tool::Subagents, &a).unwrap();
    assert!(!r.ok);
    assert!(r.output.contains("上下文"), "应提示缺运行时上下文：{}", r.output);
    let _ = std::fs::remove_dir_all(&work);
}

// ── 子智能体走真实工具总线（含运行时装配 + 假 LLM）──

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
        .arg("-c").arg(script)
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
    let cfg = ne_agent::config::RuntimeConfig::default();
    cfg.llm_params(dir).unwrap()
}

#[test]
fn subagents_via_tool_bus_runs_in_isolated_lanes() {
    let work = tmp("sub-run");
    let port = 9301;
    let mut child = start_fake_llm(port);
    wait_port(port);

    let cfg = std::sync::Arc::new(ne_agent::config::RuntimeConfig::default());
    let llm = llm_for(port, &work.join("llm"));
    let db = work.join("agent.db");
    let store = ne_agent::session::SessionStore::open(&db).unwrap();
    let sid = store.create_session("父").unwrap();
    drop(store);

    let ctx = ToolCtx::new(&work)
        .with_vault_root(&work)
        .with_runtime(db.clone(), cfg, work.clone(), vec![Tool::Read])
        .with_llm(llm)
        // 【2026-10-18】测试注入：子智能体主循环直连 127.0.0.1 假 LLM。
        // 生产 ToolCtx 从不调用 with_llm_transport → 仍受三重锁。
        .with_llm_transport(std::sync::Arc::new(|p, m| ne_engine::llm::call(p, m)))
        .with_session(&sid, ne_agent::context_tools::ContextStore::open(&db).unwrap());

    let a = args(&[(
        "subagents",
        r#"[{"taskName":"甲","role":"dev","prompt":"子任务甲"},{"taskName":"乙","role":"dev","prompt":"子任务乙"}]"#,
    )]);
    let r = ne_agent::tool::execute(&ctx, Tool::Subagents, &a).unwrap();
    let _ = child.kill();
    assert!(r.ok, "子智能体应成功：{}", r.output);
    assert!(r.output.contains("子任务甲"), "{}", r.output);
    assert!(r.output.contains("子任务乙"), "{}", r.output);

    // 隔离：两个子会话确实建在库里
    let store = ne_agent::session::SessionStore::open(&db).unwrap();
    let all = store.list_sessions().unwrap_or_default();
    let lane_count = all
        .iter()
        .filter(|s| s.parent_session_id.as_deref() == Some(sid.as_str()))
        .count();
    assert!(lane_count >= 2, "应有 ≥2 个挂父会话下的子会话，实得 {lane_count}");
    let _ = std::fs::remove_dir_all(&work);
}

