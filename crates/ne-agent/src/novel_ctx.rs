//! 小说任务的库上下文 —— **补齐全仓库缺失的 `BudgetCtx` / `ChapterCtx` 真实现**。
//!
//! # 背景：这是一条断链
//!
//! `ne_engine::budget::build_injection` 与 `ne_engine::chapter::build_prompt` 早已写好，
//! 但它们依赖的 `BudgetCtx` / `ChapterCtx` trait **全仓库只有测试里的 mock，没有真实现**。
//! 也就是说：**半条生成链是断的**——能力在，没人接上。
//! 本模块就是那一半：把 vault 库根接到这两个 trait 上。
//!
//! # 数据来源（实地确认，2026-10-11）
//!
//! 真实库根 `/sdcard/.../资料库`：
//!   * `novel.db` 65 张表 —— `materials`(916 行) / `foreshadow` / `events` / `chapters`
//!   * `03_叙事规划/细纲/卷NN.md` —— 细纲
//!   * `04_正文/卷NN/第N章.md` —— 正文
//!   * `05_管线框架/prompts/*.md` —— prompt 模板（库里已无 02_资料库，卡片走 DB 回退）
//!
//! # 只读原则
//!
//! 本模块**只读**库（`SQLITE_OPEN_READ_ONLY`），不写任何库表。
//! 写正文由 `ne_engine::chapter::write_chapter` 负责，写库由既有管线负责。

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// 库上下文：只读绑定一个 vault 库根。
pub struct NovelCtx {
    root: PathBuf,
    con: Option<Connection>,
    /// 额外的 prompt 回退目录（vault 的 `prompts/`）
    prompt_fallback: Option<PathBuf>,
}

impl NovelCtx {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let db = root.join("novel.db");
        let con = if db.exists() {
            Some(
                Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .with_context(|| format!("只读打开 novel.db 失败：{}", db.display()))?,
            )
        } else {
            None
        };
        Ok(NovelCtx { root, con, prompt_fallback: None })
    }

    /// 指定 prompt 回退目录（库内模板缺失时用，通常是 vault 的 `prompts/`）。
    pub fn with_prompt_fallback(mut self, dir: impl AsRef<Path>) -> Self {
        self.prompt_fallback = Some(dir.as_ref().to_path_buf());
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn db(&self) -> Result<&Connection> {
        self.con
            .as_ref()
            .context("novel.db 不存在（库根未初始化）")
    }

    /// 只读查询，失败返回空（库不完整时优雅降级，不炸整条生成链）。
    fn query_json<F>(&self, sql: &str, params: &[&dyn rusqlite::ToSql], map: F) -> Vec<Value>
    where
        F: Fn(&rusqlite::Row) -> rusqlite::Result<Value>,
    {
        let Ok(con) = self.db() else { return Vec::new() };
        let Ok(mut st) = con.prepare(sql) else { return Vec::new() };
        st.query_map(params, |r| map(r))
            .map(|it| it.filter_map(|x| x.ok()).collect())
            .unwrap_or_default()
    }

    /// 读库根下任意相对路径文本（缺失返回 None，不报错）。
    fn read_rel(&self, rel: &str) -> Option<String> {
        let p = self.root.join(rel);
        std::fs::read_to_string(p).ok()
    }

    /// 读 prompt 模板：优先库根 `05_管线框架/prompts/`，其次 vault 的 `prompts/`。
    pub fn prompt_template(&self, name: &str, fallback_dir: Option<&Path>) -> Option<String> {
        for cand in [
            self.root.join("05_管线框架/prompts").join(name),
            self.root.join("prompts").join(name),
        ] {
            if let Ok(t) = std::fs::read_to_string(&cand) {
                if !t.trim().is_empty() {
                    return Some(t);
                }
            }
        }
        if let Some(d) = fallback_dir {
            if let Ok(t) = std::fs::read_to_string(d.join(name)) {
                if !t.trim().is_empty() {
                    return Some(t);
                }
            }
        }
        None
    }

    /// 卷号 → 细纲文件路径。
    fn outline_path(&self, volume: &str) -> PathBuf {
        self.root.join("03_叙事规划/细纲").join(format!("{volume}.md"))
    }

    /// 上一章正文末尾（跨章/跨卷回溯）。`tail_chars` 按字符计。
    pub fn prev_tail_text(&self, volume: &str, chapter: u32, tail_chars: usize) -> String {
        // 本章之前的最近一章：同卷 chapter-1，若是第 1 章则取上一卷最后一章
        let mut candidates: Vec<PathBuf> = Vec::new();
        if chapter > 1 {
            candidates.push(self.root.join("04_正文").join(volume).join(format!("第{}章.md", chapter - 1)));
        } else {
            // 上一卷（卷号 -1）
            if let Some(prev_vol) = prev_volume(volume) {
                let dir = self.root.join("04_正文").join(&prev_vol);
                if let Ok(rd) = std::fs::read_dir(&dir) {
                    let mut files: Vec<PathBuf> = rd
                        .flatten()
                        .map(|e| e.path())
                        .filter(|p| p.extension().map(|e| e == "md").unwrap_or(false))
                        .collect();
                    files.sort();
                    if let Some(last) = files.last() {
                        candidates.push(last.clone());
                    }
                }
            }
        }
        for p in candidates {
            if let Ok(text) = std::fs::read_to_string(&p) {
                let chars: Vec<char> = text.trim().chars().collect();
                let start = chars.len().saturating_sub(tail_chars);
                return chars[start..].iter().collect();
            }
        }
        String::new()
    }
}

