//! 资料库分类树（P2a）—— **纯运维元数据**，不碰管线语义。
//!
//! # 为什么可以直接开写
//! 分类只是「人给条目贴的组织标签」：改它不动任何文档内容、不影响门禁与推演。
//! 与「分层条目内容」（落在磁盘文件、会进注入面）有本质区别 —— 后者写前强制快照。
use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

/// 幂等建表。
pub fn ensure_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS lib_category (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT NOT NULL,
    parent_id   INTEGER,
    descr       TEXT NOT NULL DEFAULT '',
    archived    INTEGER NOT NULL DEFAULT 0,
    sort        INTEGER NOT NULL DEFAULT 0,
    created_ts  INTEGER NOT NULL DEFAULT (strftime('%s','now')),
    updated_ts  INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);
CREATE INDEX IF NOT EXISTS idx_lib_category_parent ON lib_category(parent_id);
CREATE TABLE IF NOT EXISTS lib_category_member (
    category_id INTEGER NOT NULL,
    layer       TEXT NOT NULL,
    item_key    TEXT NOT NULL,
    created_ts  INTEGER NOT NULL DEFAULT (strftime('%s','now')),
    PRIMARY KEY (category_id, layer, item_key)
);
CREATE TABLE IF NOT EXISTS webui_op_log (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    ts          INTEGER NOT NULL DEFAULT (strftime('%s','now')),
    actor       TEXT NOT NULL DEFAULT 'webui',
    action      TEXT NOT NULL,
    target      TEXT NOT NULL,
    detail      TEXT NOT NULL DEFAULT '',
    snapshot_id TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_webui_op_log_ts ON webui_op_log(ts DESC);
"#,
    )?;
    Ok(())
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 审计写入（P2b 也用）。
pub fn log_op(conn: &Connection, action: &str, target: &str, detail: &str, snapshot_id: &str) -> Result<()> {
    ensure_tables(conn)?;
    conn.execute(
        "INSERT INTO webui_op_log(ts,actor,action,target,detail,snapshot_id) VALUES(?1,'webui',?2,?3,?4,?5)",
        params![now(), action, target, detail, snapshot_id],
    )?;
    Ok(())
}

fn sanitize_name(name: &str) -> Result<String> {
    let n = name.trim();
    if n.is_empty() {
        bail!("分类名不能为空");
    }
    if n.chars().count() > 80 {
        bail!("分类名过长（≤80 字）");
    }
    if n.contains('\n') || n.contains('\r') || n.contains('\0') || n.contains('/') {
        bail!("分类名含非法字符");
    }
    Ok(n.to_string())
}

/// 分类树（默认不含已归档）。
pub fn list_tree(conn: &Connection) -> Result<Value> {
    ensure_tables(conn)?;
    let mut st = conn.prepare(
        "SELECT id,IFNULL(name,''),IFNULL(parent_id,-1),IFNULL(descr,''),archived,sort \
         FROM lib_category WHERE archived=0 ORDER BY sort,id",
    )?;
    let rows: Vec<Value> = st
        .query_map([], |r| {
            let pid: i64 = r.get(2)?;
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "name": r.get::<_, String>(1)?,
                "parent_id": if pid < 0 { Value::Null } else { json!(pid) },
                "descr": r.get::<_, String>(3)?,
                "archived": r.get::<_, i64>(4)? != 0,
                "sort": r.get::<_, i64>(5)?,
                "members": 0,
            }))
        })?
        .filter_map(|x| x.ok())
        .collect();
    let mut out = Vec::new();
    for mut v in rows {
        let id = v["id"].as_i64().unwrap_or(0);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM lib_category_member WHERE category_id=?1", params![id], |r| r.get(0))
            .unwrap_or(0);
        v["members"] = json!(n);
        out.push(v);
    }
    Ok(json!({
        "command": "webui library categories",
        "count": out.len(),
        "categories": out,
        "note": "archived=1 为软删除，默认不列出。",
    }))
}

