//! ne-db：SQLite 状态回填。
//!
//! # ⚠️ 双 schema（2026-10-10 修正）
//!
//! 历史上本 crate 自带一份**简化 schema**（`foreshadow(fs_id, plant_chapter)` 等），
//! 但**真实资料库另有一套**（`foreshadow(id, planted_volume, planted_chapter,
//! deadline_volume, deadline_chapter, posterior, lik_ratio)` …）。
//! 实测二者对不上：
//!
//! ```text
//! SELECT fs_id FROM foreshadow  →  no such column: fs_id
//! INSERT INTO chapters(chapter) →  no such column: chapter
//! ```
//!
//! **后果**：本 crate 的所有写入路径在真库上直接失败；查询也只能绕开。
//!
//! # 现行策略：运行时探测列名，双向适配
//!
//! - 启动时读 `PRAGMA table_info`，判断该表是「真库版」还是「自建简化版」
//! - 写路径按探测结果选列名（真库优先）
//! - `SCHEMA_SELFHOSTED` 仅在**表不存在**时用于新建（不再覆盖已有真库表）
//!
//! 6-state = 伏笔 / 角色 / 事件 / 物品 / 时间线 / 进度。
//! 一章生成后 [`tick_chapter`] 以显式事务一次推进（失败整体回滚）。

use ne_core::{InjectionPacket, MaterialRef, UsageStat};
use rusqlite::{params, Connection};

/// **自建简化版** schema（仅在表不存在时创建，不回改已有真库表）。
///
/// ⚠️ 真库另有更完整的 schema（见模块文档）。生产环境应**优先适配真库**，
/// 本常量只在「空目录新建库」时用。
///
/// 保留原名 `SCHEMA` 以避免破坏既有调用方。
pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS chapters (
  volume TEXT, chapter INTEGER, title TEXT, status TEXT,
  words INTEGER, created_at TEXT, PRIMARY KEY(volume, chapter)
);
CREATE TABLE IF NOT EXISTS char_state (
  char_code TEXT, char_name TEXT, volume TEXT, chapter INTEGER,
  realm TEXT, location TEXT, status TEXT,
  PRIMARY KEY(char_code, volume, chapter)
);
CREATE TABLE IF NOT EXISTS foreshadow (
  fs_id TEXT, content TEXT, plant_chapter INTEGER, deadline_chapter INTEGER,
  status TEXT, PRIMARY KEY(fs_id)
);
CREATE TABLE IF NOT EXISTS events (
  event_id TEXT, name TEXT, status TEXT, last_chapter INTEGER
);
CREATE TABLE IF NOT EXISTS timeline (
  volume TEXT, chapter INTEGER, moment TEXT, civ_state TEXT
);
CREATE TABLE IF NOT EXISTS items (
  item_code TEXT, name TEXT, holder TEXT, status TEXT
);
CREATE TABLE IF NOT EXISTS card_usage (
  code TEXT PRIMARY KEY, category TEXT, name TEXT,
  ref_count INTEGER, last_chapter INTEGER
);
"#;

/// 连接（或新建）novel.db。
///
/// 注意：**只在表不存在时**建自建 schema。
/// 若库已存在（真库），`CREATE TABLE IF NOT EXISTS` 不会改动其结构，
/// 后续写路径会自动探测列名适配。
pub fn connect(db_path: &str) -> Result<Connection, anyhow::Error> {
    let con = Connection::open(db_path)?;
    con.execute_batch(SCHEMA)?;
    Ok(con)
}

// ────────────────────────────────────────────────────────────
// schema 探测：真库版 vs 自建版
// ────────────────────────────────────────────────────────────

/// 取某表的所有列名（表不存在时返回空）。
pub fn columns_of(con: &Connection, table: &str) -> Vec<String> {
    let mut st = match con.prepare(&format!("PRAGMA table_info(\"{table}\")")) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    st.query_map([], |r| r.get::<_, String>(1))
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
}

