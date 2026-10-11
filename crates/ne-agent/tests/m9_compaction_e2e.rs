//! M9 端到端：`compress` / `history_read` / `history_search` 走**真实工具总线**，
//! 以及 CLI `context --json` / `compress --show --json` 的 JSON 形状（UI 消费契约）。
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
