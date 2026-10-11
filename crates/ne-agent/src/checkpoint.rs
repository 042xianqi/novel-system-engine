//! Checkpoint 存储 —— 会话级 turn-based 文件快照 + 字节预算裁剪（S3-a）。
//!
//! 逐字对照太墟 `harness/checkpoint/CheckpointModels.kt` + `CheckpointStore.kt`：
//! 太墟用内存态 + 可选磁盘索引，Rust 侧统一落 `checkpoints` 表（用 `rusqlite`，
//! 与 `session.rs` 同款模式）。行为与常量照抄：
//!   * `MAX_KEPT = 100`：只保留最近 100 轮；
//!   * `SNAPSHOT_MAX_BYTES = 1 MiB`：单文件快照超限即**整体跳过捕获**（太墟同规则）；
//!   * `DEFAULT_MAX_SESSION_BYTES = 64 MiB`：单会话总量超预算时按**最旧**整轮淘汰，
//!     且**永不删最新一条**（`keptFloor` 语义）。
//!
//! # 本里程碑**不接**的
//!
//! 太墟把 `capture/captureAfterImage` 挂在 `ToolExecutor` 的写盘路径上
//! （`ToolExecutor.kt:343/346/353/368/376`）。接线等于改主循环 —— 开工令 §7 明令不接。
//! 本文件只交：模块 + `checkpoints` 表 + `#[cfg(test)]`。
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::rewind::{RewindPlan, RewindScope};
use crate::session::now_ms;

/// 保留的最近轮数上限（太墟 `CheckpointStore.MAX_KEPT`）。
pub const MAX_KEPT: usize = 100;
/// 单文件快照上限：超过即整体跳过捕获（太墟 `CheckpointStore.SNAPSHOT_MAX_BYTES`）。
pub const SNAPSHOT_MAX_BYTES: usize = 1 << 20;
/// 单会话快照总量预算（太墟 `FileCheckpointPersistence.DEFAULT_MAX_SESSION_BYTES`）。
pub const DEFAULT_MAX_SESSION_BYTES: usize = 64 << 20;

/// `checkpoints` 表 DDL；与 `session.rs` 的 `SESSION_SCHEMA` 末尾追加段保持一致。
pub const CHECKPOINT_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS checkpoints (
  session_id        TEXT    NOT NULL,
  turn              INTEGER NOT NULL,
  time              INTEGER NOT NULL,
  prompt            TEXT    NOT NULL DEFAULT '',
  files_json        TEXT    NOT NULL DEFAULT '[]',
  anchor_message_id TEXT,
  bytes             INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (session_id, turn)
);
CREATE INDEX IF NOT EXISTS idx_checkpoints_session ON checkpoints(session_id, turn);
"#;

/// 单次编辑前的文件快照（太墟 `FileSnap`）。
///
/// * `content == None`：该快照时刻文件**不存在**（恢复时需删除文件）。
/// * `after_content`：该路径在本轮内**最后一次成功写入后**的内容（改动后状态）；
///   `None` = 没有可用凭据（写入失败/超大文件跳过/旧数据），回退时不做外部改动冲突检测。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileSnap {
    pub path: String,
    pub content: Option<String>,
    #[serde(default)]
    pub after_content: Option<String>,
}

/// 一个用户轮次的文件快照集合（太墟 `Checkpoint`）。
#[derive(Debug, Clone, PartialEq)]
pub struct Checkpoint {
    /// 0 起始的用户轮次号。
    pub turn: i64,
    pub time: i64,
    /// 该轮用户消息，作为选择器标签。
    pub prompt: String,
    /// 该轮触碰过的文件快照（每个 path 仅保留轮初内容）。
    pub files: Vec<FileSnap>,
    /// 该轮用户消息锚点（供会话回退定位轮次边界）。
    pub anchor_message_id: Option<String>,
}