/// 新建分类。body: JSON `{name,parent_id?,descr?}`。
pub fn create(conn: &Connection, body: &str) -> Result<Value> {
    ensure_tables(conn)?;
    let v: Value = serde_json::from_str(body.trim()).unwrap_or(json!({}));
    let name = sanitize_name(v["name"].as_str().unwrap_or(""))?;
    let parent = v["parent_id"].as_i64();
    let descr = v["descr"].as_str().unwrap_or("").trim().to_string();
    if descr.chars().count() > 500 {
        bail!("描述过长（≤500 字）");
    }
    if let Some(pid) = parent {
        let ok: Option<i64> = conn
            .query_row("SELECT id FROM lib_category WHERE id=?1 AND archived=0", params![pid], |r| r.get(0))
            .optional()?;
        if ok.is_none() {
            bail!("父分类 {pid} 不存在");
        }
    }
    let dup: Option<i64> = conn
        .query_row(
            "SELECT id FROM lib_category WHERE name=?1 AND IFNULL(parent_id,-1)=IFNULL(?2,-1) AND archived=0",
            params![name, parent],
            |r| r.get(0),
        )
        .optional()?;
    if dup.is_some() {
        bail!("同层已存在同名分类：{name}");
    }
    conn.execute(
        "INSERT INTO lib_category(name,parent_id,descr) VALUES(?1,?2,?3)",
        params![name, parent, descr],
    )?;
    let id = conn.last_insert_rowid();
    log_op(conn, "category.create", &format!("category:{id}"), &format!("name={name}"), "")?;
    Ok(json!({"ok": true, "id": id, "name": name}))
}

/// 改名 / 改描述 / 归档 / 排序。
pub fn update(conn: &Connection, id: i64, body: &str) -> Result<Value> {
    ensure_tables(conn)?;
    let v: Value = serde_json::from_str(body.trim()).unwrap_or(json!({}));
    let cur: Option<(String, Option<i64>)> = conn
        .query_row("SELECT name,parent_id FROM lib_category WHERE id=?1", params![id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?;
    let Some((old_name, parent)) = cur else {
        bail!("分类 {id} 不存在");
    };
    let mut sets: Vec<String> = Vec::new();
    if let Some(n) = v["name"].as_str() {
        let n = sanitize_name(n)?;
        let dup: Option<i64> = conn
            .query_row(
                "SELECT id FROM lib_category WHERE name=?1 AND IFNULL(parent_id,-1)=IFNULL(?2,-1) AND archived=0 AND id<>?3",
                params![n, parent, id],
                |r| r.get(0),
            )
            .optional()?;
        if dup.is_some() {
            bail!("同层已存在同名分类：{n}");
        }
        sets.push(format!("name='{}'", n.replace('\'', "''")));
    }
    if let Some(d) = v["descr"].as_str() {
        sets.push(format!("descr='{}'", d.replace('\'', "''")));
    }
    if let Some(a) = v["archived"].as_bool() {
        sets.push(format!("archived={}", if a { 1 } else { 0 }));
    }
    if let Some(s) = v["sort"].as_i64() {
        sets.push(format!("sort={s}"));
    }
    if sets.is_empty() {
        bail!("无可更新字段（name/descr/archived/sort）");
    }
    let sql = format!("UPDATE lib_category SET {}, updated_ts=?1 WHERE id=?2", sets.join(","));
    conn.execute(&sql, params![now(), id])?;
    log_op(conn, "category.update", &format!("category:{id}"), &format!("was={old_name}"), "")?;
    Ok(json!({"ok": true, "id": id, "changed": sets.len()}))
}

/// 删除（仅空分类）。
pub fn delete(conn: &Connection, id: i64) -> Result<Value> {
    ensure_tables(conn)?;
    let exist: Option<String> = conn
        .query_row("SELECT name FROM lib_category WHERE id=?1", params![id], |r| r.get(0))
        .optional()?;
    let Some(name) = exist else {
        bail!("分类 {id} 不存在");
    };
    let kids: i64 = conn
        .query_row("SELECT COUNT(*) FROM lib_category WHERE parent_id=?1", params![id], |r| r.get(0))
        .unwrap_or(0);
    if kids > 0 {
        bail!("分类「{name}」下还有 {kids} 个子分类，禁止删除");
    }
    let mem: i64 = conn
        .query_row("SELECT COUNT(*) FROM lib_category_member WHERE category_id=?1", params![id], |r| r.get(0))
        .unwrap_or(0);
    if mem > 0 {
        bail!("分类「{name}」下还有 {mem} 个文档，禁止删除（先移走或归档）");
    }
    conn.execute("DELETE FROM lib_category WHERE id=?1", params![id])?;
    log_op(conn, "category.delete", &format!("category:{id}"), &format!("name={name}"), "")?;
    Ok(json!({"ok": true, "deleted": id, "name": name}))
}

/// 某分类下全部条目（跨层）。
pub fn docs(conn: &Connection, id: i64) -> Result<Value> {
    ensure_tables(conn)?;
    let mut st = conn.prepare(
        "SELECT m.layer, m.item_key, IFNULL(i.title,''), IFNULL(i.chars,0) \
         FROM lib_category_member m LEFT JOIN library_index i \
           ON i.layer=m.layer AND i.key=m.item_key \
         WHERE m.category_id=?1 ORDER BY m.layer, m.item_key",
    )?;
    let rows: Vec<Value> = st
        .query_map(params![id], |r| {
            Ok(json!({
                "layer": r.get::<_, String>(0)?,
                "key": r.get::<_, String>(1)?,
                "title": r.get::<_, String>(2)?,
                "chars": r.get::<_, i64>(3)?,
            }))
        })?
        .filter_map(|x| x.ok())
        .collect();
    Ok(json!({"command": "webui library category docs", "category_id": id, "count": rows.len(), "docs": rows}))
}

/// 条目加入分类。
pub fn add_member(conn: &Connection, id: i64, layer: &str, key: &str) -> Result<Value> {
    ensure_tables(conn)?;
    conn.execute(
        "INSERT OR IGNORE INTO lib_category_member(category_id,layer,item_key) VALUES(?1,?2,?3)",
        params![id, layer, key],
    )?;
    log_op(conn, "category.add_member", &format!("category:{id}"), &format!("{layer}/{key}"), "")?;
    Ok(json!({"ok": true, "category_id": id, "layer": layer, "key": key}))
}

pub fn rm_member(conn: &Connection, id: i64, layer: &str, key: &str) -> Result<Value> {
    ensure_tables(conn)?;
    let n = conn.execute(
        "DELETE FROM lib_category_member WHERE category_id=?1 AND layer=?2 AND item_key=?3",
        params![id, layer, key],
    )?;
    log_op(conn, "category.rm_member", &format!("category:{id}"), &format!("{layer}/{key}"), "")?;
    Ok(json!({"ok": true, "removed": n}))
}

/// 操作审计日志（最近 N 条）。
pub fn op_logs(conn: &Connection, limit: usize) -> Result<Value> {
    ensure_tables(conn)?;
    let lim = limit.clamp(1, 500) as i64;
    let mut st = conn.prepare(
        "SELECT id,ts,actor,action,target,detail,snapshot_id FROM webui_op_log ORDER BY id DESC LIMIT ?1",
    )?;
    let rows: Vec<Value> = st
        .query_map(params![lim], |r| {
            let ts: i64 = r.get(1)?;
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "ts": ts,
                "time": fmt_utc(ts),
                "actor": r.get::<_, String>(2)?,
                "action": r.get::<_, String>(3)?,
                "target": r.get::<_, String>(4)?,
                "detail": r.get::<_, String>(5)?,
                "snapshot_id": r.get::<_, String>(6)?,
            }))
        })?
        .filter_map(|x| x.ok())
        .collect();
    Ok(json!({"command": "webui audit op-logs", "count": rows.len(), "logs": rows}))
}

