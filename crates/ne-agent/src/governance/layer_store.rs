//! 分层条目 CRUD（P2b）—— **涉及磁盘文件**，写前强制快照。
//!
//! # 与 `category_store` 的本质区别
//! - 分类：纯元数据 → 直接开写
//! - 条目内容：`library_index.source` 指向**磁盘文件**，且会进注入面 → **写前必须快照**
//!
//! # 安全铁则
//! 1. **路径锁死在库根内**：`source` 解析后必须 `canonicalize` 落在 `lib_root` 之下，
//!    否则拒绝（防 `../` 穿越）。
//! 2. **写前强制快照**：调 `diff_snapshot::create`；快照失败则**不写**。
//! 3. **删除 = 移到 09_冷备份**，不真删（可回滚）。
//! 4. 所有写操作写 `webui_op_log`。
use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use super::category_store;

/// 条目 key 校验（对应 `library_index.key`）。
fn sanitize_key(k: &str) -> Result<String> {
    let s = k.trim();
    if s.is_empty() || s.chars().count() > 200 {
        bail!("key 非法（空或过长）");
    }
    if s.contains('/') || s.contains('\\') || s.contains('\0') || s.contains("..") {
        bail!("key 含非法字符");
    }
    Ok(s.to_string())
}

/// 层名校验（供路由先校验，再调用具体函数）。
pub fn check_layer(l: &str) -> Result<()> {
    sanitize_layer(l).map(|_| ())
}

/// 层名校验（库内实测层 + 任务书 6 层白名单）。
fn sanitize_layer(l: &str) -> Result<String> {
    let s = l.trim();
    const OK: [&str; 16] = [
        "L0", "L0.5", "L1", "L1x", "L2", "L3", "L4", "L5",
        "ANCHOR", "AUX", "GUARD", "SYSTEM", "META", "RAW", "DRAFT", "TMP",
    ];
    if !OK.contains(&s) {
        bail!("层名不在白名单：{s}（允许 L0/L0.5/L1/L1x/L2/L3/L4/L5/ANCHOR/AUX/GUARD 等）");
    }
    Ok(s.to_string())
}

/// 把 `library_index.source` 解析成库根内的绝对路径（**拒绝穿越**）。
pub fn resolve_source(lib_root: &Path, source: &str) -> Result<PathBuf> {
    let s = source.trim();
    if s.is_empty() {
        bail!("source 为空（该条目无源文件）");
    }
    if s.contains("..") || s.contains('\0') {
        bail!("source 含非法序列");
    }
    let p = Path::new(s);
    let abs = if p.is_absolute() { p.to_path_buf() } else { lib_root.join(p) };
    // 规范化比较：不要求目标存在（新建时会创建）
    let root = lib_root.canonicalize().unwrap_or_else(|_| lib_root.to_path_buf());
    let base = abs.parent().map(|x| x.to_path_buf()).unwrap_or_default();
    let base_c = base.canonicalize().unwrap_or(base);
    if !base_c.starts_with(&root) {
        bail!("路径越出库根，已拒绝：{s}");
    }
    Ok(abs)
}

/// 写前快照（失败即中止写操作）。
fn snapshot_guard(conn: &Connection, lib_root: &Path, label: &str) -> Result<String> {
    match super::diff_snapshot::create(conn, lib_root, label) {
        Ok((id, _n)) => Ok(id),
        Err(e) => bail!("写前快照失败，已中止（不写入）：{e}"),
    }
}