/// 本库的 schema 方言。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// 真实资料库（`foreshadow.id` / `chapters.chapter_no` / `char_state` 带 id 主键…）
    Real,
    /// 本 crate 早期自建的简化版（`foreshadow.fs_id` / `chapters.chapter`…）
    SelfHosted,
    /// 表都不在（空库）
    Unknown,
}

impl Dialect {
    /// 依据 `foreshadow` 的主键列名判定。
    ///
    /// 真库：`id` + `planted_chapter`；自建：`fs_id` + `plant_chapter`。
    pub fn detect(con: &Connection) -> Dialect {
        let cols = columns_of(con, "foreshadow");
        if cols.is_empty() {
            // foreshadow 不在，看 chapters
            let cc = columns_of(con, "chapters");
            if cc.is_empty() {
                return Dialect::Unknown;
            }
            return if cc.iter().any(|c| c == "chapter_no") {
                Dialect::Real
            } else {
                Dialect::SelfHosted
            };
        }
        if cols.iter().any(|c| c == "fs_id") {
            Dialect::SelfHosted
        } else if cols.iter().any(|c| c == "planted_chapter") {
            Dialect::Real
        } else {
            Dialect::Unknown
        }
    }
}

/// 章级回填：把一章的 chars / events / 伏笔 / 用量 写进 6-state。
/// 2026-10-08 口径：显式事务整体提交（Requirement 9：状态推进 = 事务更新 SQLite）。
pub fn tick_chapter(
    con: &Connection,
    volume: &str,
    chapter: u32,
    title: &str,
    words: usize,
    packet: &InjectionPacket,
) -> Result<(), anyhow::Error> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let tx = con.unchecked_transaction()?;
    let dialect = Dialect::detect(con);

    // chapters 主表（进度 state）
    // 真库：chapter_no + content_path/outline/…；自建：chapter
    match dialect {
        Dialect::Real => {
            tx.execute(
                "INSERT INTO chapters(volume, chapter_no, title, status, words, updated_at)
                 VALUES(?1, ?2, ?3, 'generated', ?4, datetime('now'))
                 ON CONFLICT(volume, chapter_no) DO UPDATE SET
                   title=excluded.title, status=excluded.status,
                   words=excluded.words, updated_at=datetime('now')",
                params![volume, chapter as i64, title, words as i64],
            )?;
        }
        _ => {
            tx.execute(
                "INSERT OR REPLACE INTO chapters(volume, chapter, title, status, words, created_at)
                 VALUES(?1, ?2, ?3, 'generated', ?4, ?5)",
                params![volume, chapter as i64, title, words as i64, ts],
            )?;
        }
    }

    // char_state：出场角色（角色 state）
    for c in &packet.chars {
        tx.execute(
            "INSERT OR REPLACE INTO char_state(char_code, char_name, volume, chapter, realm, location, status)
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                c.code,
                c.name,
                volume,
                chapter as i64,
                c.realm,
                c.location,
                c.status
            ],
        )?;
    }

    // events：细纲引用到的 EVT-*（事件 state）
    // 真库列：id / name / status / trigger_chapter / resolve_chapter / notes
    // 自建列：event_id / name / status / last_chapter
    for e in &packet.events {
        match dialect {
            Dialect::Real => {
                tx.execute(
                    "INSERT INTO events(id, name, status, trigger_chapter, notes)
                     VALUES(?1,?2,?3,?4,'ne-db tick')
                     ON CONFLICT(id) DO UPDATE SET name=excluded.name, status=excluded.status",
                    params![e.id, e.name, e.status, chapter as i64],
                )?;
            }
            _ => {
                tx.execute(
                    "INSERT OR REPLACE INTO events(event_id, name, status, last_chapter)
                     VALUES(?1,?2,?3,?4)",
                    params![e.id, e.name, e.status, chapter as i64],
                )?;
            }
        }
    }

    // timeline：本章时刻点（时间线 state，civ_state 占位，M3 填公式）
    let civ = packet
        .world
        .get("note")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    // 真库列：id/volume/chapter/world_time/event_summary（UNIQUE(volume,chapter)）
    // 自建列：volume/chapter/moment/civ_state
    match dialect {
        Dialect::Real => {
            tx.execute(
                "INSERT INTO timeline(volume, chapter, world_time, event_summary)
                 VALUES(?1,?2,?3,?4)
                 ON CONFLICT(volume, chapter) DO UPDATE SET event_summary=excluded.event_summary",
                params![volume, chapter as i64, "chapter_tick", civ],
            )?;
        }
        _ => {
            tx.execute(
                "INSERT INTO timeline(volume, chapter, moment, civ_state) VALUES(?1,?2,?3,?4)",
                params![volume, chapter as i64, "chapter_tick", civ],
            )?;
        }
    }

    // foreshadow：due 的伏笔落库
    // 真库列：id / content(NOT NULL) / planted_volume / planted_chapter /
    //         deadline_volume / deadline_chapter / status('pending'|'resolved'|'hollow')
    // 自建列：fs_id / content / plant_chapter / deadline_chapter / status('open')
    for fs in &packet.foreshadow_due {
        match dialect {
            Dialect::Real => {
                tx.execute(
                    "INSERT OR IGNORE INTO foreshadow(id, content, planted_volume, planted_chapter, status)
                     VALUES(?1, ?2, ?3, ?4, 'pending')",
                    params![fs, "(待补：由 ne-db tick 自动登记，请人工补 content)", volume, chapter as i64],
                )?;
            }
            _ => {
                tx.execute(
                    "INSERT OR IGNORE INTO foreshadow(fs_id, content, plant_chapter, deadline_chapter, status)
                     VALUES(?1, '', ?2, 0, 'open')",
                    params![fs, chapter as i64],
                )?;
            }
        }
    }

    // card_usage：出场角色 + 资料卡都算被引用（确定性选卡第 1 层的数据源）
    for c in &packet.chars {
        upsert_usage(&tx, &c.code, "角色", &c.name, chapter)?;
    }
    for m in &packet.materials {
        let cat = m.code.split('-').next().unwrap_or("OTHER").to_string();
        upsert_usage(&tx, &m.code, &cat, &m.name, chapter)?;
    }

    Ok(tx.commit()?)
}

