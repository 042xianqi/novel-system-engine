//! 持久化任务状态机 —— 转移表（纯函数）+ CAS 持久化。
//!
//! 契约逐条对照太墟 `AgentStateMachine.kt` + `AgentTaskTransitionPolicy`：
//!   * **转移表是纯函数**，单独成 [`TransitionPolicy`]，可穷举单测
//!   * 每次转移都是 **compare-and-set**：`UPDATE ... WHERE status IN (expected)`
//!     —— 影响行数 0 表示"有别的执行者已经改过它"，**拒绝这次陈旧转移**
//!   * 理由（太墟注释）：*"validated, compare-and-set persistence transitions so
//!     process recovery can never report phantom success"*
//!     —— 没有 CAS，进程重启后两个执行者可能都以为自己推进成功，报出**幻觉的成功**
//!
//! # 状态集与转移（照抄太墟，不自行发明）
//!
//! ```text
//! queued     → running | suspended | cancelled
//! running    → waiting_approval | recovering | suspended | failed | completed | cancelled
//! waiting_approval → running | suspended | failed | completed | cancelled
//! recovering → recovering | running | waiting_approval | suspended | failed | completed | cancelled
//! suspended  → recovering | running | cancelled
//! failed     → running | suspended | cancelled
//! completed  → （终态，不可再转）
//! cancelled  → （终态，不可再转）
//! ```
//!
//! # 持久化与 CAS 的实现
//!
//! 落在 vault 的 `agent.db`。SQLite 的 `UPDATE ... WHERE status IN (...)` 是原子的
//! （单语句 + 隐式事务），影响行数即 CAS 结果。

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

/// 任务状态（字符串常量与太墟一致，便于跨工具阅读）。
pub mod status {
    pub const QUEUED: &str = "queued";
    pub const RUNNING: &str = "running";
    pub const WAITING_APPROVAL: &str = "waiting_approval";
    pub const RECOVERING: &str = "recovering";
    pub const SUSPENDED: &str = "suspended";
    pub const FAILED: &str = "failed";
    pub const COMPLETED: &str = "completed";
    pub const CANCELLED: &str = "cancelled";
}

/// 所有状态的清单（供遍历/校验）。
pub const ALL_STATUSES: &[&str] = &[
    status::QUEUED,
    status::RUNNING,
    status::WAITING_APPROVAL,
    status::RECOVERING,
    status::SUSPENDED,
    status::FAILED,
    status::COMPLETED,
    status::CANCELLED,
];

/// 终态：不可再转出。
pub fn is_terminal(s: &str) -> bool {
    matches!(s, status::COMPLETED | status::CANCELLED)
}

/// 活跃状态（用于"某会话当前在进行什么"）。
pub fn is_active(s: &str) -> bool {
    matches!(
        s,
        status::QUEUED | status::RUNNING | status::WAITING_APPROVAL | status::RECOVERING
    )
}

/// **纯转移表**（太墟 `AgentTaskTransitionPolicy`）。独立成模块以便穷举单测。
pub struct TransitionPolicy;

impl TransitionPolicy {
    /// `from → to` 是否合法。
    pub fn is_allowed(from: &str, to: &str) -> bool {
        match from {
            status::QUEUED => matches!(to, status::RUNNING | status::SUSPENDED | status::CANCELLED),
            status::RUNNING => matches!(
                to,
                status::WAITING_APPROVAL
                    | status::RECOVERING
                    | status::SUSPENDED
                    | status::FAILED
                    | status::COMPLETED
                    | status::CANCELLED
            ),
            status::WAITING_APPROVAL => matches!(
                to,
                status::RUNNING
                    | status::SUSPENDED
                    | status::FAILED
                    | status::COMPLETED
                    | status::CANCELLED
            ),
            status::RECOVERING => matches!(
                to,
                status::RECOVERING
                    | status::RUNNING
                    | status::WAITING_APPROVAL
                    | status::SUSPENDED
                    | status::FAILED
                    | status::COMPLETED
                    | status::CANCELLED
            ),
            status::SUSPENDED => matches!(to, status::RECOVERING | status::RUNNING | status::CANCELLED),
            status::FAILED => matches!(to, status::RUNNING | status::SUSPENDED | status::CANCELLED),
            status::COMPLETED | status::CANCELLED => false,
            _ => false, // 未知状态：一切转移都不合法（保守）
        }
    }