/// 提供给恢复 UI 的轻量摘要：不含文件内容（太墟 `CheckpointMeta`）。
#[derive(Debug, Clone, PartialEq)]
pub struct CheckpointMeta {
    pub turn: i64,
    pub time: i64,
    pub prompt: String,
    pub changed_files: Vec<String>,
    pub anchor_message_id: Option<String>,
}

/// 会话内**尚未关闭**的轮次状态（内存，对齐太墟 `SessionState.active`）。
struct ActiveTurn {
    turn: i64,
    prompt: String,
    files: Vec<FileSnap>,
}

/// 检查点存储（`checkpoints` 表 + 活动轮内存态）。
pub struct CheckpointStore {
    con: Connection,
    path: PathBuf,
    /// 单会话字节预算；默认 [`DEFAULT_MAX_SESSION_BYTES`]，测试可注入更小值。
    max_session_bytes: usize,
    /// 活动轮：`session_id -> ActiveTurn`。
    active: Mutex<HashMap<String, ActiveTurn>>,
}

impl CheckpointStore {
    /// 打开（或创建）检查点库，用默认字节预算。
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_budget(path, DEFAULT_MAX_SESSION_BYTES)
    }

    /// 打开检查点库并指定单会话字节预算（太墟 `FileCheckpointPersistence(root, maxSessionBytes)`）。
    pub fn open_with_budget(path: impl AsRef<Path>, max_session_bytes: usize) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("创建检查点库目录失败：{}", dir.display()))?;
        }
        let con = Connection::open(&path)
            .with_context(|| format!("打开检查点库失败：{}", path.display()))?;
        con.execute_batch(CHECKPOINT_SCHEMA)
            .context("建 checkpoints 表失败")?;
        Ok(CheckpointStore {
            con,
            path,
            max_session_bytes,
            active: Mutex::new(HashMap::new()),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 开启新的一轮检查点；若上一轮未关闭则先关闭（对齐太墟 `beginTurn`）。
    pub fn begin_turn(&self, session_id: &str, turn: i64, prompt: &str) -> Result<()> {
        self.close_active(session_id, None)?;
        let mut active = self.active.lock().unwrap();
        active.insert(
            session_id.to_string(),
            ActiveTurn {
                turn,
                prompt: prompt.to_string(),
                files: Vec::new(),
            },
        );
        Ok(())
    }

    /// 记录某路径在该轮「触碰前」的快照（太墟 `capture`）。
    ///
    /// * 无活动轮 → 忽略（返回 `false`）；
    /// * 同轮同路径已记录 → 忽略（保留第一次的轮初内容，返回 `false`）；
    /// * 内容超 [`SNAPSHOT_MAX_BYTES`] → **整体跳过捕获**（太墟「超大文件跳过捕获」规则）。
    pub fn capture(&self, session_id: &str, snap: FileSnap) -> Result<bool> {
        if over_snapshot_limit(&snap) {
            return Ok(false);
        }
        let mut active = self.active.lock().unwrap();
        let Some(turn) = active.get_mut(session_id) else {
            return Ok(false);
        };
        if turn.files.iter().any(|f| f.path == snap.path) {
            return Ok(false);
        }
        turn.files.push(snap);
        Ok(true)
    }

    /// 记录某路径**成功写入后**的内容（太墟 `captureAfterImage`）。
    /// 无活动轮或该路径无 pre-image 时忽略（凭据永远与轮初快照成对）。
    pub fn capture_after_image(
        &self,
        session_id: &str,
        path: &str,
        after: Option<String>,
    ) -> Result<bool> {
        if let Some(a) = after.as_ref() {
            if a.len() > SNAPSHOT_MAX_BYTES {
                return Ok(false);
            }
        }
        let mut active = self.active.lock().unwrap();
        let Some(turn) = active.get_mut(session_id) else {
            return Ok(false);
        };
        let Some(snap) = turn.files.iter_mut().find(|f| f.path == path) else {
            return Ok(false);
        };
        snap.after_content = after;
        Ok(true)
    }

    /// 某路径在 store 记录中的**最后改动后凭据**：活动轮优先，其余按关闭轮新→旧取第一个。
    /// `None` = 无凭据（不做外部改动冲突检测）。
    pub fn latest_after_image(&self, session_id: &str, path: &str) -> Result<Option<String>> {
        {
            let active = self.active.lock().unwrap();
            if let Some(turn) = active.get(session_id) {
                if let Some(snap) = turn.files.iter().find(|f| f.path == path) {
                    if snap.after_content.is_some() {
                        return Ok(snap.after_content.clone());
                    }
                }
            }
        }
        let mut st = self.con.prepare(
            "SELECT files_json FROM checkpoints WHERE session_id = ?1 ORDER BY turn DESC",
        )?;
        let rows = st.query_map(params![session_id], |r| r.get::<_, String>(0))?;
        for row in rows {
            let json = row?;
            let files: Vec<FileSnap> = serde_json::from_str(&json).unwrap_or_default();
            if let Some(snap) = files.iter().find(|f| f.path == path) {
                if snap.after_content.is_some() {
                    return Ok(snap.after_content.clone());
                }
            }
        }
        Ok(None)
    }

    /// 关闭当前活动轮并落库（空轮不产生 checkpoint）；`anchor_message_id` 记入该轮。
    pub fn end_turn(&self, session_id: &str, anchor_message_id: Option<&str>) -> Result<()> {
        self.close_active(session_id, anchor_message_id.map(|s| s.to_string()))
    }

    /// 会话删除/重建时清理（太墟 `dropSession`）。
    pub fn drop_session(&self, session_id: &str) -> Result<()> {
        self.con
            .execute("DELETE FROM checkpoints WHERE session_id = ?1", params![session_id])?;
        self.active.lock().unwrap().remove(session_id);
        Ok(())
    }

    /// 该会话全部已关闭轮（按 turn 升序）。
    pub fn checkpoints(&self, session_id: &str) -> Result<Vec<Checkpoint>> {
        let mut st = self.con.prepare(
            "SELECT turn, time, prompt, files_json, anchor_message_id
             FROM checkpoints WHERE session_id = ?1 ORDER BY turn",
        )?;
        let rows = st.query_map(params![session_id], |r| {
            let files_json: String = r.get(3)?;
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                files_json,
                r.get::<_, Option<String>>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (turn, time, prompt, files_json, anchor) = row?;
            out.push(Checkpoint {
                turn,
                time,
                prompt,
                files: serde_json::from_str(&files_json).unwrap_or_default(),
                anchor_message_id: anchor,
            });
        }
        Ok(out)
    }

    /// 轻量摘要列表（不含文件内容，对齐太墟 `checkpoints()` 返回 `CheckpointMeta`）。
    pub fn checkpoint_metas(&self, session_id: &str) -> Result<Vec<CheckpointMeta>> {
        Ok(self
            .checkpoints(session_id)?
            .into_iter()
            .map(|c| CheckpointMeta {
                turn: c.turn,
                time: c.time,
                prompt: c.prompt,
                changed_files: c.files.into_iter().map(|f| f.path).collect(),
                anchor_message_id: c.anchor_message_id,
            })
            .collect())
    }

    /// 某轮的用户消息锚点（供会话回退定位；太墟 `anchorMessageIdOf`）。
    pub fn anchor_message_id_of(&self, session_id: &str, turn: i64) -> Result<Option<String>> {
        let r = self
            .con
            .query_row(
                "SELECT anchor_message_id FROM checkpoints WHERE session_id = ?1 AND turn = ?2",
                params![session_id, turn],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?;
        Ok(r.flatten())
    }

    /// 规划「代码回滚」：撤回到 `to_turn`，对该路径取 `to_turn` 起**最早**的轮初内容
    /// （太墟 `planCodeRewind`）。轮号越界返回空方案。
    pub fn plan_code_rewind(&self, session_id: &str, to_turn: i64) -> Result<RewindPlan> {
        let cps = self.checkpoints(session_id)?;
        let active_turn = {
            let a = self.active.lock().unwrap();
            a.get(session_id).map(|t| t.turn)
        };
        let newest_closed = cps.last().map(|c| c.turn).unwrap_or(-1);
        let newest = match active_turn {
            Some(at) => newest_closed.max(at),
            None => newest_closed,
        };
        let mut plan = RewindPlan {
            session_id: session_id.to_string(),
            turn: to_turn,
            scope: RewindScope::Code,
            file_snaps: Vec::new(),
        };
        if to_turn < 0 || to_turn > newest {
            return Ok(plan);
        }
        let mut seen: HashSet<String> = HashSet::new();
        let mut merged: Vec<FileSnap> = Vec::new();
        for c in cps.iter().filter(|c| c.turn >= to_turn) {
            for f in &c.files {
                if seen.insert(f.path.clone()) {
                    merged.push(f.clone());
                }
            }
        }
        if let Some(at) = active_turn {
            if to_turn <= at {
                let a = self.active.lock().unwrap();
                if let Some(t) = a.get(session_id) {
                    for f in &t.files {
                        if seen.insert(f.path.clone()) {
                            merged.push(f.clone());
                        }
                    }
                }
            }
        }
        plan.file_snaps = merged;
        Ok(plan)
    }

    /// 关闭活动轮并落库（内部）。空轮丢弃、不落库。
    fn close_active(&self, session_id: &str, anchor: Option<String>) -> Result<()> {
        let taken = {
            let mut active = self.active.lock().unwrap();
            active.remove(session_id)
        };
        let Some(turn) = taken else {
            return Ok(());
        };
        if turn.files.is_empty() {
            return Ok(());
        }
        let files_json = serde_json::to_string(&turn.files).context("序列化文件快照失败")?;
        let bytes = turn_bytes(&turn.files) as i64;
        self.con.execute(
            "INSERT INTO checkpoints
                (session_id, turn, time, prompt, files_json, anchor_message_id, bytes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(session_id, turn) DO UPDATE SET
                time = excluded.time,
                prompt = excluded.prompt,
                files_json = excluded.files_json,
                anchor_message_id = excluded.anchor_message_id,
                bytes = excluded.bytes",
            params![session_id, turn.turn, now_ms(), turn.prompt, files_json, anchor, bytes],
        )?;
        self.enforce_max_kept(session_id)?;
        self.enforce_byte_budget(session_id)?;
        Ok(())
    }

    /// 只保留最近 [`MAX_KEPT`] 轮（太墟 `closeTurn` 的 `while size > MAX_KEPT removeAt(0)`）。
    fn enforce_max_kept(&self, session_id: &str) -> Result<()> {
        let n: i64 = self.con.query_row(
            "SELECT COUNT(*) FROM checkpoints WHERE session_id = ?1",
            params![session_id],
            |r| r.get(0),
        )?;
        if (n as usize) > MAX_KEPT {
            let excess = (n as usize - MAX_KEPT) as i64;
            self.con.execute(
                "DELETE FROM checkpoints WHERE session_id = ?1 AND turn IN (
                     SELECT turn FROM checkpoints WHERE session_id = ?1
                     ORDER BY turn ASC LIMIT ?2)",
                params![session_id, excess],
            )?;
        }
        Ok(())
    }

    /// 字节预算：超预算按**最旧**整轮淘汰，当前（最新）轮永不删
    /// （太墟 `enforceByteBudget`：`it != currentTurn`）。
    fn enforce_byte_budget(&self, session_id: &str) -> Result<()> {
        let total: i64 = self.con.query_row(
            "SELECT COALESCE(SUM(bytes), 0) FROM checkpoints WHERE session_id = ?1",
            params![session_id],
            |r| r.get(0),
        )?;
        if (total as usize) <= self.max_session_bytes {
            return Ok(());
        }
        // 候选 = 除最新轮之外的轮，按 turn 升序
        let mut st = self.con.prepare(
            "SELECT turn FROM checkpoints
             WHERE session_id = ?1
               AND turn < (SELECT MAX(turn) FROM checkpoints WHERE session_id = ?1)
             ORDER BY turn ASC",
        )?;
        let turns: Vec<i64> = st
            .query_map(params![session_id], |r| r.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(st);
        let mut remaining = total;
        for turn in turns {
            if (remaining as usize) <= self.max_session_bytes {
                break;
            }
            let bytes: i64 = self.con.query_row(
                "SELECT COALESCE(bytes, 0) FROM checkpoints WHERE session_id = ?1 AND turn = ?2",
                params![session_id, turn],
                |r| r.get(0),
            )?;
            self.con.execute(
                "DELETE FROM checkpoints WHERE session_id = ?1 AND turn = ?2",
                params![session_id, turn],
            )?;
            remaining -= bytes;
        }
        Ok(())
    }
}

/// 单文件快照是否超上限（超限整体跳过捕获）。
fn over_snapshot_limit(snap: &FileSnap) -> bool {
    let big = |c: &Option<String>| c.as_ref().map(|s| s.len() > SNAPSHOT_MAX_BYTES).unwrap_or(false);
    big(&snap.content) || big(&snap.after_content)
}

/// 一轮快照的近似字节数（pre-image + 改动后凭据），用于字节预算。
fn turn_bytes(files: &[FileSnap]) -> usize {
    files
        .iter()
        .map(|f| {
            f.content.as_ref().map(|c| c.len()).unwrap_or(0)
                + f.after_content.as_ref().map(|c| c.len()).unwrap_or(0)
        })
        .sum()
}

/// 便捷构造：既有内容快照（`after_content = None`）。
pub fn snap_before(path: &str, before: Option<&str>) -> FileSnap {
    FileSnap {
        path: path.to_string(),
        content: before.map(|s| s.to_string()),
        after_content: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// 临时库路径（进程 + 序号唯一），验证持久化往返而非内存假象。
    fn temp_path(tag: &str) -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir()
            .join(format!("ne-agent-ckpt-{tag}-{}-{n}.db", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn store(tag: &str) -> (CheckpointStore, PathBuf) {
        let p = temp_path(tag);
        (CheckpointStore::open(&p).unwrap(), p)
    }

    fn snap(path: &str, before: Option<&str>) -> FileSnap {
        snap_before(path, before)
    }

    #[test]
    fn capture_then_latest_after_image() {
        let (s, p) = store("after");
        s.begin_turn("s", 0, "p0").unwrap();
        assert!(s.capture("s", snap("a.txt", Some("old"))).unwrap());
        assert!(s.capture_after_image("s", "a.txt", Some("new".into())).unwrap());
        assert_eq!(s.latest_after_image("s", "a.txt").unwrap(), Some("new".into()));
        // 无凭据的路径返回 None（b.txt 只有 pre-image）
        assert!(s.capture("s", snap("b.txt", None)).unwrap());
        assert_eq!(s.latest_after_image("s", "b.txt").unwrap(), None);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn end_turn_sets_anchor_message_id() {
        let (s, p) = store("anchor");
        s.begin_turn("s", 0, "p0").unwrap();
        assert!(s.capture("s", snap("a.txt", Some("x"))).unwrap());
        s.end_turn("s", Some("anchor-1")).unwrap();
        assert_eq!(s.anchor_message_id_of("s", 0).unwrap(), Some("anchor-1".into()));
        assert_eq!(s.anchor_message_id_of("s", 9).unwrap(), None);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn checkpoints_are_turn_ordered() {
        let (s, p) = store("order");
        for turn in [2i64, 0, 1] {
            s.begin_turn("s", turn, &format!("p{turn}")).unwrap();
            s.capture("s", snap(&format!("f{turn}"), Some("v"))).unwrap();
            s.end_turn("s", None).unwrap();
        }
        let turns: Vec<i64> = s.checkpoints("s").unwrap().iter().map(|c| c.turn).collect();
        assert_eq!(turns, vec![0, 1, 2]);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn max_kept_evicts_oldest() {
        let (s, p) = store("maxkept");
        for turn in 0..(MAX_KEPT as i64 + 3) {
            s.begin_turn("s", turn, "p").unwrap();
            s.capture("s", snap(&format!("f{turn}"), Some("v"))).unwrap();
            s.end_turn("s", None).unwrap();
        }
        let cps = s.checkpoints("s").unwrap();
        assert_eq!(cps.len(), MAX_KEPT);
        assert_eq!(cps.first().unwrap().turn, 3, "最旧的 3 轮应被淘汰");
        assert_eq!(cps.last().unwrap().turn, MAX_KEPT as i64 + 2);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn byte_budget_evicts_oldest_keeps_newest() {
        let p = temp_path("budget");
        // 每轮 ~1.2KB，预算 2500B：写 3 轮必超 → 最早的被整轮淘汰，最新一条保留。
        let s = CheckpointStore::open_with_budget(&p, 2_500).unwrap();
        for turn in 0..3i64 {
            s.begin_turn("s", turn, "p").unwrap();
            let big = "内".repeat((turn as usize + 1) * 400);
            s.capture("s", snap("a.txt", Some(&big))).unwrap();
            s.end_turn("s", None).unwrap();
        }
        let turns: Vec<i64> = s.checkpoints("s").unwrap().iter().map(|c| c.turn).collect();
        assert!(!turns.contains(&0), "最旧轮应被淘汰：{turns:?}");
        assert!(turns.contains(&2), "最新轮必须保留：{turns:?}");
        let total: i64 = s
            .con
            .query_row(
                "SELECT COALESCE(SUM(bytes),0) FROM checkpoints WHERE session_id='s'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(total as usize <= 2_500 + 1_300, "预算应收敛，实际 {total}");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn snapshot_over_1mib_rejected_or_truncated() {
        // 选择「整体跳过捕获」（reject）：对齐太墟 captureBeforeWrite 的
        // “超大文件跳过捕获” 规则——绝不把截断内容当作有效 pre-image（截断会在
        // rewind 时静默损坏文件），宁可不记该路径。
        let (s, p) = store("bigsnap");
        s.begin_turn("s", 0, "p0").unwrap();
        let big = "x".repeat(SNAPSHOT_MAX_BYTES + 1);
        assert!(!s.capture("s", snap("big.txt", Some(&big))).unwrap(), "超 1MiB 应被拒");
        let exact = "x".repeat(SNAPSHOT_MAX_BYTES);
        assert!(s.capture("s", snap("exact.txt", Some(&exact))).unwrap(), "恰好 1MiB 应接受");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn drop_session_removes_all() {
        let (s, p) = store("drop");
        s.begin_turn("s", 0, "p0").unwrap();
        s.capture("s", snap("a.txt", Some("x"))).unwrap();
        s.end_turn("s", None).unwrap();
        assert_eq!(s.checkpoints("s").unwrap().len(), 1);
        s.drop_session("s").unwrap();
        assert!(s.checkpoints("s").unwrap().is_empty());
        // 重开后同样为空（无残留）
        drop(s);
        let s2 = CheckpointStore::open(&p).unwrap();
        assert!(s2.checkpoints("s").unwrap().is_empty());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn persistence_roundtrip_across_reopen() {
        let p = temp_path("roundtrip");
        {
            let s = CheckpointStore::open(&p).unwrap();
            s.begin_turn("s", 0, "第一轮").unwrap();
            s.capture("s", snap("a.txt", Some("old-a"))).unwrap();
            s.capture("s", snap("b.txt", None)).unwrap();
            s.capture_after_image("s", "a.txt", Some("new-a".into())).unwrap();
            s.end_turn("s", Some("anchor-1")).unwrap();
        }
        // 重开：数据仍在（持久化往返）
        let s2 = CheckpointStore::open(&p).unwrap();
        let cps = s2.checkpoints("s").unwrap();
        assert_eq!(cps.len(), 1);
        assert_eq!(cps[0].prompt, "第一轮");
        assert_eq!(cps[0].anchor_message_id.as_deref(), Some("anchor-1"));
        assert_eq!(cps[0].files.len(), 2);
        // 改动后凭据也往返
        assert_eq!(s2.latest_after_image("s", "a.txt").unwrap(), Some("new-a".into()));
        // 回退方案：含 null 快照（该轮创建的文件 → 回滚即删除）
        let plan = s2.plan_code_rewind("s", 0).unwrap();
        assert_eq!(plan.file_snaps.len(), 2);
        assert_eq!(
            plan.file_snaps.iter().find(|f| f.path == "b.txt").unwrap().content,
            None
        );
        let _ = std::fs::remove_file(&p);
    }

    // ── 以下用例名逐条对齐太墟 CheckpointStoreTest.kt ──

    #[test]
    fn capture_without_active_turn_is_ignored() {
        let (s, p) = store("noactive");
        assert!(!s.capture("s", snap("a.txt", Some("x"))).unwrap());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn capture_dedups_per_path_per_turn_and_keeps_first_touch() {
        let (s, p) = store("dedup");
        s.begin_turn("s", 0, "p0").unwrap();
        assert!(s.capture("s", snap("a.txt", Some("turn0-start"))).unwrap());
        assert!(!s.capture("s", snap("a.txt", Some("turn0-after"))).unwrap());
        s.end_turn("s", None).unwrap();
        let cps = s.checkpoints("s").unwrap();
        assert_eq!(cps.len(), 1);
        assert_eq!(cps[0].files.len(), 1);
        assert_eq!(cps[0].files[0].content.as_deref(), Some("turn0-start"));
        assert_eq!(cps[0].files[0].path, "a.txt");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn empty_turn_creates_no_checkpoint() {
        let (s, p) = store("empty");
        s.begin_turn("s", 0, "p0").unwrap();
        s.begin_turn("s", 1, "p1").unwrap(); // 关闭上一轮（空）
        s.end_turn("s", None).unwrap();
        assert!(s.checkpoints("s").unwrap().is_empty());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn code_rewind_takes_earliest_snapshot_per_path_from_target_turn() {
        let (s, p) = store("plan");
        s.begin_turn("s", 0, "p0").unwrap();
        s.capture("s", snap("a.txt", None)).unwrap();
        s.capture("s", snap("b.txt", Some("b0"))).unwrap();
        s.end_turn("s", None).unwrap();
        s.begin_turn("s", 1, "p1").unwrap();
        s.capture("s", snap("a.txt", Some("a1"))).unwrap();
        s.capture("s", snap("c.txt", Some("c1"))).unwrap();
        s.end_turn("s", None).unwrap();
        s.begin_turn("s", 2, "p2").unwrap();
        s.capture("s", snap("a.txt", Some("a2"))).unwrap();
        s.end_turn("s", None).unwrap();

        let to0 = s.plan_code_rewind("s", 0).unwrap();
        let m: HashMap<String, Option<String>> = to0
            .file_snaps
            .iter()
            .map(|f| (f.path.clone(), f.content.clone()))
            .collect();
        assert_eq!(m.get("a.txt").unwrap(), &None, "撤回到 turn0：a.txt 当时不存在");
        assert_eq!(m.get("b.txt").unwrap().as_deref(), Some("b0"));
        assert_eq!(m.get("c.txt").unwrap().as_deref(), Some("c1"));

        let to1 = s.plan_code_rewind("s", 1).unwrap();
        let a1 = to1.file_snaps.iter().find(|f| f.path == "a.txt").unwrap();
        assert_eq!(a1.content.as_deref(), Some("a1"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn rewind_plan_is_empty_for_out_of_range_turns() {
        let (s, p) = store("range");
        s.begin_turn("s", 0, "p0").unwrap();
        s.capture("s", snap("a.txt", None)).unwrap();
        s.end_turn("s", None).unwrap();
        assert!(s.plan_code_rewind("s", 5).unwrap().file_snaps.is_empty());
        assert!(s.plan_code_rewind("s", -1).unwrap().file_snaps.is_empty());
        let _ = std::fs::remove_file(&p);
    }
}
