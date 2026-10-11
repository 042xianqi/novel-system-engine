//! M9 端到端：`compress` / `history_read` / `history_search` 走**真实工具总线**，
//! 以及 CLI `context --json` / `compress --show --json` 的 JSON 形状（UI 消费契约，
//! 见文件末尾 spawn 真实二进制的那组测试）。
//!
//! 不依赖真实模型：摘要走机械降级路径（确定性）。

use ne_agent::tool::{Tool, ToolArgs, ToolCtx};

fn tmp(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("ne-agent-m9-{tag}-{}", std::process::id()));
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

/// 造库 + 3 轮会话，返回 (work, db, sid)。
fn seed(tag: &str) -> (std::path::PathBuf, std::path::PathBuf, String) {
    let work = tmp(tag);
    let db = work.join("agent.db");
    let store = ne_agent::session::SessionStore::open(&db).unwrap();
    let sid = store.create_session("M9 测试").unwrap();
    let mut parent = None;
    for i in 1..=3 {
        parent = Some(store.append(&sid, parent, "user", &format!("第{i}轮用户消息内容")).unwrap());
        parent = Some(store.append(&sid, parent, "assistant", &format!("第{i}轮助手回复")).unwrap());
    }
    (work, db, sid)
}

fn ctx_for(work: &std::path::Path, db: &std::path::Path, sid: &str) -> ToolCtx {
    let cfg = std::sync::Arc::new(ne_agent::config::RuntimeConfig::default());
    ToolCtx::new(work)
        .with_vault_root(work)
        .with_runtime(
            db.to_path_buf(),
            cfg,
            work.to_path_buf(),
            vec![Tool::Compress, Tool::HistoryRead, Tool::HistorySearch],
        )
        .with_session(sid, ne_agent::context_tools::ContextStore::open(db).unwrap())
}