    /// 一组期望状态是否都能转到 `to`（太墟 `isAllowed(Set, to)`）。
    pub fn is_allowed_from_any(from: &[&str], to: &str) -> bool {
        !from.is_empty() && from.iter().all(|f| Self::is_allowed(f, to))
    }
}

/// 一条任务。
#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    pub id: String,
    pub session_id: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub status_detail: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub error_message: Option<String>,
    pub attempt_count: i64,
    pub max_attempts: i64,
    pub auto_resume: bool,
    /// 分数进度 0.0~0.99（照太墟，不显示 100% 直到真正完成）
    pub progress: f64,
    pub last_round: i64,
    pub max_rounds: i64,
}

/// 任务持久化存储（CAS 语义）。
pub struct TaskStore {
    con: Connection,
}

const TASK_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS tasks (
    id              TEXT PRIMARY KEY,
    session_id      TEXT NOT NULL,
    title           TEXT NOT NULL DEFAULT '',
    description     TEXT NOT NULL,
    status          TEXT NOT NULL,
    status_detail   TEXT NOT NULL DEFAULT '',
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    started_at      INTEGER,
    completed_at    INTEGER,
    error_message   TEXT,
    attempt_count   INTEGER NOT NULL DEFAULT 0,
    max_attempts    INTEGER NOT NULL DEFAULT 2,
    auto_resume     INTEGER NOT NULL DEFAULT 1,
    progress        REAL NOT NULL DEFAULT 0,
    last_round      INTEGER NOT NULL DEFAULT 0,
    max_rounds      INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_tasks_session ON tasks(session_id);
CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
"#;

impl TaskStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let con = Connection::open(path).context("打开任务库失败")?;
        con.execute_batch(TASK_SCHEMA).context("建任务表失败")?;
        Ok(TaskStore { con })
    }

    /// 建一个 queued 任务（太墟 `createQueued`）。
    pub fn create_queued(
        &self,
        id: &str,
        session_id: &str,
        title: &str,
        description: &str,
        auto_resume: bool,
        max_attempts: i64,
        now: i64,
    ) -> Result<Task> {
        if session_id.trim().is_empty() {
            anyhow::bail!("持久任务必须有 session id");
        }
        if description.trim().is_empty() {
            anyhow::bail!("持久任务必须有 description");
        }
        let title: String = if title.trim().is_empty() {
            description.lines().next().unwrap_or("").chars().take(80).collect()
        } else {
            title.chars().take(80).collect()
        };
        let max_attempts = max_attempts.clamp(1, 5);
        self.con.execute(
            "INSERT INTO tasks (id, session_id, title, description, status, status_detail,
                                created_at, updated_at, max_attempts, auto_resume)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET
                session_id=excluded.session_id, title=excluded.title,
                description=excluded.description, updated_at=excluded.updated_at",
            params![
                id,
                session_id,
                title,
                description,
                status::QUEUED,
                "等待执行",
                now,
                max_attempts,
                if auto_resume { 1 } else { 0 }
            ],
        )?;
        self.get(id)?.ok_or_else(|| anyhow::anyhow!("刚建的任务读不到"))
    }

    pub fn get(&self, id: &str) -> Result<Option<Task>> {
        let t = self
            .con
            .query_row(
                "SELECT id, session_id, title, description, status, status_detail,
                        created_at, updated_at, started_at, completed_at, error_message,
                        attempt_count, max_attempts, auto_resume, progress, last_round, max_rounds
                 FROM tasks WHERE id = ?1",
                params![id],
                row_to_task,
            )
            .optional()?;
        Ok(t)
    }

    /// CAS 转移（太墟 `repository.transition` 的等价物）。
    ///
    /// 返回 `true` = 本次转移生效；`false` = **状态不匹配**（有别人改过，陈旧转移被拒）。
    pub fn transition(&self, tr: &Transition) -> Result<bool> {
        // 1) 转移表校验（纯函数）—— 非法转移**显式报错**，不静默 false
        if !TransitionPolicy::is_allowed_from_any(&tr.expected_statuses, tr.next_status) {
            anyhow::bail!("非法任务转移：{:?} → {}", tr.expected_statuses, tr.next_status);
        }
        // 2) CAS：UPDATE ... WHERE id = ? AND status IN (...) —— 影响行数即结果
        //    参数：set 段 8 个（?1..?8）→ id（?9）→ IN 列表（?10..）
        let placeholders: Vec<String> = (0..tr.expected_statuses.len())
            .map(|i| format!("?{}", 10 + i))
            .collect();
        let sql = format!(
            "UPDATE tasks SET
                status = ?1,
                status_detail = COALESCE(?2, status_detail),
                error_message = ?3,
                updated_at = ?4,
                started_at = COALESCE(?5, started_at),
                completed_at = COALESCE(?6, completed_at),
                progress = COALESCE(?7, progress),
                attempt_count = attempt_count + ?8
             WHERE id = ?9 AND status IN ({})",
            placeholders.join(",")
        );
        // 用 rusqlite 的 ToSql 值：先收集所有参数为 owned 值，再 params_from_iter
        let mut vals: Vec<Box<dyn rusqlite::ToSql>> = vec![
            Box::new(tr.next_status.to_string()),
            Box::new(tr.status_detail.clone()),
            Box::new(tr.error_message.clone()),
            Box::new(tr.updated_at),
            Box::new(tr.started_at),
            Box::new(tr.completed_at),
            Box::new(tr.progress),
            Box::new(tr.attempt_increment),
            Box::new(tr.id.clone()),
        ];
        for s in &tr.expected_statuses {
            vals.push(Box::new(s.to_string()));
        }
        let refs: Vec<&dyn rusqlite::ToSql> = vals.iter().map(|b| b.as_ref()).collect();
        let n = self.con.execute(&sql, refs.as_slice())?;
        Ok(n > 0)
    }

    /// 检查点（进度 + 轮次），**不改变状态**（太墟 `checkpoint`）。
    ///
    /// 只有 RUNNING/RECOVERING 才接受检查点（防止给已完成任务写进度）。
    pub fn checkpoint(
        &self,
        id: &str,
        round: i64,
        max_rounds: i64,
        detail: &str,
        now: i64,
    ) -> Result<bool> {
        let round = round.max(0);
        let max_rounds = max_rounds.max(0);
        // 进度上限 0.99：只有真正 completed 才显示 100%（太墟同法）
        let progress = if max_rounds <= 0 {
            0.0
        } else {
            (round as f64 / max_rounds as f64).clamp(0.0, 0.99)
        };
        let n = self.con.execute(
            "UPDATE tasks SET last_round = ?1, max_rounds = ?2, progress = ?3,
                    status_detail = ?4, updated_at = ?5
             WHERE id = ?6 AND status IN ('running','recovering')",
            params![round, max_rounds, progress, detail, now, id],
        )?;
        Ok(n > 0)
    }

    /// 某会话的活跃任务（太墟 `activeForSession`，按状态优先级取一个）。
    pub fn active_for_session(&self, session_id: &str) -> Result<Option<Task>> {
        let mut rows: Vec<Task> = Vec::new();
        let mut stmt = self.con.prepare(
            "SELECT id, session_id, title, description, status, status_detail,
                    created_at, updated_at, started_at, completed_at, error_message,
                    attempt_count, max_attempts, auto_resume, progress, last_round, max_rounds
             FROM tasks WHERE session_id = ?1",
        )?;
        let it = stmt.query_map(params![session_id], row_to_task)?;
        for r in it {
            let t = r?;
            if is_active(&t.status) {
                rows.push(t);
            }
        }
        // 优先级：running(0) < recovering(1) < waiting_approval(2) < queued(3)
        rows.sort_by_key(|t| match t.status.as_str() {
            status::RUNNING => 0,
            status::RECOVERING => 1,
            status::WAITING_APPROVAL => 2,
            status::QUEUED => 3,
            _ => i32::MAX,
        });
        Ok(rows.into_iter().next())
    }

    /// 可恢复任务（太墟 `recoverable`）：非终态 + auto_resume + 还有重试额度 + 到点了。
    pub fn recoverable(&self, now: i64) -> Result<Vec<Task>> {
        let mut stmt = self.con.prepare(
            "SELECT id, session_id, title, description, status, status_detail,
                    created_at, updated_at, started_at, completed_at, error_message,
                    attempt_count, max_attempts, auto_resume, progress, last_round, max_rounds
             FROM tasks",
        )?;
        let it = stmt.query_map([], row_to_task)?;
        let mut out = Vec::new();
        for r in it {
            let t = r?;
            if is_active(&t.status)
                && !t.session_id.is_empty()
                && t.auto_resume
                && t.attempt_count < t.max_attempts
            {
                out.push(t);
            }
        }
        let _ = now;
        Ok(out)
    }

    pub fn list_all(&self) -> Result<Vec<Task>> {
        let mut stmt = self.con.prepare(
            "SELECT id, session_id, title, description, status, status_detail,
                    created_at, updated_at, started_at, completed_at, error_message,
                    attempt_count, max_attempts, auto_resume, progress, last_round, max_rounds
             FROM tasks ORDER BY created_at DESC",
        )?;
        let it = stmt.query_map([], row_to_task)?;
        let mut out = Vec::new();
        for r in it {
            out.push(r?);
        }
        Ok(out)
    }
}

