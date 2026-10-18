//! 会话系统 —— **会话树** + 持久化 + 恢复。
//!
//! # 为什么是树而不是线性表
//!
//! 这是 M1 的**不可逆决定之一**（见 docs/ROADMAP-ne-agent.md）。
//! 线性表（`messages(session_id, seq)`）看起来够用，但：
//!   * 无法做 rewind / 分支（同一会话从某轮分叉试两条路）
//!   * 无法做"编辑历史消息后重跑"
//!   * compaction 需要"某段被摘要替换"，线性表只能删不能留
//! 而 `<id, parent_id>` 的树**在 v1 走单链时行为与线性表完全一致**，
//! 代价只是多一个字段。等 M5 要 rewind 时，结构已经就位。
//!
//! # 存储
//!
//! 落在 vault 的 `agent.db`（与小说 `novel.db` 分家，见 vault.rs）。

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};

/// 会话库 schema。全部 `IF NOT EXISTS`，可重复执行。
pub const SESSION_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS sessions (
    id                TEXT PRIMARY KEY,
    title             TEXT NOT NULL DEFAULT '',
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL,
    -- 会话树：一个会话可从另一个会话派生（子会话）
    parent_session_id TEXT,
    -- 当前 head（M5）：为 rewind 服务。NULL = 未设（此时按 MAX(id) 兜底）
    head_id           INTEGER,
    meta              TEXT
);

CREATE TABLE IF NOT EXISTS messages (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    -- 会话树：本消息的父消息。单链模式下 = 上一条消息的 id
    parent_id  INTEGER,
    role       TEXT NOT NULL,          -- system | user | assistant | tool
    content    TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    tokens     INTEGER NOT NULL DEFAULT 0,
    meta       TEXT
);

CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, id);

CREATE TABLE IF NOT EXISTS kv (
    k TEXT PRIMARY KEY,
    v TEXT NOT NULL
);