fn prev_volume(volume: &str) -> Option<String> {
    let n: u32 = volume.trim_start_matches("卷").parse().ok()?;
    if n <= 1 {
        None
    } else {
        Some(format!("卷{:02}", n - 1))
    }
}

// ───────────────────────────── BudgetCtx 实现

impl ne_engine::budget::BudgetCtx for NovelCtx {
    fn outline_text(&self, volume: &str) -> Option<String> {
        std::fs::read_to_string(self.outline_path(volume)).ok()
    }

    fn material(&self, code: &str) -> Option<Value> {
        // 先按 code，再按 new_code/old_code 兜底（细纲可能用旧编号）
        self.query_json(
            "SELECT code,name,category,summary,content,file_path FROM materials
             WHERE code=?1 OR new_code=?1 OR old_code=?1 LIMIT 1",
            &[&code],
            |r| {
                Ok(json!({
                    "code": r.get::<_, String>(0)?,
                    "name": r.get::<_, String>(1)?,
                    "category": r.get::<_, String>(2)?,
                    "summary": r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    "content": r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                    "file_path": r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                }))
            },
        )
        .into_iter()
        .next()
    }

    fn read_lib_file(&self, rel_path: &str) -> Option<String> {
        // 卡片磁盘副本可能已删（S10 现状）→ 尝试 DB content 回退
        if let Some(t) = self.read_rel(&format!("02_资料库/{rel_path}")) {
            return Some(t);
        }
        // rel_path 形如 `丹药/MTC-001.md` → 取文件名去扩展名当 code 查 DB
        let stem = Path::new(rel_path).file_stem()?.to_string_lossy().to_string();
        self.query_json(
            "SELECT content FROM materials WHERE code=?1 OR file_path LIKE ?2 LIMIT 1",
            &[&stem, &format!("%{rel_path}")],
            |r| Ok(json!(r.get::<_, Option<String>>(0)?.unwrap_or_default())),
        )
        .into_iter()
        .next()
        .and_then(|v| v.as_str().map(|s| s.to_string()))
    }

    fn role_rows(&self) -> Vec<Value> {
        self.query_json(
            "SELECT code,name FROM materials WHERE category='角色' ORDER BY code",
            &[],
            |r| Ok(json!({"code": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?})),
        )
    }

    fn char_card_rows(&self, codes: &[String]) -> Vec<Value> {
        if codes.is_empty() {
            return Vec::new();
        }
        let list = codes.iter().map(|c| format!("'{c}'")).collect::<Vec<_>>().join(",");
        // personality = 结构化性格 JSON（本库当前全空，保留以兼容未来灌值）
        // content     = 角色卡正文（用于「语言风格」兜底提取：性格/说话/口吻…）
        let sql = format!(
            "SELECT code,name,coalesce(personality,''),coalesce(content,'') FROM materials
             WHERE code IN ({list})"
        );
        self.query_json(&sql, &[], |r| {
            Ok(json!({
                "code": r.get::<_, String>(0)?,
                "name": r.get::<_, String>(1)?,
                "personality": r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                "content": r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                "summary": "",
            }))
        })
    }

