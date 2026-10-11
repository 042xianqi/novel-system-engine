//! `session` 的单元测试（单独文件，避免主文件超架构门禁 700 行）。

use super::*;

    use super::*;

    fn store(tag: &str) -> (SessionStore, PathBuf) {
        let p = std::env::temp_dir().join(format!(
            "ne-agent-session-{tag}-{}-{}.db",
            std::process::id(),
            now_ms()
        ));
        let _ = std::fs::remove_file(&p);
        (SessionStore::open(&p).unwrap(), p)
    }

    #[test]
    fn create_and_append_and_history() {
        let (s, p) = store("hist");
        let sid = s.create_session("测试").unwrap();
        assert_eq!(s.head(&sid).unwrap(), None);
        let m1 = s.append(&sid, None, "user", "你好").unwrap();
        let m2 = s.append(&sid, Some(m1), "assistant", "你好，我是 ne-agent").unwrap();
        assert_eq!(s.head(&sid).unwrap(), Some(m2));
        let h = s.history(&sid).unwrap();
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].role, "user");
        assert_eq!(h[1].role, "assistant");
        assert_eq!(h[1].content, "你好，我是 ne-agent");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn latest_session_orders_by_update() {
        let (s, p) = store("latest");
        let a = s.create_session("A").unwrap();
        let b = s.create_session("B").unwrap();
        s.append(&a, None, "user", "touch a").unwrap();
        assert_eq!(s.latest_session().unwrap().unwrap().id, a);
        s.append(&b, None, "user", "touch b").unwrap();
        assert_eq!(s.latest_session().unwrap().unwrap().id, b);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn tree_branch_history_follows_head_path() {
        // 这是"树而非线性"的关键回归：从同一父分叉两条，history 只取 head 那条。
        let (s, p) = store("branch");
        let sid = s.create_session("分叉").unwrap();
        let root = s.append(&sid, None, "user", "起点").unwrap();
        let a1 = s.append(&sid, Some(root), "assistant", "分支A").unwrap();
        let b1 = s.append(&sid, Some(root), "assistant", "分支B").unwrap();

        // 两个子节点都挂在 root 下
        let kids = s.children(&sid, root).unwrap();
        assert_eq!(kids, vec![a1, b1]);

        // head = 最大 id = b1 → history 走 B 路径，不含 A
        let h = s.history(&sid).unwrap();
        let texts: Vec<&str> = h.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(texts, vec!["起点", "分支B"]);

        // 会话树：派生会话带 parent_session_id
        let child = s.derive_session(&sid, "子会话").unwrap();
        let row = s.get_session(&child).unwrap().unwrap();
        assert_eq!(row.parent_session_id.as_deref(), Some(sid.as_str()));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn persists_across_reopen() {
        let (s, p) = store("persist");
        let sid = s.create_session("持久").unwrap();
        s.append(&sid, None, "user", "记住我").unwrap();
        drop(s);
        let s2 = SessionStore::open(&p).unwrap();
        let h = s2.history(&sid).unwrap();
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].content, "记住我");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn rewind_to_keeps_messages_but_moves_head() {
        let (s, p) = store("rewind");
        let sid = s.create_session("回退").unwrap();
        let m1 = s.append(&sid, None, "user", "第一轮").unwrap();
        let m2 = s.append(&sid, Some(m1), "assistant", "回复一").unwrap();
        let _m3 = s.append(&sid, Some(m2), "user", "第二轮").unwrap();
        assert_eq!(s.history(&sid).unwrap().len(), 3);

        // 回退到 m1：其后消息仍在库中，但不再是当前分支
        s.rewind_to(&sid, m1).unwrap();
        let h = s.history(&sid).unwrap();
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].content, "第一轮");
        assert_eq!(s.message_count(&sid).unwrap(), 3, "数据不删");

        // 从新 head 再追加 → 分叉（老分支内容不丢）
        let m4 = s.append(&sid, Some(m1), "assistant", "新分支回复").unwrap();
        let h2 = s.history(&sid).unwrap();
        assert_eq!(h2.len(), 2);
        assert_eq!(h2[1].content, "新分支回复");
        assert_eq!(s.message_count(&sid).unwrap(), 4, "两条分支共存");
        let _ = m4;
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn rewind_back_n_steps() {
        let (s, p) = store("rewindback");
        let sid = s.create_session("N 步").unwrap();
        let m1 = s.append(&sid, None, "user", "a").unwrap();
        let m2 = s.append(&sid, Some(m1), "assistant", "b").unwrap();
        let m3 = s.append(&sid, Some(m2), "user", "c").unwrap();
        // 回退 1 步 → head = m2
        assert_eq!(s.rewind_back(&sid, 1).unwrap(), Some(m2));
        assert_eq!(s.history(&sid).unwrap().len(), 2);
        // 回退 10 步（超过长度）→ 到最早一条
        assert_eq!(s.rewind_back(&sid, 10).unwrap(), Some(m1));
        assert_eq!(s.history(&sid).unwrap().len(), 1);
        let _ = m3;
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn rewind_last_turn_drops_current_user() {
        let (s, p) = store("rewindturn");
        let sid = s.create_session("重来").unwrap();
        let m1 = s.append(&sid, None, "user", "第一问").unwrap();
        let m2 = s.append(&sid, Some(m1), "assistant", "答一").unwrap();
        let _m3 = s.append(&sid, Some(m2), "user", "第二问").unwrap();
        // 回退最后一条 user → head = m2（答一）
        assert_eq!(s.rewind_last_turn(&sid).unwrap(), Some(m2));
        assert_eq!(s.history(&sid).unwrap().last().unwrap().content, "答一");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn rewind_last_turn_on_first_user_empties_branch() {
        let (s, p) = store("rewindfirst");
        let sid = s.create_session("空").unwrap();
        let m1 = s.append(&sid, None, "user", "唯一问").unwrap();
        // 首条就是 user → 分支清空（哨兵 -1），但消息不删
        assert_eq!(s.rewind_last_turn(&sid).unwrap(), None);
        assert_eq!(s.head(&sid).unwrap(), None);
        assert!(s.history(&sid).unwrap().is_empty());
        assert_eq!(s.message_count(&sid).unwrap(), 1);
        let _ = m1;
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn rewind_rejects_foreign_message() {
        let (s, p) = store("rewindbad");
        let a = s.create_session("A").unwrap();
        let b = s.create_session("B").unwrap();
        let ma = s.append(&a, None, "user", "属于 A").unwrap();
        // 用 A 的消息去 rewind B → 必须报错（防跨会话串改）
        assert!(s.rewind_to(&b, ma).is_err());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn head_id_survives_reopen_after_rewind() {
        let (s, p) = store("rewindpersist");
        let sid = s.create_session("跨界回退").unwrap();
        let m1 = s.append(&sid, None, "user", "一").unwrap();
        let m2 = s.append(&sid, Some(m1), "assistant", "二").unwrap();
        s.append(&sid, Some(m2), "user", "三").unwrap();
        s.rewind_to(&sid, m1).unwrap();
        drop(s);
        // 重开后 head 仍是 m1（而非 MAX(id)）
        let s2 = SessionStore::open(&p).unwrap();
        assert_eq!(s2.head(&sid).unwrap(), Some(m1));
        assert_eq!(s2.history(&sid).unwrap().len(), 1);
        let _ = std::fs::remove_file(&p);
    }
