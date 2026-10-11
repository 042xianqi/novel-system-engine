//! 回退模型 + conversation-only 控制器（S3-b 弱交付）。
//!
//! 对照太墟 `checkpoint/CheckpointModels.kt` 与 `RewindController.kt`：
//!   * 模型（[`RewindScope`]/[`RewindPlan`]/[`RewindResult`]/[`RewindUndoRecord`]）照搬；
//!   * 控制器**只做对话回退**——委托 [`crate::session::SessionStore::rewind_to`]，
//!     **文件恢复（CODE 范围）本里程碑砍掉**（开工令 §5：Rust 无 `WorkspaceFileAccess`
//!     抽象，硬做即空壳），CODE 范围返回 `partial = true` + note。
//!
//! # 为什么不从零造 controller
//!
//! 太墟 `RewindController` 依赖 `WorkspaceFileAccess`（write/preview/fileSizeOrNull/delete）
//! 与 `ConversationRewinder`（SessionTreeStore/HarnessRuntimeRepository），Rust 侧均无对应；
//! 而对话侧能力 `session.rs` 已有（`rewind_to`）。故本文件是**很薄的一层**：
//! 模型 + 把模型映射到既有 `rewind_to`，不重复实现。
use anyhow::Result;

use crate::checkpoint::FileSnap;
use crate::session::SessionStore;

/// 恢复范围：仅代码 / 仅对话 / 两者（太墟 `RewindScope`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewindScope {
    Code,
    Conversation,
    Both,
}

/// prepare/commit 两段式中的不可变方案（太墟 `RewindPlan`）。
#[derive(Debug, Clone, PartialEq)]
pub struct RewindPlan {
    pub session_id: String,
    /// 目标轮次。
    pub turn: i64,
    pub scope: RewindScope,
    /// 待恢复的文件快照（按 path 去重，取每 path 从目标轮起最早的轮初内容）。
    /// 由 [`crate::checkpoint::CheckpointStore::plan_code_rewind`] 填充。
    pub file_snaps: Vec<FileSnap>,
}

impl RewindPlan {
    /// 新建方案（file_snaps 留空，由 checkpoint store 规划填充）。
    pub fn new(session_id: impl Into<String>, turn: i64, scope: RewindScope) -> Self {
        RewindPlan {
            session_id: session_id.into(),
            turn,
            scope,
            file_snaps: Vec::new(),
        }
    }
}

/// commit 的结果（太墟 `RewindResult`）。
#[derive(Debug, Clone, PartialEq)]
pub struct RewindResult {
    pub files_restored: usize,
    pub files_deleted: usize,
    /// 存在无法满足的部分时为 true（如对话 fork 未就绪 / CODE 范围未实现）。
    pub partial: bool,
    pub note: Option<String>,
    /// CONVERSATION/BOTH 恢复派生出的新会话 id（Rust 侧暂无 fork，恒 None）。
    pub forked_session_id: Option<String>,
    /// 因外部改动被跳过恢复的路径（Rust 侧暂无文件层，恒空）。
    pub conflicts: Vec<String>,
}

/// 一次 rewind 的撤销记录（太墟 `RewindUndoRecord`）。
/// Rust 侧文件层未落地，保留结构以便后续里程碑接线。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RewindUndoRecord {
    /// rewind 实际写入/删除的状态。
    pub applied: Vec<FileSnap>,
    /// 与 `applied` 按下标对齐的「rewind 前磁盘状态」。
    pub undo_snaps: Vec<FileSnap>,
}

/// conversation-only 回退控制器。
///
/// 文件恢复（CODE）本里程碑不实现——见模块头注释。
pub struct RewindController;

impl RewindController {
    /// 只读规划：CODE 范围从 `store` 取文件方案；CONVERSATION 范围文件快照为空。
    /// （`store` 由调用方持有，此处只装配模型。）
    pub fn prepare(
        session_id: &str,
        turn: i64,
        scope: RewindScope,
        file_snaps: Vec<FileSnap>,
    ) -> RewindPlan {
        RewindPlan {
            session_id: session_id.to_string(),
            turn,
            scope,
            file_snaps: if scope == RewindScope::Conversation {
                Vec::new()
            } else {
                file_snaps
            },
        }
    }