    fn foreshadow_rows(&self, volume: &str, chapter: u32) -> (Vec<Value>, Vec<Value>) {
        // 到期：deadline 卷<=当前卷 且（同卷 deadline 章<=当前章 或 跨卷），且未收
        let due = self.query_json(
            "SELECT id, content FROM foreshadow
             WHERE status NOT IN ('resolved','closed','done')
               AND deadline_volume IS NOT NULL
               AND deadline_volume <= ?1
             ORDER BY deadline_volume, deadline_chapter LIMIT 20",
            &[&volume],
            |r| Ok(json!({"id": r.get::<_, String>(0)?, "content": r.get::<_, Option<String>>(1)?.unwrap_or_default()})),
        );
        let _ = chapter;
        // 可埋伏笔：库内 seeded/pending 且未定稿
        let can_plant = self.query_json(
            "SELECT id, content FROM foreshadow
             WHERE status IN ('seeded','pending','planted')
             ORDER BY id LIMIT 10",
            &[],
            |r| Ok(json!({"id": r.get::<_, String>(0)?, "content": r.get::<_, Option<String>>(1)?.unwrap_or_default()})),
        );
        (due, can_plant)
    }

    fn event_rows(&self, ids: &[String]) -> Vec<Value> {
        if ids.is_empty() {
            return Vec::new();
        }
        let list = ids.iter().map(|c| format!("'{c}'")).collect::<Vec<_>>().join(",");
        let sql = format!("SELECT id,name,status FROM events WHERE id IN ({list})");
        self.query_json(&sql, &[], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "name": r.get::<_, String>(1)?,
                "status": r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            }))
        })
    }

    fn prev_tail(&self, volume: &str, chapter: u32, tail_chars: usize) -> String {
        self.prev_tail_text(volume, chapter, tail_chars)
    }
}

// ───────────────────────────── ChapterCtx 实现

impl ne_engine::chapter::ChapterCtx for NovelCtx {
    fn injection(&self, volume: &str, chapter: u32) -> Result<Value> {
        // 委托 ne_engine::budget::build_injection（同一套 BudgetCtx 实现，口径一致）
        ne_engine::budget::build_injection(self, volume, chapter)
    }

    fn prompt_file(&self, name: &str) -> Option<String> {
        self.prompt_template(name, self.prompt_fallback.as_deref())
    }

    fn lib_root(&self) -> Option<std::path::PathBuf> {
        Some(self.root.clone())
    }

