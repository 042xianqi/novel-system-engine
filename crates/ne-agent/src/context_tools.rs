//! 协作工具 —— `plan`（多步骤计划）+ `scratchpad`（会话局部草稿）。
//!
//! 契约逐条对照太墟 `AgentContextExecutor.kt`（`executePlan` / `executeScratchpad`）。
//!
//! # 从太墟注释里学到的一条硬教训
//!
//! > `plan.status` 是计划生命周期枚举（active/completed/cancelled），**不是**自然语言进度描述。
//! > 模型常把阶段说明塞进 status，直接写入会导致 `getActivePlan` **永久找不到记录**。
//!
//! 所以 [`PlanStatus`] 只接受这三个值；`advance` 时若模型传了别的东西，**保留原状态**
//! 而不是写进去。这是"模型会乱填枚举"的防御。
//!
//! # 为什么这两个工具值得有
//!
//! 长任务（如"写完整卷"）跨很多轮。没有外置的计划/草稿，模型只能靠对话历史记状态——
//! 而历史会被折叠（M5），状态就丢了。把计划和草稿**持久化到库**，模型每轮重新读，
//! 就不受上下文折叠影响。这是"能长跑"的另一半。

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

/// 计划状态（**生命周期枚举**，非自然语言）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanStatus {
    Active,
    Completed,
    Cancelled,
}

impl PlanStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            PlanStatus::Active => "active",
            PlanStatus::Completed => "completed",
            PlanStatus::Cancelled => "cancelled",
        }
    }
    /// 解析；未知值返回 None（调用方据此**保留原状态**，不写入垃圾）。
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "active" => Some(PlanStatus::Active),
            "completed" => Some(PlanStatus::Completed),
            "cancelled" | "canceled" => Some(PlanStatus::Cancelled),
            _ => None,
        }
    }
}

/// 一条计划。
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub session_id: String,
    pub goal: String,
    /// 步骤 JSON（原样存，模型自己组织结构）
    pub steps_json: String,
    pub status: String,
    pub updated_at: i64,
}

/// 一条草稿。
#[derive(Debug, Clone, PartialEq)]
pub struct Scratch {
    pub key: String,
    pub value: String,
    pub updated_at: i64,
}

/// 计划 + 草稿的持久化存储。
pub struct ContextStore {
    con: Connection,
}