/// 读单条完整内容（正文从磁盘读）。
pub fn get_item(conn: &Connection, lib_root: &Path, layer: &str, key: &str) -> Result<Value> {
    let layer = sanitize_layer(layer)?;
    let key = sanitize_key(key)?;
    let row: Option<(String, String, i64, String, i64)> = conn
        .query_row(
            "SELECT IFNULL(source,''),IFNULL(title,''),IFNULL(chars,0),IFNULL(tags,'[]'),IFNULL(dirty,0) \
             FROM library_index WHERE layer=?1 AND key=?2",
            params![layer, key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((source, title, chars, tags, dirty)) = row else {
        bail!("条目不存在：{layer}/{key}");
    };
    let mut content = String::new();
    let mut exists = false;
    if !source.is_empty() {
        if let Ok(p) = resolve_source(lib_root, &source) {
            exists = p.is_file();
            if exists {
                content = std::fs::read_to_string(&p).unwrap_or_default();
            }
        }
    }
    let note = super::note_tag_store::get_note(conn, &key).unwrap_or(None);
    Ok(json!({
        "command": "webui layer item get",
        "layer": layer, "key": key, "title": title,
        "source": source, "file_exists": exists,
        "chars": chars, "tags": serde_json::from_str::<Value>(&tags).unwrap_or(json!([])),
        "dirty": dirty,
        "content": content,
        "note": note,
    }))
}

/// 层条目列表（分页 + 筛选：tag/status/q）。
pub fn list_items(
    conn: &Connection,
    layer: Option<&str>,
    tag: Option<&str>,
    status: Option<&str>,
    q: Option<&str>,
    limit: usize,
    offset: usize,
) -> Result<Value> {
    let lim = limit.clamp(1, 500) as i64;
    let off = offset as i64;
    let mut sql = String::from(
        "SELECT layer,key,IFNULL(title,''),chars,IFNULL(source,''),IFNULL(dirty,0) FROM library_index",
    );
    let mut where_parts: Vec<String> = Vec::new();
    let mut binds: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(l) = layer.filter(|s| !s.trim().is_empty()) {
        where_parts.push(format!("layer=?{}", binds.len() + 1));
        binds.push(Box::new(l.to_string()));
    }
    if let Some(s) = q.filter(|s| !s.trim().is_empty()) {
        where_parts.push(format!(
            "(key LIKE ?{0} OR title LIKE ?{0})",
            binds.len() + 1
        ));
        binds.push(Box::new(format!("%{s}%")));
    }
    if !where_parts.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&where_parts.join(" AND "));
    }
    sql.push_str(" ORDER BY layer,key");
    let mut st = conn.prepare(&sql)?;
    let args: Vec<&dyn rusqlite::ToSql> = binds.iter().map(|b| b.as_ref()).collect();
    let all: Vec<Value> = st
        .query_map(args.as_slice(), |r| {
            Ok(json!({
                "layer": r.get::<_, String>(0)?,
                "key": r.get::<_, String>(1)?,
                "title": r.get::<_, String>(2)?,
                "chars": r.get::<_, i64>(3)?,
                "source": r.get::<_, String>(4)?,
                "dirty": r.get::<_, i64>(5)?,
            }))
        })?
        .filter_map(|x| x.ok())
        .collect();

    // 标签 / 状态过滤（复用 note_tag_store，避免 SQL 拼接 tags JSON）
    let ann = super::note_tag_store::all_annotations(conn).unwrap_or_default();
    let mut items: Vec<Value> = Vec::new();
    for mut v in all {
        let k = v["key"].as_str().unwrap_or("").to_string();
        let a = ann.get(&k);
        let tags_v = a.map(|x| x["tags"].clone()).unwrap_or(json!([]));
        let st_v = a.and_then(|x| x["note"]["status"].as_str().map(|s| s.to_string()));
        if let Some(t) = tag.filter(|s| !s.trim().is_empty()) {
            let hit = tags_v.as_array().map(|arr| arr.iter().any(|x| x.as_str() == Some(t))).unwrap_or(false);
            if !hit {
                continue;
            }
        }
        if let Some(s) = status.filter(|s| !s.trim().is_empty()) {
            if st_v.as_deref() != Some(s) {
                continue;
            }
        }
        v["tags"] = tags_v;
        v["note_status"] = json!(st_v);
        items.push(v);
    }
    let total = items.len() as i64;
    let page: Vec<Value> = items.into_iter().skip(off as usize).take(lim as usize).collect();
    Ok(json!({
        "command": "webui layer items",
        "layer": layer, "count": page.len(), "total": total,
        "limit": lim, "offset": off,
        "items": page,
    }))
}