    fn kb_rules(&self, _scene: &str, _limit: usize) -> Vec<Value> {
        // 真实库当前**无 kb_rules 表**（65 表里没有）→ 如实返回空，
        // build_knowledge_injection 会填「无匹配规则，按 §1 铁律写」。不杜撰。
        let tables: i64 = self
            .db()
            .ok()
            .and_then(|c| c.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='kb_rules'", [], |r| r.get(0)).ok())
            .unwrap_or(0);
        if tables == 0 {
            return Vec::new();
        }
        self.query_json(
            "SELECT code,name,rule_text,example FROM kb_rules LIMIT 50",
            &[],
            |r| {
                Ok(json!({
                    "code": r.get::<_, String>(0)?,
                    "name": r.get::<_, String>(1)?,
                    "rule_text": r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    "example": r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                }))
            },
        )
    }

    fn material_content(&self, code: &str) -> Option<String> {
        self.query_json(
            "SELECT content FROM materials WHERE code=?1 OR new_code=?1 OR old_code=?1 LIMIT 1",
            &[&code],
            |r| Ok(json!(r.get::<_, Option<String>>(0)?.unwrap_or_default())),
        )
        .into_iter()
        .next()
        .and_then(|v| v.as_str().map(|s| s.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ne_engine::budget::BudgetCtx;
    use ne_engine::chapter::ChapterCtx;

    /// 造一个最小假库根：细纲 + 上一章 + novel.db(materials/foreshadow/events)
    fn fake_lib(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ne-agent-novel-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("03_叙事规划/细纲")).unwrap();
        std::fs::create_dir_all(d.join("04_正文/卷01")).unwrap();
        std::fs::create_dir_all(d.join("05_管线框架/prompts")).unwrap();
        std::fs::write(
            d.join("03_叙事规划/细纲/卷01.md"),
            "### 第1章 起\n\n**目标**：测试\n\n在场：沈砚\n引典：LOC-99-001\n",
        )
        .unwrap();
        std::fs::write(d.join("04_正文/卷01/第1章.md"), "上一章正文的末尾内容。").unwrap();
        std::fs::write(d.join("05_管线框架/prompts/l5_正文.md"), "【模板】正文写作指令").unwrap();
        let con = Connection::open(d.join("novel.db")).unwrap();
        con.execute_batch(
            "CREATE TABLE materials(id INTEGER PRIMARY KEY, code TEXT, name TEXT, category TEXT,
                sub_category TEXT, file_path TEXT, summary TEXT, personality TEXT, content TEXT,
                old_code TEXT, new_code TEXT);
             CREATE TABLE foreshadow(id TEXT, content TEXT, planted_volume TEXT, planted_chapter INTEGER,
                deadline_volume TEXT, deadline_chapter INTEGER, status TEXT);
             CREATE TABLE events(id TEXT, name TEXT, status TEXT);
             INSERT INTO materials(code,name,category,content) VALUES('CHAR-99-001','沈砚','角色','沈砚，青牛镇，境界练气三层');
             INSERT INTO materials(code,name,category,content) VALUES('LOC-99-001','青牛镇','地点','小镇');
             INSERT INTO foreshadow(id,content,deadline_volume,status) VALUES('FS-1','主角身世',  '卷01','seeded');
             INSERT INTO events(id,name,status) VALUES('EVT-99-005','残响初鸣','pending');",
        )
        .unwrap();
        d
    }

    #[test]
    fn opens_and_reads_material() {
        let d = fake_lib("mat");
        let c = NovelCtx::open(&d).unwrap();
        let m = c.material("CHAR-99-001").unwrap();
        assert_eq!(m["name"], "沈砚");
        assert!(m["content"].as_str().unwrap().contains("青牛镇"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn material_code_fallback_via_new_code() {
        let d = fake_lib("fallback");
        // 造一条有 old/new code 的卡
        let con = Connection::open(d.join("novel.db")).unwrap();
        con.execute("INSERT INTO materials(code,name,category,old_code,new_code) VALUES('X','旧卡','道具','OLD-1','NEW-1')", []).unwrap();
        drop(con);
        let c = NovelCtx::open(&d).unwrap();
        assert!(c.material("OLD-1").is_some(), "应按 old_code 兜底命中");
        assert!(c.material("NEW-1").is_some(), "应按 new_code 兜底命中");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn builds_injection_without_panic() {
        let d = fake_lib("inject");
        let c = NovelCtx::open(&d).unwrap();
        let inj = c.injection("卷01", 1).unwrap();
        assert!(inj.get("chapter").is_some());
        assert!(inj["chars"].as_array().is_some());
        // materials 应命中 LOC-99-001
        let mats = inj["materials"].as_array().cloned().unwrap_or_default();
        assert!(!mats.is_empty(), "资料卡应被选中：{inj}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn builds_full_prompt() {
        let d = fake_lib("prompt");
        let c = NovelCtx::open(&d).unwrap();
        let p = ne_engine::chapter::build_prompt(&c, 1, "卷01").unwrap();
        assert!(p.contains("【模板】正文写作指令"), "应含库内模板");
        assert!(p.contains("第1章"), "应含章号");
        assert!(p.contains("字数"), "应含字数约束");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn prev_tail_reads_previous_chapter() {
        let d = fake_lib("tail");
        let c = NovelCtx::open(&d).unwrap();
        let t = c.prev_tail("卷01", 2, 500);
        assert!(t.contains("上一章正文"), "应读到第1章末尾：{t}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_db_degrades_gracefully() {
        // 无 novel.db 的库根不得 panic；injection 仍可构造（卡片为空）
        let d = std::env::temp_dir().join(format!("ne-agent-novel-nodb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("03_叙事规划/细纲")).unwrap();
        std::fs::write(d.join("03_叙事规划/细纲/卷01.md"), "### 第1章 起\n").unwrap();
        let c = NovelCtx::open(&d).unwrap();
        assert!(c.material("X").is_none());
        assert!(c.injection("卷01", 1).is_ok(), "缺库时也应能构造注入包");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn kb_rules_empty_when_table_absent() {
        let d = fake_lib("kb");
        let c = NovelCtx::open(&d).unwrap();
        assert!(c.kb_rules("战斗", 9).is_empty(), "无 kb_rules 表应如实返回空，不杜撰");
        let _ = std::fs::remove_dir_all(&d);
    }
}