#[test]
fn compress_tool_after_folds_and_keeps_originals() {
    let (work, db, sid) = seed("compress-after");
    let ctx = ctx_for(&work, &db, &sid);

    let before = ne_agent::session::SessionStore::open(&db).unwrap().message_count(&sid).unwrap();
    // 无 llm → 机械降级
    let r = ne_agent::tool::execute(&ctx, Tool::Compress, &args(&[("mode", "after")])).unwrap();
    assert!(r.ok, "compress 应成功：{}", r.output);
    assert!(r.output.contains("机械降级"), "无模型应机械降级：{}", r.output);

    // 原文不删
    let after = ne_agent::session::SessionStore::open(&db).unwrap().message_count(&sid).unwrap();
    assert_eq!(before, after, "压缩不得删消息");
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn compress_tool_before_requires_anchor() {
    let (work, db, sid) = seed("compress-before");
    let ctx = ctx_for(&work, &db, &sid);
    // 缺 anchor → 拒
    let r = ne_agent::tool::execute(&ctx, Tool::Compress, &args(&[("mode", "before")])).unwrap();
    assert!(!r.ok, "before 无 anchor 应拒");
    // 有唯一锚点 → 成功
    let r2 = ne_agent::tool::execute(
        &ctx,
        Tool::Compress,
        &args(&[("mode", "before"), ("anchor", "第2轮用户消息内容")]),
    )
    .unwrap();
    assert!(r2.ok, "{}", r2.output);
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn compress_tool_invalid_mode_rejected() {
    let (work, db, sid) = seed("compress-badmode");
    let ctx = ctx_for(&work, &db, &sid);
    let r = ne_agent::tool::execute(&ctx, Tool::Compress, &args(&[("mode", "sideways")])).unwrap();
    assert!(!r.ok);
    assert!(r.output.contains("before"), "应提示合法 mode：{}", r.output);
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn history_read_and_search_see_folded_content() {
    let (work, db, sid) = seed("hist");
    let ctx = ctx_for(&work, &db, &sid);
    // 先压缩（折叠第1轮）
    let c = ne_agent::tool::execute(&ctx, Tool::Compress, &args(&[("mode", "after")])).unwrap();
    assert!(c.ok, "{}", c.output);

    // 搜索仍能找到被折叠的第1轮
    let s = ne_agent::tool::execute(
        &ctx,
        Tool::HistorySearch,
        &args(&[("query", "第1轮用户")]),
    )
    .unwrap();
    assert!(s.ok, "{}", s.output);
    assert!(s.output.contains("#"), "应给出带 id 的命中：{}", s.output);

    // 取第一条命中的 id，回读原文
    let id_line = s.output.lines().find(|l| l.contains('#')).unwrap();
    let id: String = id_line
        .split('#')
        .nth(1)
        .unwrap()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    let r = ne_agent::tool::execute(&ctx, Tool::HistoryRead, &args(&[("id", &id)])).unwrap();
    assert!(r.ok, "{}", r.output);
    assert!(r.output.contains("第1轮用户消息内容"), "{}", r.output);
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn history_read_missing_id_reports() {
    let (work, db, sid) = seed("hist-miss");
    let ctx = ctx_for(&work, &db, &sid);
    let r = ne_agent::tool::execute(&ctx, Tool::HistoryRead, &args(&[("id", "999999")])).unwrap();
    assert!(!r.ok);
    assert!(r.output.contains("不存在"), "{}", r.output);
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn these_tools_are_approval_exempt_and_readonly_tier() {
    use ne_agent::tool::Tier;
    for t in [Tool::Compress, Tool::HistoryRead, Tool::HistorySearch] {
        assert_eq!(t.tier(), Tier::Read, "{} 无副作用，应为 Read 档", t.name());
        assert!(t.is_session_local(), "{} 应为会话局部", t.name());
    }
}

// ---------------------------------------------------------------------------
// CLI 契约（真·二进制级）：注释里承诺的 `context --json` / `compress --show --json`
// 形状必须真的可被 UI 消费——这里 spawn 真实二进制断言，而不是只靠人工跑一遍。
// ---------------------------------------------------------------------------

use std::process::Command;

/// 在临时 vault 目录造一个含 3 轮会话的库（不经模型，直接写库）。
fn seed_vault(tag: &str) -> std::path::PathBuf {
    let root = tmp(tag);
    let v = ne_agent::Vault::ensure(&root).unwrap();
    let store = ne_agent::session::SessionStore::open(v.agent_db()).unwrap();
    let sid = store.create_session("CLI 契约").unwrap();
    let mut parent = None;
    for i in 1..=3 {
        parent = Some(store.append(&sid, parent, "user", &format!("第{i}轮用户消息内容")).unwrap());
        parent = Some(store.append(&sid, parent, "assistant", &format!("第{i}轮助手回复")).unwrap());
    }
    root
}

/// 跑 `ne-agent --vault <v> [--json] <args...>`，返回解析后的 JSON。
fn run_json(vault: &std::path::Path, json: bool, args: &[&str]) -> serde_json::Value {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ne-agent"));
    cmd.arg("--vault").arg(vault);
    if json {
        cmd.arg("--json");
    }
    cmd.args(args);
    let out = cmd.output().expect("应能启动 ne-agent");
    assert!(
        out.status.success(),
        "命令 {:?} 失败：stdout={} stderr={}",
        args,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "命令 {:?} 的 stdout 不是 JSON（{e}）：{}",
            args,
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

#[test]
fn cli_context_json_shape() {
    let root = seed_vault("cli-context");
    let v = run_json(&root, true, &["context"]);
    let ctx = &v["context"];
    // UI 消费契约：这些字段名是稳定接口，改动即破坏 UI。
    for k in [
        "maxPromptTokens",
        "reserveTokens",
        "autoCompaction",
        "autoThresholdPercent",
        "keepRecentToolResults",
        "foldingLine",
    ] {
        assert!(ctx.get(k).is_some(), "context JSON 缺字段 {k}：{v}");
    }
    // foldingLine = window - reserve - tool_schema_reserve
    let w = ctx["maxPromptTokens"].as_u64().unwrap();
    let r = ctx["reserveTokens"].as_u64().unwrap();
    let f = ctx["foldingLine"].as_u64().unwrap();
    assert!(w > r && f < w, "折叠线应小于窗口上限：{v}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn cli_context_set_preserves_comments_and_applies() {
    let root = seed_vault("cli-context-set");
    // 默认 runtime.yaml 的 context 段含注释；设置后必须仍在。
    let before = std::fs::read_to_string(root.join("runtime.yaml")).unwrap();
    assert!(before.contains("# "), "前提：默认 runtime.yaml 含注释");

    let out = Command::new(env!("CARGO_BIN_EXE_ne-agent"))
        .arg("--vault")
        .arg(&root)
        .arg("context")
        .args(["--window", "64000", "--auto-compact", "--auto-threshold", "85"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let after = std::fs::read_to_string(root.join("runtime.yaml")).unwrap();
    assert!(after.contains("max_prompt_tokens: 64000"), "窗口未写入：\n{after}");
    assert!(after.contains("auto_compaction: true"), "开关未写入：\n{after}");
    assert!(after.contains("auto_threshold_percent: 85"), "阈值未写入：\n{after}");
    // 关键承诺：写回**不抹注释**（用整体序列化就会丢）。
    assert!(after.contains("# "), "写回后注释被抹掉了：\n{after}");

    // 再读一次，确认生效
    let v = run_json(&root, true, &["context"]);
    assert_eq!(v["context"]["maxPromptTokens"].as_u64().unwrap(), 64000);
    assert_eq!(v["context"]["autoCompaction"].as_bool().unwrap(), true);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn cli_compress_show_json_shape_and_history_read_back() {
    let root = seed_vault("cli-compress");
    // 未压缩时的投影形状
    let v = run_json(&root, true, &["compress", "--show"]);
    assert_eq!(v["foldedCount"].as_u64().unwrap(), 0);
    assert_eq!(v["retainedCount"].as_u64().unwrap(), 6);
    assert!(v["estTokens"].as_u64().is_some(), "应有 estTokens：{v}");
    assert!(v["estTokensFull"].as_u64().is_some(), "应有 estTokensFull：{v}");
    assert!(v["retained"].is_array(), "retained 应为数组：{v}");

    // 真压缩（机械降级，确定性）
    let out = Command::new(env!("CARGO_BIN_EXE_ne-agent"))
        .arg("--vault")
        .arg(&root)
        .args(["compress", "--mode", "after", "--mechanical"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let v2 = run_json(&root, true, &["compress", "--show"]);
    assert!(v2["foldedCount"].as_u64().unwrap() > 0, "压缩后应有折叠：{v2}");
    assert!(
        v2["estTokens"].as_u64().unwrap() < v2["estTokensFull"].as_u64().unwrap(),
        "压缩后投影 token 应小于全量：{v2}"
    );

    // 被折叠的原文仍可按 id 回读
    let msg = run_json(&root, true, &["history", "read", "1"]);
    assert_eq!(msg["role"].as_str().unwrap(), "user");
    assert!(msg["content"].as_str().unwrap().contains("第1轮"), "{msg}");
    let _ = std::fs::remove_dir_all(&root);
}