/// Unix 秒 → `YYYY-MM-DD HH:MM:SSZ`（手算，零依赖）。
fn fmt_utc(ts: i64) -> String {
    if ts <= 0 {
        return String::new();
    }
    let days = ts / 86400;
    let rem = ts % 86400;
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (mut y, mut d) = (1970i64, days);
    loop {
        let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
        let len = if leap { 366 } else { 365 };
        if d < len {
            break;
        }
        d -= len;
        y += 1;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let ml = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut m = 0usize;
    while m < 12 && d >= ml[m] {
        d -= ml[m];
        m += 1;
    }
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}Z", y, m + 1, d + 1, h, mi, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        ensure_tables(&c).unwrap();
        c
    }

    #[test]
    fn create_list_delete_roundtrip() {
        let c = mk();
        let r = create(&c, r#"{"name":"卷一素材"}"#).unwrap();
        let id = r["id"].as_i64().unwrap();
        assert_eq!(list_tree(&c).unwrap()["count"], 1);
        add_member(&c, id, "L0", "k1").unwrap();
        assert!(delete(&c, id).is_err(), "有成员应拒绝删除");
        rm_member(&c, id, "L0", "k1").unwrap();
        assert!(delete(&c, id).is_ok());
    }

    #[test]
    fn rejects_empty_and_dup() {
        let c = mk();
        assert!(create(&c, r#"{"name":"  "}"#).is_err());
        create(&c, r#"{"name":"A"}"#).unwrap();
        assert!(create(&c, r#"{"name":"A"}"#).is_err());
    }

    #[test]
    fn child_blocks_parent_delete() {
        let c = mk();
        let p = create(&c, r#"{"name":"父"}"#).unwrap()["id"].as_i64().unwrap();
        create(&c, &format!(r#"{{"name":"子","parent_id":{p}}}"#)).unwrap();
        assert!(delete(&c, p).is_err());
    }

    #[test]
    fn op_log_records() {
        let c = mk();
        create(&c, r#"{"name":"X"}"#).unwrap();
        assert!(op_logs(&c, 10).unwrap()["count"].as_i64().unwrap() >= 1);
    }
}