    /// 执行方案（对话侧）。
    ///
    /// * CONVERSATION / BOTH：把会话 head 回退到 `plan.turn` 对应的消息——
    ///   但 Rust 侧 turn→msg_id 的映射尚未接线，故本阶段**仅返回 partial 结果并附带说明**，
    ///   真正的 `session.rewind_to(msg_id)` 调用由上游拿到锚点后驱动（见
    ///   [`Self::seek_conversation`]）。
    /// * CODE：文件恢复未实现 → `partial = true`。
    ///
    /// `partial` 语义对齐太墟：存在无法满足的部分即 true。
    pub fn commit(plan: &RewindPlan) -> RewindResult {
        let (partial, note) = match plan.scope {
            RewindScope::Code => (
                true,
                "文件恢复（CODE 范围）本里程碑未接线：Rust 侧尚无工作区文件访问层。".to_string(),
            ),
            RewindScope::Conversation => (
                true,
                "对话回退需上游提供该轮锚点消息 id 后调用 SessionStore::rewind_to。".to_string(),
            ),
            RewindScope::Both => (
                true,
                "对话回退需上游提供该轮锚点消息 id 后调用 SessionStore::rewind_to。\n\
                 文件恢复（CODE 范围）本里程碑未接线。"
                    .to_string(),
            ),
        };
        RewindResult {
            files_restored: 0,
            files_deleted: 0,
            partial,
            note: Some(note),
            forked_session_id: None,
            conflicts: Vec::new(),
        }
    }

    /// 把会话回退到「锚点消息之前」——委托既有 [`SessionStore::rewind_to`]。
    ///
    /// `anchor_message_id` 来自 `CheckpointStore::anchor_message_id_of`。
    /// 该消息必须属于本会话（`rewind_to` 内部校验，跨会话串改会被拒）。
    pub fn seek_conversation(
        store: &SessionStore,
        session_id: &str,
        anchor_message_id: i64,
    ) -> Result<RewindResult> {
        store.rewind_to(session_id, anchor_message_id)?;
        Ok(RewindResult {
            files_restored: 0,
            files_deleted: 0,
            partial: false,
            note: Some(format!("会话 head 已回退到消息 {anchor_message_id}。")),
            forked_session_id: None,
            conflicts: Vec::new(),
        })
    }

    /// 撤销最近一次 rewind —— 单层级。Rust 侧文件层未落地，恒返回 `None`。
    pub fn undo_last_rewind(_session_id: &str) -> Option<RewindResult> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint::snap_before;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_path(tag: &str) -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir()
            .join(format!("ne-agent-rewind-{tag}-{}-{n}.db", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn prepare_drops_file_snaps_for_conversation_scope() {
        let snaps = vec![snap_before("a.txt", Some("x"))];
        let p = RewindController::prepare("s", 0, RewindScope::Conversation, snaps.clone());
        assert!(p.file_snaps.is_empty());
        let p2 = RewindController::prepare("s", 0, RewindScope::Code, snaps);
        assert_eq!(p2.file_snaps.len(), 1);
    }

    #[test]
    fn commit_code_scope_is_partial_not_silent_success() {
        let plan = RewindPlan::new("s", 0, RewindScope::Code);
        let r = RewindController::commit(&plan);
        assert!(r.partial, "文件恢复未实现必须显式 partial，绝不静默成功");
        assert_eq!(r.files_restored, 0);
        assert_eq!(r.files_deleted, 0);
        assert!(r.note.unwrap().contains("文件恢复"));
    }

    #[test]
    fn commit_conversation_scope_reports_pending_anchor() {
        let plan = RewindPlan::new("s", 0, RewindScope::Conversation);
        let r = RewindController::commit(&plan);
        assert!(r.partial);
        assert!(r.note.unwrap().contains("锚点"));
    }

    #[test]
    fn seek_conversation_rewinds_head_via_session_store() {
        let p = temp_path("seek");
        let s = SessionStore::open(&p).unwrap();
        let sid = s.create_session("t").unwrap();
        let m1 = s.append(&sid, None, "user", "第一轮").unwrap();
        let _m2 = s.append(&sid, Some(m1), "assistant", "回复").unwrap();
        let m3 = s.append(&sid, Some(_m2), "user", "第二轮").unwrap();
        assert_eq!(s.head(&sid).unwrap(), Some(m3));
        // 回退到 m1 之前即是 m1 本身（head 指向它）
        let r = RewindController::seek_conversation(&s, &sid, m1).unwrap();
        assert!(!r.partial);
        assert_eq!(s.head(&sid).unwrap(), Some(m1));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn seek_conversation_rejects_foreign_message() {
        let p = temp_path("foreign");
        let s = SessionStore::open(&p).unwrap();
        let a = s.create_session("a").unwrap();
        let b = s.create_session("b").unwrap();
        let mb = s.append(&b, None, "user", "b 的消息").unwrap();
        // 用 b 的消息去回退 a：必须被拒（跨会话串改防护）
        assert!(RewindController::seek_conversation(&s, &a, mb).is_err());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn undo_without_file_layer_returns_none() {
        assert!(RewindController::undo_last_rewind("s").is_none());
    }

    #[test]
    fn models_construct_and_compare() {
        let rec = RewindUndoRecord {
            applied: vec![snap_before("a", Some("v"))],
            undo_snaps: vec![snap_before("a", None)],
        };
        assert_eq!(rec.applied.len(), 1);
        assert_eq!(rec.undo_snaps[0].content, None);
        assert_eq!(RewindScope::Both, RewindScope::Both);
        assert_ne!(RewindScope::Code, RewindScope::Conversation);
    }
}