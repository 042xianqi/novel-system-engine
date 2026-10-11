//! `compaction` 的单元测试（M9）。

use super::*;
use crate::session::{MsgRow, SessionStore};

fn store(tag: &str) -> (SessionStore, std::path::PathBuf) {
    let p = std::env::temp_dir().join(format!(
        "ne-agent-comp-{tag}-{}-{}.db",
        std::process::id(),
        crate::session::now_ms()
    ));
    let _ = std::fs::remove_file(&p);
    (SessionStore::open(&p).unwrap(), p)
}

/// 造一个 3 轮 user 的会话（每条 user + 一条 assistant）。
fn seed(s: &SessionStore) -> String {
    let sid = s.create_session("压缩测试").unwrap();
    let mut parent = None;
    for i in 1..=3 {
        parent = Some(s.append(&sid, parent, "user", &format!("第{i}轮用户消息内容")).unwrap());
        parent = Some(s.append(&sid, parent, "assistant", &format!("第{i}轮助手回复")).unwrap());
    }
    sid
}

#[test]
fn mode_parse() {
    assert_eq!(CompressMode::parse("before"), Some(CompressMode::Before));
    assert_eq!(CompressMode::parse(" AFter "), Some(CompressMode::After));
    assert_eq!(CompressMode::parse("nope"), None);
    assert_eq!(CompressMode::After.as_str(), "after");
}

#[test]
fn before_requires_unique_anchor() {
    let (s, p) = store("anchor");
    let sid = seed(&s);
    let hist = s.history(&sid).unwrap();

    // 太短 → 拒
    assert!(matches!(
        resolve_compress_anchor(&hist, CompressMode::Before, "短"),
        ScrollOutcome::Invalid { .. }
    ));
    // 没匹配 → 拒
    assert!(matches!(
        resolve_compress_anchor(&hist, CompressMode::Before, "不存在的锚点文字"),
        ScrollOutcome::Invalid { .. }
    ));
    // 唯一匹配 → 保留该轮
    match resolve_compress_anchor(&hist, CompressMode::Before, "第2轮用户消息内容") {
        ScrollOutcome::Resolved { keep_from_msg_id } => {
            let kept = hist.iter().find(|m| m.id == keep_from_msg_id).unwrap();
            assert_eq!(kept.content, "第2轮用户消息内容");
        }
        other => panic!("应解析成功：{other:?}"),
    }
    // 首轮作锚点 → 前面没有可压的 → 拒
    assert!(matches!(
        resolve_compress_anchor(&hist, CompressMode::Before, "第1轮用户消息内容"),
        ScrollOutcome::Invalid { .. }
    ));
    let _ = std::fs::remove_file(&p);
}

#[test]
fn after_keeps_current_turn() {
    let (s, p) = store("after");
    let sid = seed(&s);
    let hist = s.history(&sid).unwrap();
    match resolve_compress_anchor(&hist, CompressMode::After, "") {
        ScrollOutcome::Resolved { keep_from_msg_id } => {
            // 边界 = 最后一条 user（第3轮）：它及之后保留
            let kept = hist.iter().find(|m| m.id == keep_from_msg_id).unwrap();
            assert_eq!(kept.content, "第3轮用户消息内容");
            assert_eq!(kept.role, "user");
        }
        other => panic!("应解析成功：{other:?}"),
    }
    let _ = std::fs::remove_file(&p);
}

#[test]
fn compact_folds_but_keeps_originals() {
    let (s, p) = store("compact");
    let sid = seed(&s);
    let total = s.message_count(&sid).unwrap();

    let out = compact(&s, &sid, CompressMode::After, "", None).unwrap();
    assert!(out.folded_message_count >= 1);
    assert!(!out.llm_summarized, "无摘要器 → 机械降级");

    // 原文不删
    assert_eq!(s.message_count(&sid).unwrap(), total, "压缩不得删消息");

    // 投影：折叠 + 保留 = 原总数
    let proj = project(&s, &sid).unwrap();
    assert_eq!(proj.folded_count + proj.retained.len(), total as usize);
    assert!(proj.summary.is_some());
    assert!(proj.est_tokens <= proj.est_tokens_full, "压缩后不应更大");
    let _ = std::fs::remove_file(&p);
}

