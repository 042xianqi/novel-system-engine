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
}

impl MsgRow {
    /// 转成 ne-engine 的 LLM 消息（复用现有类型，不另立一套）。
    pub fn to_llm(&self) -> ne_engine::llm::Message {
        ne_engine::llm::Message { role: self.role.clone(), content: self.content.clone() }
    }
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
        let t = now_ms();
        self.con.execute(
            "INSERT INTO messages (session_id, parent_id, role, content, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![session, parent, role, content, t],
        )?;
        let id = self.con.last_insert_rowid();
        self.con.execute(
            "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
            params![t, session],
        )?;
        Ok(id)
    }

    /// 当前 head：会话内 id 最大的消息（单链模式下即最新一条）。
    pub fn head(&self, session: &str) -> Result<Option<i64>> {
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
                    "SELECT id, parent_id, role, content, created_at FROM messages WHERE id = ?1",
                    params![id],
                    |r| {
                        Ok(MsgRow {
                            id: r.get(0)?,
                            parent_id: r.get(1)?,
                            role: r.get(2)?,
                            content: r.get(3)?,
                            created_at: r.get(4)?,
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
}

#[cfg(test)]
mod tests {
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
}