/// 新建条目：写磁盘 + 插索引（**写前快照**）。
pub fn create_item(conn: &Connection, lib_root: &Path, layer: &str, body: &str) -> Result<Value> {
    let layer = sanitize_layer(layer)?;
    let v: Value = serde_json::from_str(body.trim()).unwrap_or(json!({}));
    let title = v["title"].as_str().unwrap_or("").trim().to_string();
    let content = v["content"].as_str().unwrap_or("").to_string();
    let key = match v["key"].as_str() {
        Some(k) if !k.trim().is_empty() => sanitize_key(k)?,
        _ => bail!("新建需要 key"),
    };
    if content.is_empty() {
        bail!("新建需要 content（正文）");
    }
    // 唯一性
    let dup: Option<i64> = conn
        .query_row("SELECT id FROM library_index WHERE layer=?1 AND key=?2", params![layer, key], |r| r.get(0))
        .optional()?;
    if dup.is_some() {
        bail!("已存在：{layer}/{key}");
    }
    // 目标路径：库根下 02_资料库/_webui/{layer}/{key}.md（不覆盖既有目录结构）
    let rel = format!("02_资料库/_webui/{layer}/{key}.md");
    let p = resolve_source(lib_root, &rel)?;
    if p.exists() {
        bail!("目标文件已存在：{rel}");
    }
    // ★ 写前快照
    let snap = snapshot_guard(conn, lib_root, &format!("webui-create-{layer}-{key}"))?;
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let md = format!("# {}\n\n{}", if title.is_empty() { &key } else { &title }, content);
    std::fs::write(&p, &md)?;
    let chars = md.chars().filter(|c| !c.is_whitespace()).count() as i64;
    conn.execute(
        "INSERT INTO library_index(layer,key,source,chars,title,tags,dirty) VALUES(?1,?2,?3,?4,?5,'[]',0)",
        params![layer, key, rel, chars, title],
    )?;
    category_store::log_op(conn, "item.create", &format!("{layer}/{key}"), &format!("chars={chars}"), &snap)?;
    Ok(json!({"ok": true, "layer": layer, "key": key, "source": rel, "chars": chars, "snapshot_id": snap}))
}