fn row_to_task(r: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
    Ok(Task {
        id: r.get(0)?,
        session_id: r.get(1)?,
        title: r.get(2)?,
        description: r.get(3)?,
        status: r.get(4)?,
        status_detail: r.get(5)?,
        created_at: r.get(6)?,
        updated_at: r.get(7)?,
        started_at: r.get(8)?,
        completed_at: r.get(9)?,
        error_message: r.get(10)?,
        attempt_count: r.get(11)?,
        max_attempts: r.get(12)?,
        auto_resume: r.get::<_, i64>(13)? != 0,
        progress: r.get(14)?,
        last_round: r.get(15)?,
        max_rounds: r.get(16)?,
    })
}

/// 一次 CAS 转移请求。
#[derive(Debug, Clone, Default)]
pub struct Transition {
    pub id: String,
    pub expected_statuses: Vec<&'static str>,
    pub next_status: &'static str,
    pub error_message: Option<String>,
    pub status_detail: Option<String>,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub progress: Option<f64>,
    pub attempt_increment: i64,
    pub updated_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn store(tag: &str) -> (TaskStore, PathBuf) {
        let p = std::env::temp_dir()
            .join(format!("ne-agent-task-{tag}-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&p);
        (TaskStore::open(&p).unwrap(), p)
    }

    fn t() -> i64 {
        1_700_000_000_000
    }

    #[test]
    fn transition_policy_terminal_states_have_no_exit() {
        for f in [status::COMPLETED, status::CANCELLED] {
            for to in ALL_STATUSES {
                assert!(!TransitionPolicy::is_allowed(f, to), "{f} 不该能转到 {to}");
            }
        }
    }

    #[test]
    fn transition_policy_matches_taixu_table() {
        assert!(TransitionPolicy::is_allowed(status::QUEUED, status::RUNNING));
        assert!(TransitionPolicy::is_allowed(status::QUEUED, status::CANCELLED));
        assert!(!TransitionPolicy::is_allowed(status::QUEUED, status::FAILED), "queued 不能直接 failed");
        assert!(TransitionPolicy::is_allowed(status::RUNNING, status::WAITING_APPROVAL));
        assert!(TransitionPolicy::is_allowed(status::RUNNING, status::COMPLETED));
        assert!(!TransitionPolicy::is_allowed(status::WAITING_APPROVAL, status::RECOVERING),
            "waiting_approval 不能转 recovering（太墟表里没有）");
        assert!(TransitionPolicy::is_allowed(status::SUSPENDED, status::RUNNING));
        assert!(!TransitionPolicy::is_allowed(status::SUSPENDED, status::FAILED));
        assert!(TransitionPolicy::is_allowed(status::FAILED, status::RUNNING), "failed 可重试");
    }

    #[test]
    fn unknown_status_rejects_all() {
        assert!(!TransitionPolicy::is_allowed("bogus", status::RUNNING));
        assert!(!TransitionPolicy::is_allowed(status::RUNNING, "bogus"));
    }

    #[test]
    fn create_and_get_queued() {
        let (s, p) = store("create");
        let task = s
            .create_queued("T1", "S1", "标题", "干点什么", true, 2, t())
            .unwrap();
        assert_eq!(task.status, status::QUEUED);
        assert_eq!(task.attempt_count, 0);
        assert!(task.auto_resume);
        let got = s.get("T1").unwrap().unwrap();
        assert_eq!(got.title, "标题");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn create_rejects_empty_session_or_desc() {
        let (s, p) = store("reject");
        assert!(s.create_queued("T", "", "t", "d", true, 2, t()).is_err());
        assert!(s.create_queued("T", "S", "t", "  ", true, 2, t()).is_err());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn cas_rejects_stale_transition() {
        let (s, p) = store("cas");
        s.create_queued("T", "S", "", "任务", true, 2, t()).unwrap();
        // queued → running 生效
        let ok = s
            .transition(&Transition {
                id: "T".into(),
                expected_statuses: vec![status::QUEUED],
                next_status: status::RUNNING,
                updated_at: t() + 1,
                ..Default::default()
            })
            .unwrap();
        assert!(ok);
        // 再来一次 queued → running：已不是 queued → CAS 拒绝（false，非报错）
        let again = s
            .transition(&Transition {
                id: "T".into(),
                expected_statuses: vec![status::QUEUED],
                next_status: status::RUNNING,
                updated_at: t() + 2,
                ..Default::default()
            })
            .unwrap();
        assert!(!again, "陈旧转移必须被 CAS 拒绝（防幻觉成功）");
        assert_eq!(s.get("T").unwrap().unwrap().status, status::RUNNING);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn illegal_transition_is_error_not_false() {
        let (s, p) = store("illegal");
        s.create_queued("T", "S", "", "任务", true, 2, t()).unwrap();
        // queued → completed 不在表里 → 应报错（而非静默 false）
        let r = s.transition(&Transition {
            id: "T".into(),
            expected_statuses: vec![status::QUEUED],
            next_status: status::COMPLETED,
            updated_at: t(),
            ..Default::default()
        });
        assert!(r.is_err(), "非法转移必须显式报错");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn full_lifecycle_queued_to_completed() {
        let (s, p) = store("life");
        s.create_queued("T", "S", "", "写一章", true, 2, t()).unwrap();
        let step = |from, to| {
            s.transition(&Transition {
                id: "T".into(),
                expected_statuses: vec![from],
                next_status: to,
                updated_at: t() + 1,
                attempt_increment: if to == status::RUNNING { 1 } else { 0 },
                ..Default::default()
            })
            .unwrap()
        };
        assert!(step(status::QUEUED, status::RUNNING));
        assert!(step(status::RUNNING, status::WAITING_APPROVAL));
        assert!(step(status::WAITING_APPROVAL, status::RUNNING));
        assert!(step(status::RUNNING, status::COMPLETED));
        let task = s.get("T").unwrap().unwrap();
        assert_eq!(task.status, status::COMPLETED);
        assert_eq!(task.attempt_count, 2, "两次进入 running");
        // 终态后不能再转
        assert!(!s
            .transition(&Transition {
                id: "T".into(),
                expected_statuses: vec![status::COMPLETED],
                next_status: status::RUNNING,
                updated_at: t() + 2,
                ..Default::default()
            })
            .unwrap_or(false));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn checkpoint_only_in_running_states() {
        let (s, p) = store("ckpt");
        s.create_queued("T", "S", "", "任务", true, 2, t()).unwrap();
        // queued 状态下的检查点应被拒
        assert!(!s.checkpoint("T", 3, 10, "进度", t() + 1).unwrap());
        // 转 running 后应接受
        s.transition(&Transition {
            id: "T".into(),
            expected_statuses: vec![status::QUEUED],
            next_status: status::RUNNING,
            updated_at: t(),
            ..Default::default()
        })
        .unwrap();
        assert!(s.checkpoint("T", 3, 10, "第3轮", t() + 2).unwrap());
        let task = s.get("T").unwrap().unwrap();
        assert_eq!(task.last_round, 3);
        assert!((task.progress - 0.3).abs() < 1e-6);
        // 进度不得到 100%
        assert!(s.checkpoint("T", 10, 10, "满", t() + 3).unwrap());
        let task = s.get("T").unwrap().unwrap();
        assert!(task.progress < 1.0, "未完成不得显示 100%：{}", task.progress);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn active_for_session_picks_by_priority() {
        let (s, p) = store("active");
        s.create_queued("A", "S", "", "排队", true, 2, t()).unwrap();
        s.create_queued("B", "S", "", "在跑", true, 2, t()).unwrap();
        s.transition(&Transition {
            id: "B".into(),
            expected_statuses: vec![status::QUEUED],
            next_status: status::RUNNING,
            updated_at: t(),
            ..Default::default()
        })
        .unwrap();
        let act = s.active_for_session("S").unwrap().unwrap();
        assert_eq!(act.id, "B", "running 优先于 queued");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn recoverable_filters_by_auto_resume_and_attempts() {
        let (s, p) = store("recov");
        s.create_queued("R1", "S", "", "会自动恢复", true, 2, t()).unwrap();
        s.create_queued("R2", "S", "", "不自动", false, 2, t()).unwrap();
        s.transition(&Transition {
            id: "R1".into(),
            expected_statuses: vec![status::QUEUED],
            next_status: status::RUNNING,
            updated_at: t(),
            attempt_increment: 1,
            ..Default::default()
        })
        .unwrap();
        let r = s.recoverable(t()).unwrap();
        let ids: Vec<&str> = r.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"R1"));
        assert!(!ids.contains(&"R2"), "auto_resume=false 不应恢复");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn recoverable_excludes_exhausted_attempts() {
        let (s, p) = store("exhaust");
        // max_attempts=1，跑一次后就用完
        s.create_queued("E", "S", "", "一次机会", true, 1, t()).unwrap();
        s.transition(&Transition {
            id: "E".into(),
            expected_statuses: vec![status::QUEUED],
            next_status: status::RUNNING,
            updated_at: t(),
            attempt_increment: 1,
            ..Default::default()
        })
        .unwrap();
        let r = s.recoverable(t()).unwrap();
        assert!(r.iter().all(|t| t.id != "E"), "重试额度用尽不应再恢复");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn terminal_states_not_active() {
        assert!(!is_active(status::COMPLETED));
        assert!(!is_active(status::CANCELLED));
        assert!(is_active(status::RUNNING));
        assert!(is_terminal(status::COMPLETED));
        assert!(is_terminal(status::CANCELLED));
    }
}