fn upsert_usage(
    tx: &rusqlite::Transaction<'_>,
    code: &str,
    category: &str,
    name: &str,
    chapter: u32,
) -> Result<(), anyhow::Error> {
    tx.execute(
        "INSERT INTO card_usage(code, category, name, ref_count, last_chapter)
         VALUES(?1, ?2, ?3, 1, ?4)
         ON CONFLICT(code) DO UPDATE SET ref_count = ref_count + 1, last_chapter = excluded.last_chapter",
        params![code, category, name, chapter as i64],
    )?;
    Ok(())
}

/// 确定性选卡数据源：card_usage + foreshadow(open) → UsageStat 列表。
pub fn usage_stats(con: &Connection) -> Result<Vec<UsageStat>, anyhow::Error> {
    let mut st = con.prepare("SELECT code, ref_count, last_chapter FROM card_usage")?;
    let mut out: Vec<UsageStat> = st
        .query_map([], |r| {
            Ok(UsageStat {
                code: r.get(0)?,
                ref_count: r.get(1)?,
                last_chapter: r.get::<_, i64>(2)?.max(0) as u32,
                foreshadow_due: false,
            })
        })?
        .flatten()
        .collect();
    // 方言适配：真库用 id/planted_chapter/status='pending'；自建用 fs_id/plant_chapter/'open'
    let dialect = Dialect::detect(con);
    let open_sql = match dialect {
        Dialect::Real => {
            "SELECT id, COALESCE(planted_chapter, 0) FROM foreshadow WHERE status = 'pending'"
        }
        _ => "SELECT fs_id, plant_chapter FROM foreshadow WHERE status = 'open'",
    };
    let open: Vec<(String, i64)> = {
        let mut st = con.prepare(open_sql)?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        rows.flatten().collect()
    };
    // open 伏笔卡若尚无用量行（伏笔先行落库、卡未被引用），补一条 foreshadow_due 条目
    for (code, plant) in &open {
        if !out.iter().any(|u| u.code == *code) {
            out.push(UsageStat {
                code: code.clone(),
                ref_count: 0,
                last_chapter: (*plant).max(0) as u32,
                foreshadow_due: true,
            });
        }
    }
    for u in out.iter_mut() {
        if open.iter().any(|(c, _)| c == &u.code) {
            u.foreshadow_due = true;
        }
    }
    Ok(out)
}