/// 更新条目（改正文 / 标题）。**写前快照**。
pub fn update_item(conn: &Connection, lib_root: &Path, layer: &str, key: &str, body: &str) -> Result<Value> {
    let layer = sanitize_layer(layer)?;
    let key = sanitize_key(key)?;
    let v: Value = serde_json::from_str(body.trim()).unwrap_or(json!({}));
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT IFNULL(source,''),IFNULL(title,'') FROM library_index WHERE layer=?1 AND key=?2",
            params![layer, key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((source, old_title)) = row else {
        bail!("条目不存在：{layer}/{key}");
    };
    let p = resolve_source(lib_root, &source)?;
    let snap = snapshot_guard(conn, lib_root, &format!("webui-update-{layer}-{key}"))?;
    let mut changed: Vec<&str> = Vec::new();
    if let Some(c) = v["content"].as_str() {
        std::fs::write(&p, c)?;
        changed.push("content");
    }
    let new_title = v["title"].as_str().map(|s| s.trim().to_string()).unwrap_or(old_title);
    if v["title"].is_string() {
        changed.push("title");
    }
    // ★ 只改标题、没改正文时：必须同步把磁盘首行 `# 标题` 也改掉。
    //   否则 library_index.title 与文件首行不一致（索引说 A、文件说 B），
    //   等下一次全库重扫时，索引会被文件首行覆盖回去，用户改的标题莫名消失。
    //   传了 content 的情况不动文件：content 是整体覆写，首行由调用方自己写。
    if v["title"].is_string() && !v["content"].is_string() {
        let cur = std::fs::read_to_string(&p).unwrap_or_default();
        let mut lines: Vec<String> = cur.lines().map(|l| l.to_string()).collect();
        let want = format!("# {}", new_title);
        if lines.first().map(|l| l.trim_start().starts_with("# ")).unwrap_or(false) {
            lines[0] = want;
        } else if !new_title.is_empty() {
            // 首行不是标题行：在文件头插一行，正文一行不丢
            lines.insert(0, want);
            lines.insert(1, String::new());
        }
        let mut out = lines.join("\n");
        if !out.ends_with('\n') {
            out.push('\n');
        }
        std::fs::write(&p, out)?;
    }
    // 重算 chars
    let cur = std::fs::read_to_string(&p).unwrap_or_default();
    let chars = cur.chars().filter(|c| !c.is_whitespace()).count() as i64;
    conn.execute(
        "UPDATE library_index SET title=?1,chars=?2 WHERE layer=?3 AND key=?4",
        params![new_title, chars, layer, key],
    )?;
    category_store::log_op(conn, "item.update", &format!("{layer}/{key}"), &format!("changed={changed:?} chars={chars}"), &snap)?;
    Ok(json!({"ok": true, "layer": layer, "key": key, "changed": changed, "chars": chars, "snapshot_id": snap}))
}

/// 删除条目：**移到 09_冷备份**（不真删）+ 删索引。**写前快照**。
pub fn delete_item(conn: &Connection, lib_root: &Path, layer: &str, key: &str) -> Result<Value> {
    let layer = sanitize_layer(layer)?;
    let key = sanitize_key(key)?;
    let row: Option<String> = conn
        .query_row(
            "SELECT IFNULL(source,'') FROM library_index WHERE layer=?1 AND key=?2",
            params![layer, key],
            |r| r.get(0),
        )
        .optional()?;
    let Some(source) = row else {
        bail!("条目不存在：{layer}/{key}");
    };
    let snap = snapshot_guard(conn, lib_root, &format!("webui-delete-{layer}-{key}"))?;
    let mut moved = String::new();
    if !source.is_empty() {
        if let Ok(p) = resolve_source(lib_root, &source) {
            if p.is_file() {
                let dst_rel = format!("09_冷备份/_webui_del/{layer}/{key}.md");
                let dst = lib_root.join(&dst_rel);
                if let Some(d) = dst.parent() {
                    std::fs::create_dir_all(d)?;
                }
                let src_bytes = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                std::fs::rename(&p, &dst).or_else(|_| std::fs::copy(&p, &dst).map(|_| ()))?;
                // ── 任务书 #6：冷备份完整性校验（失败则阻断，防素材丢失）
                if !dst.is_file() {
                    let _ = category_store::log_op(conn, "item.delete.VERIFY_FAIL",
                        &format!("{layer}/{key}"), "冷备份文件未生成", &snap);
                    bail!("删除已中止：冷备份未生成（{dst_rel}），素材未丢失可重试");
                }
                let dst_bytes = std::fs::metadata(&dst).map(|m| m.len()).unwrap_or(0);
                if src_bytes != dst_bytes {
                    let _ = category_store::log_op(conn, "item.delete.VERIFY_FAIL",
                        &format!("{layer}/{key}"),
                        &format!("字节数不一致 src={src_bytes} dst={dst_bytes}"), &snap);
                    bail!("删除已中止：冷备份字节数不一致（src={src_bytes} dst={dst_bytes}）");
                }
                // 校验通过：源文件必须已不在原位
                if p.exists() {
                    let _ = category_store::log_op(conn, "item.delete.VERIFY_FAIL",
                        &format!("{layer}/{key}"), "源文件仍存在（移动未生效）", &snap);
                    bail!("删除已中止：源文件仍存在，移动未生效");
                }
                moved = dst_rel;
            }
        }
    }
    conn.execute("DELETE FROM library_index WHERE layer=?1 AND key=?2", params![layer, key])?;
    category_store::log_op(
        conn, "item.delete", &format!("{layer}/{key}"),
        &format!("moved_to={moved}"), &snap,
    )?;
    Ok(json!({"ok": true, "deleted": format!("{layer}/{key}"), "moved_to": moved, "snapshot_id": snap}))
}

/// 批量操作：`{action: tag|status|move|delete, keys:[], tag?, status?, to_layer?}`。
pub fn batch(conn: &Connection, lib_root: &Path, layer: &str, body: &str) -> Result<Value> {
    let layer = sanitize_layer(layer)?;
    let v: Value = serde_json::from_str(body.trim()).unwrap_or(json!({}));
    let action = v["action"].as_str().unwrap_or("").to_string();
    let keys: Vec<String> = v["keys"].as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();
    if keys.is_empty() {
        bail!("需要 keys 数组");
    }
    match action.as_str() {
        "tag" => {
            let tag = v["tag"].as_str().unwrap_or("").to_string();
            if tag.is_empty() { bail!("需要 tag"); }
            let mut n = 0;
            for k in &keys {
                if super::note_tag_store::add_tags(conn, k, std::slice::from_ref(&tag), "manual").is_ok() {
                    n += 1;
                }
            }
            category_store::log_op(conn, "batch.tag", &layer, &format!("tag={tag} n={n}"), "")?;
            Ok(json!({"ok": true, "tagged": n}))
        }
        "status" => {
            let st = v["status"].as_str().unwrap_or("").to_string();
            let mut n = 0;
            for k in &keys {
                if super::note_tag_store::set_note(conn, k, Some(&st), None).is_ok() { n += 1; }
            }
            category_store::log_op(conn, "batch.status", &layer, &format!("status={st} n={n}"), "")?;
            Ok(json!({"ok": true, "updated": n}))
        }
        "move" => {
            let to = sanitize_layer(v["to_layer"].as_str().unwrap_or(""))?;
            if to == layer { bail!("目标层与当前层相同"); }
            let snap = snapshot_guard(conn, lib_root, &format!("webui-move-{layer}-to-{to}"))?;
            let mut n = 0;
            for k in &keys {
                let c = conn.execute(
                    "UPDATE library_index SET layer=?1 WHERE layer=?2 AND key=?3",
                    params![to, layer, k],
                ).unwrap_or(0);
                n += c;
            }
            category_store::log_op(conn, "batch.move", &layer, &format!("to={to} n={n}"), &snap)?;
            Ok(json!({"ok": true, "moved": n, "to_layer": to, "snapshot_id": snap}))
        }
        "delete" => {
            let snap = snapshot_guard(conn, lib_root, &format!("webui-batchdel-{layer}"))?;
            let mut n = 0;
            for k in &keys {
                if delete_item(conn, lib_root, &layer, k).is_ok() { n += 1; }
            }
            category_store::log_op(conn, "batch.delete", &layer, &format!("n={n}"), &snap)?;
            Ok(json!({"ok": true, "deleted": n, "snapshot_id": snap}))
        }
        _ => bail!("action 需为 tag/status/move/delete"),
    }
}

/// 上下游关联：按 key 前缀猜同族，按分类成员 + 引用出现次数找关联。
pub fn graph(conn: &Connection, layer: &str, key: &str) -> Result<Value> {
    let layer = sanitize_layer(layer)?;
    let key = sanitize_key(key)?;
    let me = format!("{layer}/{key}");
    // 同分类的其它条目
    let mut st = conn.prepare(
        "SELECT DISTINCT m2.layer, m2.item_key FROM lib_category_member m1 \
         JOIN lib_category_member m2 ON m2.category_id=m1.category_id \
         WHERE m1.layer=?1 AND m1.item_key=?2 AND NOT (m2.layer=?1 AND m2.item_key=?2)",
    )?;
    let related: Vec<Value> = st
        .query_map(params![layer, key], |r| {
            Ok(json!({"layer": r.get::<_, String>(0)?, "key": r.get::<_, String>(1)?, "via": "category"}))
        })?
        .filter_map(|x| x.ok())
        .collect();
    // 同名 key 在其它层（上下游常见形态）
    let mut st2 = conn.prepare("SELECT layer,key FROM library_index WHERE key=?1 AND layer<>?2")?;
    let same_key: Vec<Value> = st2
        .query_map(params![key, layer], |r| {
            Ok(json!({"layer": r.get::<_, String>(0)?, "key": r.get::<_, String>(1)?, "via": "same_key"}))
        })?
        .filter_map(|x| x.ok())
        .collect();
    Ok(json!({
        "command": "webui layer graph",
        "node": me, "related": related, "same_key_other_layers": same_key,
        "note": "relation = 同分类 + 同名跨层；后续可扩为解析正文内卡号引用。",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE library_index(id INTEGER PRIMARY KEY AUTOINCREMENT,layer TEXT,key TEXT,title TEXT,chars INTEGER,source TEXT,src_mtime INTEGER NOT NULL DEFAULT 0,tags TEXT DEFAULT '[]',dirty INTEGER DEFAULT 0,UNIQUE(layer,key));",
        ).unwrap();
        super::super::note_tag_store::ensure_tables(&c).unwrap();
        category_store::ensure_tables(&c).unwrap();
        c
    }

    #[test]
    fn rejects_path_escape() {
        let root = std::env::temp_dir().join("webui_p2b_root");
        let _ = std::fs::create_dir_all(&root);
        assert!(resolve_source(&root, "../../etc/passwd").is_err());
        assert!(resolve_source(&root, "a/../../../x.md").is_err());
        assert!(resolve_source(&root, "ok/inside.md").is_ok());
    }

    #[test]
    fn layer_whitelist() {
        assert!(sanitize_layer("L0").is_ok());
        assert!(sanitize_layer("L5").is_ok());
        assert!(sanitize_layer("BAD").is_err());
        assert!(sanitize_layer("../../x").is_err());
    }

    #[test]
    fn key_sanitize() {
        assert!(sanitize_key("CHAR-99-001").is_ok());
        assert!(sanitize_key("a/b").is_err());
        assert!(sanitize_key("..").is_err());
    }

    #[test]
    fn create_update_delete_roundtrip() {
        let root = std::env::temp_dir().join("webui_p2b_rt");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let c = mk_conn();
        let r = create_item(&c, &root, "L0", r#"{"key":"T-1","title":"测试","content":"正文一"}"#).unwrap();
        assert_eq!(r["ok"], true);
        let g = get_item(&c, &root, "L0", "T-1").unwrap();
        assert!(g["content"].as_str().unwrap().contains("正文一"));
        update_item(&c, &root, "L0", "T-1", r#"{"content":"正文二改"}"#).unwrap();
        let g2 = get_item(&c, &root, "L0", "T-1").unwrap();
        assert!(g2["content"].as_str().unwrap().contains("正文二改"));
        let d = delete_item(&c, &root, "L0", "T-1").unwrap();
        assert!(d["moved_to"].as_str().unwrap().contains("09_冷备份"));
        assert!(get_item(&c, &root, "L0", "T-1").is_err());
    }
}