const CTX_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS plans (
    session_id  TEXT PRIMARY KEY,
    goal        TEXT NOT NULL,
    steps_json  TEXT NOT NULL DEFAULT '[]',
    status      TEXT NOT NULL DEFAULT 'active',
    updated_at  INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS scratchpads (
    session_id  TEXT NOT NULL,
    key         TEXT NOT NULL,
    value       TEXT NOT NULL,
    updated_at  INTEGER NOT NULL,
    PRIMARY KEY (session_id, key)
);
"#;

/// 限额（照太墟常量）。
pub const MAX_SCRATCH_KEY_CHARS: usize = 128;
pub const MAX_SCRATCH_VALUE_CHARS: usize = 8_192;
pub const MAX_SCRATCH_ITEMS: usize = 200;

impl ContextStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let con = Connection::open(path)?;
        con.execute_batch(CTX_SCHEMA)?;
        Ok(ContextStore { con })
    }

    // ───────────────────── plan

    /// 创建/替换活跃计划（`replace_active`）。
    pub fn save_plan(
        &self,
        session_id: &str,
        goal: &str,
        steps_json: &str,
        status: PlanStatus,
        now: i64,
    ) -> Result<Plan> {
        if goal.trim().is_empty() {
            anyhow::bail!("plan 必须提供 goal 目标描述");
        }
        self.con.execute(
            "INSERT INTO plans (session_id, goal, steps_json, status, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(session_id) DO UPDATE SET
                goal = excluded.goal,
                steps_json = excluded.steps_json,
                status = excluded.status,
                updated_at = excluded.updated_at",
            params![session_id, goal, steps_json, status.as_str(), now],
        )?;
        self.get_plan(session_id)?.ok_or_else(|| anyhow::anyhow!("刚存的计划读不到"))
    }

    /// 取活跃计划（只认 `status = 'active'`，太墟 `getActivePlan` 同法）。
    pub fn get_plan(&self, session_id: &str) -> Result<Option<Plan>> {
        let p = self
            .con
            .query_row(
                "SELECT session_id, goal, steps_json, status, updated_at
                 FROM plans WHERE session_id = ?1 AND status = 'active'",
                params![session_id],
                |r| {
                    Ok(Plan {
                        session_id: r.get(0)?,
                        goal: r.get(1)?,
                        steps_json: r.get(2)?,
                        status: r.get(3)?,
                        updated_at: r.get(4)?,
                    })
                },
            )
            .optional()?;
        Ok(p)
    }

    /// 推进计划：更新步骤 / 状态。`new_status = None` 表示**保留原状态**
    /// （模型给了非法值时就走这条——见模块头注释的教训）。
    pub fn advance_plan(
        &self,
        session_id: &str,
        steps_json: Option<&str>,
        new_status: Option<PlanStatus>,
        now: i64,
    ) -> Result<Option<Plan>> {
        let cur = self.get_plan(session_id)?;
        let Some(cur) = cur else { return Ok(None) };
        let steps = steps_json.unwrap_or(&cur.steps_json);
        let status = new_status.map(|s| s.as_str().to_string()).unwrap_or(cur.status);
        self.con.execute(
            "UPDATE plans SET steps_json = ?1, status = ?2, updated_at = ?3 WHERE session_id = ?4",
            params![steps, status, now, session_id],
        )?;
        self.get_plan(session_id)
    }

    /// 清空计划（`clear_active`）。
    pub fn clear_plan(&self, session_id: &str) -> Result<()> {
        self.con
            .execute("DELETE FROM plans WHERE session_id = ?1", params![session_id])?;
        Ok(())
    }

    // ───────────────────── scratchpad

    pub fn save_scratch(&self, session_id: &str, key: &str, value: &str, now: i64) -> Result<()> {
        let key = key.trim();
        if key.is_empty() {
            anyhow::bail!("scratchpad save 必须提供 key");
        }
        if key.chars().count() > MAX_SCRATCH_KEY_CHARS {
            anyhow::bail!("scratchpad key 不能超过 {MAX_SCRATCH_KEY_CHARS} 字符");
        }
        if value.chars().count() > MAX_SCRATCH_VALUE_CHARS {
            anyhow::bail!("scratchpad value 不能超过 {MAX_SCRATCH_VALUE_CHARS} 字符");
        }
        if self.list_scratch(session_id)?.len() >= MAX_SCRATCH_ITEMS
            && self.get_scratch(session_id, key)?.is_none()
        {
            anyhow::bail!("scratchpad 条目已达上限 {MAX_SCRATCH_ITEMS}，请先清理");
        }
        self.con.execute(
            "INSERT INTO scratchpads (session_id, key, value, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(session_id, key) DO UPDATE SET
                value = excluded.value, updated_at = excluded.updated_at",
            params![session_id, key, value, now],
        )?;
        Ok(())
    }

    pub fn get_scratch(&self, session_id: &str, key: &str) -> Result<Option<Scratch>> {
        let s = self
            .con
            .query_row(
                "SELECT key, value, updated_at FROM scratchpads WHERE session_id = ?1 AND key = ?2",
                params![session_id, key.trim()],
                |r| {
                    Ok(Scratch {
                        key: r.get(0)?,
                        value: r.get(1)?,
                        updated_at: r.get(2)?,
                    })
                },
            )
            .optional()?;
        Ok(s)
    }

    pub fn list_scratch(&self, session_id: &str) -> Result<Vec<Scratch>> {
        let mut stmt = self.con.prepare(
            "SELECT key, value, updated_at FROM scratchpads WHERE session_id = ?1 ORDER BY key",
        )?;
        let it = stmt.query_map(params![session_id], |r| {
            Ok(Scratch {
                key: r.get(0)?,
                value: r.get(1)?,
                updated_at: r.get(2)?,
            })
        })?;
        let mut out = Vec::new();
        for r in it {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn delete_scratch(&self, session_id: &str, key: &str) -> Result<()> {
        self.con.execute(
            "DELETE FROM scratchpads WHERE session_id = ?1 AND key = ?2",
            params![session_id, key.trim()],
        )?;
        Ok(())
    }

    pub fn clear_scratch(&self, session_id: &str) -> Result<()> {
        self.con
            .execute("DELETE FROM scratchpads WHERE session_id = ?1", params![session_id])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn store(tag: &str) -> (ContextStore, PathBuf) {
        let p = std::env::temp_dir()
            .join(format!("ne-agent-ctx-{tag}-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&p);
        (ContextStore::open(&p).unwrap(), p)
    }

    #[test]
    fn plan_status_rejects_freeform_text() {
        // 关键：模型常把阶段说明塞进 status → 必须解析失败（调用方据此保留原状态）
        assert_eq!(PlanStatus::parse("活跃中"), None);
        assert_eq!(PlanStatus::parse("第一阶段完成"), None);
        assert_eq!(PlanStatus::parse("active"), Some(PlanStatus::Active));
        assert_eq!(PlanStatus::parse("COMPLETED"), Some(PlanStatus::Completed));
    }

    #[test]
    fn save_and_get_active_plan() {
        let (s, p) = store("plan");
        s.save_plan("S", "写完整卷", r#"[{"id":"1","title":"搞定"}]"#, PlanStatus::Active, 100)
            .unwrap();
        let got = s.get_plan("S").unwrap().unwrap();
        assert_eq!(got.goal, "写完整卷");
        assert_eq!(got.status, "active");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn completed_plan_not_returned_as_active() {
        let (s, p) = store("planfin");
        s.save_plan("S", "目标", "[]", PlanStatus::Active, 1).unwrap();
        s.advance_plan("S", None, Some(PlanStatus::Completed), 2).unwrap();
        assert!(s.get_plan("S").unwrap().is_none(), "完成的计划不再算活跃");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn advance_with_invalid_status_keeps_original() {
        let (s, p) = store("advbad");
        s.save_plan("S", "目标", "[]", PlanStatus::Active, 1).unwrap();
        // 模拟模型传了非法状态 → 调用方传 None → 保留 active
        s.advance_plan("S", Some(r#"[{"done":true}]"#), None, 2).unwrap();
        let got = s.get_plan("S").unwrap().unwrap();
        assert_eq!(got.status, "active", "非法状态不得写坏 plan");
        assert!(got.steps_json.contains("done"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn advance_on_missing_plan_returns_none() {
        let (s, p) = store("advmiss");
        assert!(s.advance_plan("NOPE", None, None, 1).unwrap().is_none());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn clear_plan() {
        let (s, p) = store("clr");
        s.save_plan("S", "目标", "[]", PlanStatus::Active, 1).unwrap();
        s.clear_plan("S").unwrap();
        assert!(s.get_plan("S").unwrap().is_none());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn scratch_save_get_list_delete() {
        let (s, p) = store("scratch");
        s.save_scratch("S", "假说", "DNS 坏了", 1).unwrap();
        s.save_scratch("S", "进度", "第3章", 2).unwrap();
        assert_eq!(s.get_scratch("S", "假说").unwrap().unwrap().value, "DNS 坏了");
        let list = s.list_scratch("S").unwrap();
        assert_eq!(list.len(), 2);
        // ORDER BY key 是 SQLite 默认（UTF-8 字节序）：假(E5…) < 进(E8…)
        assert_eq!(list[0].key, "假说");
        s.delete_scratch("S", "假说").unwrap();
        assert_eq!(s.list_scratch("S").unwrap().len(), 1);
        s.clear_scratch("S").unwrap();
        assert!(s.list_scratch("S").unwrap().is_empty());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn scratch_upsert_overwrites() {
        let (s, p) = store("upsert");
        s.save_scratch("S", "k", "v1", 1).unwrap();
        s.save_scratch("S", "k", "v2", 2).unwrap();
        assert_eq!(s.get_scratch("S", "k").unwrap().unwrap().value, "v2");
        assert_eq!(s.list_scratch("S").unwrap().len(), 1);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn scratch_rejects_empty_key_and_oversize() {
        let (s, p) = store("bad");
        assert!(s.save_scratch("S", "  ", "v", 1).is_err());
        assert!(s.save_scratch("S", "k", &"x".repeat(MAX_SCRATCH_VALUE_CHARS + 1), 1).is_err());
        assert!(s.save_scratch("S", &"k".repeat(MAX_SCRATCH_KEY_CHARS + 1), "v", 1).is_err());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn scratch_sessions_are_isolated() {
        let (s, p) = store("iso");
        s.save_scratch("S1", "k", "一", 1).unwrap();
        s.save_scratch("S2", "k", "二", 1).unwrap();
        assert_eq!(s.get_scratch("S1", "k").unwrap().unwrap().value, "一");
        assert_eq!(s.get_scratch("S2", "k").unwrap().unwrap().value, "二");
        s.clear_scratch("S1").unwrap();
        assert_eq!(s.list_scratch("S2").unwrap().len(), 1, "清 S1 不该动 S2");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn missing_scratch_returns_none() {
        let (s, p) = store("miss");
        assert!(s.get_scratch("S", "无").unwrap().is_none());
        let _ = std::fs::remove_file(&p);
    }
}