/// 灌卡（对应 db/init.py + fill.py）：把 02_资料库 的 materials 灌进 materials 表。
/// 含 3 行摘要字段（summary）；旧库无该列时自动补列。
pub fn load_materials(con: &Connection, materials: &[MaterialRef]) -> Result<usize, anyhow::Error> {
    con.execute_batch(
        "CREATE TABLE IF NOT EXISTS materials (
           code TEXT PRIMARY KEY, name TEXT, content TEXT, is_full INTEGER, summary TEXT DEFAULT ''
         );",
    )?;
    // 旧库迁移：materials 表存在但缺 summary 列 → 补列（已存在则忽略报错）
    let _ = con.execute_batch("ALTER TABLE materials ADD COLUMN summary TEXT DEFAULT '';");
    let mut n = 0;
    for m in materials {
        let r = con.execute(
            "INSERT OR REPLACE INTO materials(code, name, content, is_full, summary) VALUES(?1,?2,?3,?4,?5)",
            params![m.code, m.name, m.content, m.is_full as i64, m.summary],
        );
        if let Ok(affected) = r {
            n += affected;
        }
    }
    Ok(n)
}

/// 各表行数快照（M3 验收：从 0 行 → >0）。
pub fn table_counts(con: &Connection) -> Result<Vec<(String, i64)>, anyhow::Error> {
    let tables = [
        "chapters", "char_state", "foreshadow", "events", "timeline", "items", "materials",
        "card_usage",
    ];
    let mut out = Vec::new();
    for t in tables {
        let n: i64 = con
            .query_row(&format!("SELECT COUNT(*) FROM {t}"), params![], |r| r.get(0))
            .unwrap_or(0);
        out.push((t.to_string(), n));
    }
    Ok(out)
}