-- 压缩记录（M9）：**不删原文**，只记"哪段被折叠进哪段摘要"。
-- 投影时用最新一条把 [最早..keep_from_msg_id) 折叠为摘要；原文仍在 messages 表中，
-- 可按 id 回读（history_read）。追加式：多次压缩各自留痕，可审计、可回滚。
CREATE TABLE IF NOT EXISTS compactions (
    id                     INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id             TEXT NOT NULL,
    summary                TEXT NOT NULL,
    -- 折叠边界：该消息 id **及之后**保留原文；之前的被摘要替代
    keep_from_msg_id       INTEGER NOT NULL,
    folded_message_count   INTEGER NOT NULL DEFAULT 0,
    retained_message_count INTEGER NOT NULL DEFAULT 0,
    est_tokens_before      INTEGER NOT NULL DEFAULT 0,
    mode                   TEXT NOT NULL DEFAULT '',
    created_at             INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_compactions_session ON compactions(session_id, id);

-- 检查点（S3-a）：turn-based 文件快照 + 字节预算裁剪。
-- 对齐太墟 checkpoint/CheckpointStore.kt：每个用户轮次记该轮触碰文件的轮初内容；
-- files_json 为 Vec<FileSnap> 的 JSON；bytes 为该轮快照近似字节数（预算裁剪依据）。
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

/// 一条会话的元信息。
#[derive(Debug, Clone)]
pub struct SessionRow {
    pub id: String,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub parent_session_id: Option<String>,
}

/// 一条消息（含树指针）。
#[derive(Debug, Clone)]
pub struct MsgRow {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub role: String,
    pub content: String,
    pub created_at: i64,
    /// 附加字段的 JSON（工具调用配对信息 `tool_calls` / `tool_call_id`）。
    /// 文本协议时代为空；NATIVE 协议下用于在投影里重建配对。
    pub meta: Option<String>,
}

impl MsgRow {
    /// 转成 ne-engine 的 LLM 消息（复用现有类型，不另立一套）。
    pub fn to_llm(&self) -> ne_engine::llm::Message {
        ne_engine::llm::Message {
            role: self.role.clone(),
            content: self.content.clone(),
            tool_calls: self.tool_calls(),
            tool_call_id: self.tool_call_id(),
        }
    }

    /// 该消息携带的工具调用（来自 meta 列；非 assistant 或无 meta 则为 None）。
    pub fn tool_calls(&self) -> Option<Vec<ne_engine::llm::ToolCall>> {
        let meta = self.meta.as_deref()?;
        let v: serde_json::Value = serde_json::from_str(meta).ok()?;
        let calls: Vec<ne_engine::llm::ToolCall> =
            serde_json::from_value(v.get("tool_calls")?.clone()).ok()?;
        if calls.is_empty() { None } else { Some(calls) }
    }

    /// 该消息配对的 tool_call_id（来自 meta 列）。
    pub fn tool_call_id(&self) -> Option<String> {
        let meta = self.meta.as_deref()?;
        let v: serde_json::Value = serde_json::from_str(meta).ok()?;
        v.get("tool_call_id")?.as_str().map(|s| s.to_string())
    }
}

/// 一条压缩记录（M9）。**只记元信息与摘要**，原文永远留在 `messages` 表。
#[derive(Debug, Clone)]
pub struct CompactionRow {
    pub id: i64,
    pub session_id: String,
    pub summary: String,
    /// 该消息 id **及之后**保留原文；之前的已被摘要替代。
    pub keep_from_msg_id: i64,
    pub folded_message_count: i64,
    pub retained_message_count: i64,
    pub est_tokens_before: i64,
    pub mode: String,
    pub created_at: i64,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 生成会话 id（时间戳 + 进程内计数，无外部依赖）。
fn gen_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("s{:x}-{:x}", now_ms(), n)
}

pub struct SessionStore {
    con: Connection,
    path: PathBuf,
}

impl SessionStore {
    /// 打开（或创建）会话库。`path` 通常是 `vault.agent_db()`。
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("创建会话库目录失败：{}", dir.display()))?;
        }
        let con = Connection::open(&path)
            .with_context(|| format!("打开会话库失败：{}", path.display()))?;
        con.execute_batch(SESSION_SCHEMA)
            .context("建会话库 schema 失败")?;
        // 迁移：老库补 head_id 列（M5 前建的库没有）
        ensure_column(&con, "sessions", "head_id", "INTEGER")?;
        Ok(SessionStore { con, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    // ───────────────────────────── 会话

    pub fn create_session(&self, title: &str) -> Result<String> {
        let id = gen_id();
        let t = now_ms();
        self.con.execute(
            "INSERT INTO sessions (id, title, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
            params![id, title, t],
        )?;
        Ok(id)
    }

    /// 派生一个子会话（会话树）。
    pub fn derive_session(&self, parent: &str, title: &str) -> Result<String> {
        let id = gen_id();
        let t = now_ms();
        self.con.execute(
            "INSERT INTO sessions (id, title, created_at, updated_at, parent_session_id)
             VALUES (?1, ?2, ?3, ?3, ?4)",
            params![id, title, t, parent],
        )?;
        Ok(id)
    }

    /// 用**指定 id** 建子会话（子智能体 Lane 用；id 确定 → 可复用以续跑）。
    ///
    /// 幂等：已存在则不动（返回 false 表示已存在）。
    pub fn create_child_session(&self, id: &str, parent: &str, title: &str) -> Result<bool> {
        let t = now_ms();
        let n = self.con.execute(
            "INSERT OR IGNORE INTO sessions
                (id, title, created_at, updated_at, parent_session_id)
             VALUES (?1, ?2, ?3, ?3, ?4)",
            params![id, title, t, parent],
        )?;
        Ok(n > 0)
    }

    pub fn get_session(&self, id: &str) -> Result<Option<SessionRow>> {
        let r = self
            .con
            .query_row(
                "SELECT id, title, created_at, updated_at, parent_session_id FROM sessions WHERE id = ?1",
                params![id],
                |r| {
                    Ok(SessionRow {
                        id: r.get(0)?,
                        title: r.get(1)?,
                        created_at: r.get(2)?,
                        updated_at: r.get(3)?,
                        parent_session_id: r.get(4)?,
                    })
                },
            )
            .optional()?;
        Ok(r)
    }

    /// 最近活跃的会话（`--resume` 用）。
    pub fn latest_session(&self) -> Result<Option<SessionRow>> {
        let r = self
            .con
            .query_row(
                "SELECT id, title, created_at, updated_at, parent_session_id
                 FROM sessions ORDER BY updated_at DESC, rowid DESC LIMIT 1",
                [],
                |r| {
                    Ok(SessionRow {
                        id: r.get(0)?,
                        title: r.get(1)?,
                        created_at: r.get(2)?,
                        updated_at: r.get(3)?,
                        parent_session_id: r.get(4)?,
                    })
                },
            )
            .optional()?;
        Ok(r)
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionRow>> {
        let mut st = self.con.prepare(
            "SELECT id, title, created_at, updated_at, parent_session_id
             FROM sessions ORDER BY updated_at DESC, rowid DESC",
        )?;
        let rows = st.query_map([], |r| {
            Ok(SessionRow {
                id: r.get(0)?,
                title: r.get(1)?,
                created_at: r.get(2)?,
                updated_at: r.get(3)?,
                parent_session_id: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    // ───────────────────────────── 消息

    /// 追加一条消息。`parent` 为 None 表示会话第一条（单链模式下，
    /// 调用方通常传当前 head）。
    pub fn append(
        &self,
        session: &str,
        parent: Option<i64>,
        role: &str,
        content: &str,
    ) -> Result<i64> {
        self.append_meta(session, parent, role, content, None)
    }

    /// 追加一条消息（可带 meta JSON）。
    ///
    /// `meta` 存协议侧附加字段：assistant 轮的 `{"tool_calls":[...]}`、
    /// tool 轮的 `{"tool_call_id":"..."}`。NATIVE 工具协议靠它重建配对。
    pub fn append_meta(
        &self,
        session: &str,
        parent: Option<i64>,
        role: &str,
        content: &str,
        meta: Option<&str>,
    ) -> Result<i64> {
        let t = now_ms();
        self.con.execute(
            "INSERT INTO messages (session_id, parent_id, role, content, created_at, meta)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![session, parent, role, content, t, meta],
        )?;
        let id = self.con.last_insert_rowid();
        // 追新消息即推进 head（rewind 后 append 会从新位置继续，形成分支）
        self.con.execute(
            "UPDATE sessions SET updated_at = ?1, head_id = ?2 WHERE id = ?3",
            params![t, id, session],
        )?;
        Ok(id)
    }

    /// 回退到某条消息：把 head 指向它（其后消息**保留在库中**，只是不再属于当前分支）。
    ///
    /// 语义对齐太墟 `SessionTreeStore.rewindBefore`：不删数据，只改指针。
    /// 之后 `history()` 会沿新 head 回溯，`append()` 会从新 head 分叉。
    ///
    /// 校验：该消息必须属于本会话（防止跨会话串改）。
    pub fn rewind_to(&self, session: &str, msg_id: i64) -> Result<()> {
        let ok: Option<i64> = self
            .con
            .query_row(
                "SELECT id FROM messages WHERE id = ?1 AND session_id = ?2",
                params![msg_id, session],
                |r| r.get(0),
            )
            .optional()?;
        if ok.is_none() {
            anyhow::bail!("消息 {msg_id} 不属于会话 {session}（或不存在）");
        }
        self.con.execute(
            "UPDATE sessions SET head_id = ?1, updated_at = ?2 WHERE id = ?3",
            params![msg_id, now_ms(), session],
        )?;
        Ok(())
    }

    /// 回退 `n` 条消息（沿当前分支）。`n=0` 无操作。
    /// 返回回退后的 head（到顶则为该分支最早一条）。
    pub fn rewind_back(&self, session: &str, n: usize) -> Result<Option<i64>> {
        let hist = self.history(session)?;
        if hist.is_empty() {
            return Ok(None);
        }
        let idx = hist.len().saturating_sub(1 + n);
        let target = hist[idx].id;
        self.rewind_to(session, target)?;
        Ok(Some(target))
    }

    /// 回退到最后一条 user 消息之前（"重来这一轮"）。
    ///
    /// 返回新的 head：指向**该 user 消息之前**的消息；若该 user 就是首条，则清空 head。
    pub fn rewind_last_turn(&self, session: &str) -> Result<Option<i64>> {
        let hist = self.history(session)?;
        let Some(ui) = hist.iter().rposition(|m| m.role == "user") else {
            return Ok(self.head(session)?);
        };
        if ui == 0 {
            // 首条就是 user → head 置 -1（哨兵：显式空分支）；不删消息
            self.con.execute(
                "UPDATE sessions SET head_id = -1, updated_at = ?1 WHERE id = ?2",
                params![now_ms(), session],
            )?;
            return Ok(None);
        }
        let target = hist[ui - 1].id;
        self.rewind_to(session, target)?;
        Ok(Some(target))
    }

    /// 当前 head：优先用显式 `head_id`（rewind 后可能不是 MAX(id)），
    /// 否则回退 `MAX(id)`（老库/未设 head 的会话）。
    ///
    /// `head_id = -1` 是哨兵：显式表示"分支已清空"（rewind_last_turn 撤掉首条 user 时）。
    pub fn head(&self, session: &str) -> Result<Option<i64>> {
        // 1) 显式 head_id
        let explicit: Option<i64> = self
            .con
            .query_row(
                "SELECT head_id FROM sessions WHERE id = ?1",
                params![session],
                |r| r.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten();
        match explicit {
            Some(-1) => return Ok(None), // 哨兵：空分支
            Some(id) => return Ok(Some(id)),
            None => {}
        }
        // 2) 回退 MAX(id)
        let r = self
            .con
            .query_row(
                "SELECT MAX(id) FROM messages WHERE session_id = ?1",
                params![session],
                |r| r.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten();
        Ok(r)
    }

    /// 沿 `parent_id` 从 head 回溯，返回**该分支**的线性历史。
    ///
    /// 这是树结构的正确读法：即便会话内存在分叉，也只会取出当前 head
    /// 所属的那条路径。
    pub fn history(&self, session: &str) -> Result<Vec<MsgRow>> {
        let Some(head) = self.head(session)? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        let mut cur = Some(head);
        while let Some(id) = cur {
            let row = self
                .con
                .query_row(
                    "SELECT id, parent_id, role, content, created_at, meta FROM messages WHERE id = ?1",
                    params![id],
                    |r| {
                        Ok(MsgRow {
                            id: r.get(0)?,
                            parent_id: r.get(1)?,
                            role: r.get(2)?,
                            content: r.get(3)?,
                            created_at: r.get(4)?,
                            meta: r.get(5)?,
                        })
                    },
                )
                .optional()?;
            match row {
                Some(m) => {
                    cur = m.parent_id;
                    out.push(m);
                }
                None => break,
            }
        }
        out.reverse();
        Ok(out)
    }

    /// 某消息的直接子节点（分支点会 >1）。
    pub fn children(&self, session: &str, id: i64) -> Result<Vec<i64>> {
        let mut st = self.con.prepare(
            "SELECT id FROM messages WHERE session_id = ?1 AND parent_id = ?2 ORDER BY id",
        )?;
        let rows = st.query_map(params![session, id], |r| r.get::<_, i64>(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn message_count(&self, session: &str) -> Result<i64> {
        let n: i64 = self.con.query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
            params![session],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    // ───────────────────────────── 压缩（M9）

    /// 落一条压缩记录。范围校验交给调用方（[`crate::compaction`]）。
    #[allow(clippy::too_many_arguments)]
    pub fn insert_compaction(
        &self,
        session: &str,
        summary: &str,
        keep_from_msg_id: i64,
        folded: i64,
        retained: i64,
        est_before: i64,
        mode: &str,
    ) -> Result<i64> {
        self.con.execute(
            "INSERT INTO compactions
                (session_id, summary, keep_from_msg_id, folded_message_count,
                 retained_message_count, est_tokens_before, mode, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![session, summary, keep_from_msg_id, folded, retained, est_before, mode, now_ms()],
        )?;
        Ok(self.con.last_insert_rowid())
    }

    /// 最近一条压缩记录（`None` = 该会话从未压缩）。
    pub fn latest_compaction(&self, session: &str) -> Result<Option<CompactionRow>> {
        let r = self
            .con
            .query_row(
                "SELECT id, session_id, summary, keep_from_msg_id, folded_message_count,
                        retained_message_count, est_tokens_before, mode, created_at
                 FROM compactions WHERE session_id = ?1 ORDER BY id DESC LIMIT 1",
                params![session],
                row_to_compaction,
            )
            .optional()?;
        Ok(r)
    }

    /// 全部压缩记录（按时间正序；供 UI 展示压缩历史）。
    pub fn list_compactions(&self, session: &str) -> Result<Vec<CompactionRow>> {
        let mut st = self.con.prepare(
            "SELECT id, session_id, summary, keep_from_msg_id, folded_message_count,
                    retained_message_count, est_tokens_before, mode, created_at
             FROM compactions WHERE session_id = ?1 ORDER BY id",
        )?;
        let rows = st.query_map(params![session], row_to_compaction)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// 删除某会话全部压缩记录（"还原原始上下文"）。
    pub fn clear_compactions(&self, session: &str) -> Result<usize> {
        let n = self
            .con
            .execute("DELETE FROM compactions WHERE session_id = ?1", params![session])?;
        Ok(n)
    }

    /// 按 id 取一条消息（**回读被折叠的原文**用；不限会话，由调用方校验归属）。
    pub fn message_by_id(&self, id: i64) -> Result<Option<MsgRow>> {
        let r = self
            .con
            .query_row(
                "SELECT id, parent_id, role, content, created_at, meta FROM messages WHERE id = ?1",
                params![id],
                |r| {
                    Ok(MsgRow {
                        id: r.get(0)?,
                        parent_id: r.get(1)?,
                        role: r.get(2)?,
                        content: r.get(3)?,
                        created_at: r.get(4)?,
                        meta: r.get(5)?,
                    })
                },
            )
            .optional()?;
        Ok(r)
    }

    /// 按子串模糊搜索本会话**全部分支**的消息（被折叠的原文也能搜到）。
    ///
    /// 返回 `(消息 id, 角色, 命中片段)`；结果按 id 升序，最多 `limit` 条。
    pub fn search_messages(
        &self,
        session: &str,
        needle: &str,
        limit: usize,
    ) -> Result<Vec<(i64, String, String)>> {
        let like = format!("%{}%", needle.replace('%', "\\%").replace('_', "\\_"));
        let mut st = self.con.prepare(
            "SELECT id, role, content FROM messages
             WHERE session_id = ?1 AND content LIKE ?2 ESCAPE '\\'
             ORDER BY id LIMIT ?3",
        )?;
        let rows = st.query_map(params![session, like, limit as i64], |r| {
            let id: i64 = r.get(0)?;
            let role: String = r.get(1)?;
            let content: String = r.get(2)?;
            Ok((id, role, snippet(&content, needle)))
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}

/// 压缩记录的行映射（两个查询共用）。
fn row_to_compaction(r: &rusqlite::Row) -> rusqlite::Result<CompactionRow> {
    Ok(CompactionRow {
        id: r.get(0)?,
        session_id: r.get(1)?,
        summary: r.get(2)?,
        keep_from_msg_id: r.get(3)?,
        folded_message_count: r.get(4)?,
        retained_message_count: r.get(5)?,
        est_tokens_before: r.get(6)?,
        mode: r.get(7)?,
        created_at: r.get(8)?,
    })
}

/// 取命中位置周围的一段文本（UI 展示用）。
fn snippet(content: &str, needle: &str) -> String {
    const PAD: usize = 60;
    match content.find(needle) {
        None => content.chars().take(120).collect(),
        Some(byte_pos) => {
            let start = content[..byte_pos]
                .char_indices()
                .rev()
                .nth(PAD)
                .map(|(i, _)| i)
                .unwrap_or(0);
            let tail_start = byte_pos + needle.len();
            let end = content[tail_start..]
                .char_indices()
                .nth(PAD)
                .map(|(i, _)| tail_start + i)
                .unwrap_or(content.len());
            let mut s = String::new();
            if start > 0 {
                s.push('…');
            }
            s.push_str(&content[start..end]);
            if end < content.len() {
                s.push('…');
            }
            s
        }
    }
}

/// 幂等地补一列（SQLite 无 `ADD COLUMN IF NOT EXISTS`）。
fn ensure_column(con: &Connection, table: &str, column: &str, ty: &str) -> Result<()> {
    let exists: bool = con
        .prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |r| r.get::<_, String>(1))?
        .filter_map(|x| x.ok())
        .any(|name| name == column);
    if !exists {
        con.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {ty}"))?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;