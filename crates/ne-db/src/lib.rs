//! ne-db：SQLite 6-state 回填（2026-10-08 口径）。
//!
//! 6-state = 伏笔(foreshadow) / 角色(char_state) / 事件(events) /
//! 物品(items) / 时间线(timeline) / 进度(chapters)。
//! 一章生成后 `tick_chapter` 以显式事务一次性推进（失败整体回滚），
//! 同时回填 card_usage（确定性选卡的数据源：被引次数 / 章距 / 伏笔待收）。
//! 9-30 审计实锤这 5 表 0 行 → M3 灌满。

use ne_core::{InjectionPacket, MaterialRef, UsageStat};
use rusqlite::{params, Connection};

/// 6-state 业务表 + chapters 的 schema（幂等，CREATE IF NOT EXISTS）。
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

/// 连接（或新建）novel.db，建表。
pub fn connect(db_path: &str) -> Result<Connection, anyhow::Error> {
    let con = Connection::open(db_path)?;
    con.execute_batch(SCHEMA)?;
    Ok(con)
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

    // chapters 主表（进度 state）
    tx.execute(
        "INSERT OR REPLACE INTO chapters(volume, chapter, title, status, words, created_at)
         VALUES(?1, ?2, ?3, 'generated', ?4, ?5)",
        params![volume, chapter as i64, title, words as i64, ts],
    )?;

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
    for e in &packet.events {
        tx.execute(
            "INSERT OR REPLACE INTO events(event_id, name, status, last_chapter)
             VALUES(?1,?2,?3,?4)",
            params![e.id, e.name, e.status, chapter as i64],
        )?;
    }

    // timeline：本章时刻点（时间线 state，civ_state 占位，M3 填公式）
    let civ = packet
        .world
        .get("note")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    tx.execute(
        "INSERT INTO timeline(volume, chapter, moment, civ_state) VALUES(?1,?2,?3,?4)",
        params![volume, chapter as i64, "chapter_tick", civ],
    )?;

    // foreshadow：due 的伏笔落库为 open（伏笔 state；items state 走 ITM 前缀扫描接缝）
    for fs in &packet.foreshadow_due {
        tx.execute(
            "INSERT OR IGNORE INTO foreshadow(fs_id, content, plant_chapter, deadline_chapter, status)
             VALUES(?1, '', ?2, 0, 'open')",
            params![fs, chapter as i64],
        )?;
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
    let open: Vec<(String, i64)> = {
        let mut st = con.prepare("SELECT fs_id, plant_chapter FROM foreshadow WHERE status = 'open'")?;
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