/// 从 legacy novel.db（外部工具建的丰富库）回填 ne-db 的 card_usage。
/// 数据源：materials(code,name,category,refs/ref_by JSON) + char_state(char_code,char_name,chapter)。
/// 幂等：先清空再全量重建（ON CONFLICT）。返回 (materials 行数, char 行数)。
pub fn backfill_card_usage_from_legacy(con: &Connection) -> Result<(usize, usize), anyhow::Error> {
    con.execute_batch(SCHEMA)?;
    let _ = con.execute_batch("DELETE FROM card_usage;");
    // materials：ref_count = refs + ref_by 的 JSON 数组长度（被引+引用）
    let n_mat = con
        .execute(
            "INSERT INTO card_usage(code, category, name, ref_count, last_chapter)              SELECT code, category, name,                     COALESCE(json_array_length(refs),0) + COALESCE(json_array_length(ref_by),0), 0              FROM materials              WHERE code IS NOT NULL AND code <> ''              ON CONFLICT(code) DO UPDATE SET ref_count = excluded.ref_count",
            [],
        )
        .unwrap_or(0);
    // char_state：ref_count = 出场记录数，last_chapter = 最大章
    let n_char = con
        .execute(
            "INSERT INTO card_usage(code, category, name, ref_count, last_chapter)              SELECT char_code, '角色', char_name, COUNT(*), MAX(chapter)              FROM char_state GROUP BY char_code              ON CONFLICT(code) DO UPDATE SET ref_count = excluded.ref_count, last_chapter = excluded.last_chapter",
            [],
        )
        .unwrap_or(0);
    Ok((n_mat, n_char))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── 2026-10-10 F1 修复的回归测试 ──────────────────────────

    /// 真库 schema 的模拟 DDL（列名取自实测 PRAGMA table_info）。
    const REAL_DDL: &str = "
        CREATE TABLE chapters(id INTEGER PRIMARY KEY AUTOINCREMENT, volume TEXT NOT NULL,
          chapter_no INTEGER NOT NULL, title TEXT, outline TEXT, scene_type TEXT,
          status TEXT DEFAULT 'pending', words INTEGER DEFAULT 0, content_path TEXT,
          quality_result TEXT, techniques_used TEXT, created_at TEXT, updated_at TEXT,
          UNIQUE(volume, chapter_no));
        CREATE TABLE char_state(id INTEGER PRIMARY KEY AUTOINCREMENT, char_code TEXT NOT NULL,
          char_name TEXT NOT NULL, volume TEXT, chapter INTEGER, realm TEXT, location TEXT,
          status TEXT, notes TEXT, UNIQUE(char_code, volume, chapter));
        CREATE TABLE events(id TEXT PRIMARY KEY, name TEXT NOT NULL,
          status TEXT DEFAULT 'pending', trigger_chapter INTEGER, resolve_chapter INTEGER,
          dependencies TEXT, notes TEXT);
        CREATE TABLE timeline(id INTEGER PRIMARY KEY AUTOINCREMENT, volume TEXT, chapter INTEGER,
          world_time TEXT, event_summary TEXT, UNIQUE(volume, chapter));
        CREATE TABLE foreshadow(id TEXT PRIMARY KEY, content TEXT NOT NULL, planted_volume TEXT,
          planted_chapter INTEGER, deadline_volume TEXT, deadline_chapter INTEGER,
          status TEXT DEFAULT 'pending', resolved_chapter INTEGER, evidence TEXT,
          posterior REAL DEFAULT 0.5, lik_ratio REAL DEFAULT 2.0);
        CREATE TABLE card_usage(code TEXT PRIMARY KEY, category TEXT, name TEXT,
          ref_count INTEGER, last_chapter INTEGER);
    ";

    #[test]
    fn detect_selfhosted() {
        let c = connect(":memory:").unwrap();
        assert_eq!(Dialect::detect(&c), Dialect::SelfHosted);
    }

    #[test]
    fn detect_real() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(REAL_DDL).unwrap();
        assert_eq!(Dialect::detect(&c), Dialect::Real);
    }

    #[test]
    fn detect_unknown_on_empty() {
        let c = Connection::open_in_memory().unwrap();
        assert_eq!(Dialect::detect(&c), Dialect::Unknown);
    }

    #[test]
    fn columns_of_works() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE t(a TEXT, b INTEGER);").unwrap();
        assert_eq!(columns_of(&c, "t"), vec!["a".to_string(), "b".to_string()]);
        assert!(columns_of(&c, "nope").is_empty());
    }

    /// 核心回归：真库 schema 下 tick 不再报 "no column named"。
    #[test]
    fn tick_on_real_schema_succeeds() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(REAL_DDL).unwrap();
        assert_eq!(Dialect::detect(&c), Dialect::Real);
        let pkt = ne_core::InjectionPacket::default();
        tick_chapter(&c, "卷01", 1, "测试章", 2000, &pkt)
            .expect("F1：真库 schema 下 tick 必须成功");
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM chapters WHERE chapter_no=1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        // 幂等：再 tick 同章不应重复
        tick_chapter(&c, "卷01", 1, "测试章", 2100, &pkt).unwrap();
        let n2: i64 = c
            .query_row("SELECT COUNT(*) FROM chapters WHERE chapter_no=1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n2, 1, "同章重复 tick 应 UPSERT 而非插入");
    }

    #[test]
    fn tick_on_selfhosted_still_works() {
        let c = connect(":memory:").unwrap();
        assert_eq!(Dialect::detect(&c), Dialect::SelfHosted);
        let pkt = ne_core::InjectionPacket::default();
        tick_chapter(&c, "卷01", 1, "测试章", 2000, &pkt)
            .expect("自建 schema 下 tick 仍须成功（向后兼容）");
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM chapters WHERE chapter=1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn usage_stats_reads_real_foreshadow() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(REAL_DDL).unwrap();
        c.execute("INSERT INTO card_usage(code,category,name,ref_count,last_chapter) VALUES('CHAR-1','角色','甲',3,5)", []).unwrap();
        c.execute("INSERT INTO foreshadow(id,content,planted_chapter,status) VALUES('F-1','x',2,'pending')", []).unwrap();
        let st = usage_stats(&c).unwrap();
        assert!(st.iter().any(|u| u.code == "F-1" && u.foreshadow_due), "真库 pending 伏笔应被识别：{st:?}");
        assert!(st.iter().any(|u| u.code == "CHAR-1" && u.ref_count == 3));
    }

    #[test]
    fn usage_stats_reads_selfhosted_foreshadow() {
        let c = connect(":memory:").unwrap();
        c.execute("INSERT INTO card_usage(code,category,name,ref_count,last_chapter) VALUES('CHAR-9','角色','乙',2,4)", []).unwrap();
        c.execute("INSERT INTO foreshadow(fs_id,content,plant_chapter,deadline_chapter,status) VALUES('FS-9','y',1,0,'open')", []).unwrap();
        let st = usage_stats(&c).unwrap();
        assert!(st.iter().any(|u| u.code == "FS-9" && u.foreshadow_due), "{st:?}");
    }

    #[test]
    fn tick_chapter_fills_tables() {
        let con = connect(":memory:").unwrap();
        let mut pkt = InjectionPacket::default();
        let mut ch = ne_core::ChapterOutline::default();
        ch.number = 5;
        ch.title = "气阻".into();
        pkt.chapter = ch;
        pkt.chars.push(ne_core::CharacterSnapshot {
            name: "陈寂".into(),
            code: "CHAR-99-001".into(),
            realm: "炼气".into(),
            location: "青牛镇".into(),
            status: "在场".into(),
        });
        pkt.events.push(ne_core::EventRef {
            id: "EVT-99-005".into(),
            name: "穿越事件".into(),
            status: "pending".into(),
        });
        pkt.foreshadow_due.push("FOP-99-001".into());
        pkt.materials.push(ne_core::MaterialRef {
            code: "REL-99-004".into(),
            name: "影阁内门".into(),
            content: String::new(),
            summary: String::new(),
            is_full: true,
        });
        tick_chapter(&con, "卷01", 5, "气阻", 3000, &pkt).unwrap();

        let counts = table_counts(&con).unwrap();
        let get = |t: &str| -> i64 { counts.iter().find(|(n, _) | n == t).map(|(_, v)| *v).unwrap() };
        assert_eq!(get("chapters"), 1);
        assert_eq!(get("char_state"), 1);
        assert_eq!(get("events"), 1);
        assert_eq!(get("timeline"), 1);
        assert_eq!(get("foreshadow"), 1);
        assert_eq!(get("card_usage"), 2);

        // 用量统计：被引次数 + 伏笔待收
        let stats = usage_stats(&con).unwrap();
        let rel = stats.iter().find(|s| s.code == "REL-99-004").unwrap();
        assert_eq!(rel.ref_count, 1);
        assert_eq!(rel.last_chapter, 5);
        assert!(!rel.foreshadow_due);
        let fop = stats.iter().find(|s| s.code == "FOP-99-001").unwrap();
        assert!(fop.foreshadow_due);
    }

    #[test]
    fn load_materials_counts() {
        let con = connect(":memory:").unwrap();
        let mats = vec![MaterialRef {
            code: "EVT-99-005".into(),
            name: "陈寂穿越事件".into(),
            content: "正文……".into(),
            summary: String::new(),
            is_full: true,
        }];
        let n = load_materials(&con, &mats).unwrap();
        assert_eq!(n, 1);
        assert_eq!(
            table_counts(&con).unwrap().iter().find(|(t,_)| t=="materials").unwrap().1,
            1
        );
    }
}