#[test]
fn projection_summary_is_user_role_and_prefixed() {
    let (s, p) = store("proj");
    let sid = seed(&s);
    compact(&s, &sid, CompressMode::After, "", None).unwrap();
    let proj = project(&s, &sid).unwrap();
    let msgs = proj.to_messages();
    // 首条应是摘要（user 角色，带固定抬头）
    assert_eq!(msgs[0].role, "user");
    assert!(msgs[0].content.starts_with(SUMMARY_HEADER));
    let _ = std::fs::remove_file(&p);
}

#[test]
fn clear_compaction_restores_full_context() {
    let (s, p) = store("clear");
    let sid = seed(&s);
    let full = s.history(&sid).unwrap().len();
    compact(&s, &sid, CompressMode::After, "", None).unwrap();
    assert!(project(&s, &sid).unwrap().summary.is_some());
    let n = s.clear_compactions(&sid).unwrap();
    assert_eq!(n, 1);
    let proj = project(&s, &sid).unwrap();
    assert!(proj.summary.is_none());
    assert_eq!(proj.retained.len(), full, "清除压缩记录后回到全量");
    let _ = std::fs::remove_file(&p);
}

#[test]
fn repeated_compaction_uses_latest() {
    let (s, p) = store("repeat");
    let sid = seed(&s);
    // 第一次压第1轮之前（after: 边界=最后user=第3轮）
    compact(&s, &sid, CompressMode::After, "", None).unwrap();
    // 再追加一轮，再压
    let head = s.head(&sid).unwrap();
    let m = s.append(&sid, head, "user", "第4轮用户消息内容").unwrap();
    s.append(&sid, Some(m), "assistant", "第4轮助手回复").unwrap();
    let out2 = compact(&s, &sid, CompressMode::After, "", None).unwrap();
    assert!(out2.folded_message_count >= 1);
    // 投影用最新一条
    let proj = project(&s, &sid).unwrap();
    assert!(proj.summary.is_some());
    // 压缩记录有两
    assert_eq!(s.list_compactions(&sid).unwrap().len(), 2);
    let _ = std::fs::remove_file(&p);
}

#[test]
fn history_read_and_search_see_folded() {
    let (s, p) = store("readback");
    let sid = seed(&s);
    compact(&s, &sid, CompressMode::After, "", None).unwrap();
    // 被折叠的第一轮原文仍可按内容搜到
    let hits = s.search_messages(&sid, "第1轮用户", 10).unwrap();
    assert!(!hits.is_empty(), "被折叠原文应可搜到");
    let id = hits[0].0;
    let m = s.message_by_id(id).unwrap().unwrap();
    assert_eq!(m.content, "第1轮用户消息内容");
    let _ = std::fs::remove_file(&p);
}

#[test]
fn mechanical_summary_preserves_user_turns() {
    let folded = vec![
        MsgRow { id: 1, parent_id: None, role: "user".into(), content: "目标：写第一章".into(), created_at: 0 },
        MsgRow { id: 2, parent_id: Some(1), role: "assistant".into(), content: "好的".into(), created_at: 0 },
        MsgRow { id: 3, parent_id: Some(2), role: "user".into(), content: "改用第三人称".into(), created_at: 0 },
    ];
    let s = MechanicalSummarizer.summarize(&SummarizeInput { folded: &folded }).unwrap();
    assert!(s.contains("写第一章"), "机械摘要须保住用户要点：{s}");
    assert!(s.contains("第三人称"));
    assert!(s.contains("2 个用户轮"));
}
