//! 管线文件读写（页面②③④⑤）—— L0/L1/L2/L3 的 md 读 + 写。
//!
//! 安全铁则：路径锁库根内 / 白名单目录 / 仅 .md / 写前强制快照 / 写操作进审计。
use anyhow::{bail, Result};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use super::category_store;

const ALLOW_DIRS: [&str; 3] = ["03_叙事规划/", "02_资料库/", "05_管线框架/"];

/// 解析并校验路径（锁库根内 + 白名单 + 仅 .md）。
pub fn resolve_md(lib_root: &Path, rel: &str) -> Result<PathBuf> {
    let s = rel.trim().trim_start_matches('/');
    if s.is_empty() { bail!("path 不能为空"); }
    if s.contains("..") || s.contains('\0') { bail!("path 含非法序列"); }
    if !s.ends_with(".md") { bail!("只允许 .md 文件"); }
    if !ALLOW_DIRS.iter().any(|d| s.starts_with(d)) {
        bail!("path 不在白名单目录（允许：{}）", ALLOW_DIRS.join(" / "));
    }
    let abs = lib_root.join(s);
    let root = lib_root.canonicalize().unwrap_or_else(|_| lib_root.to_path_buf());
    let mut probe = abs.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    while !probe.exists() {
        match probe.parent() { Some(p) => probe = p.to_path_buf(), None => break }
    }
    let probe_c = probe.canonicalize().unwrap_or(probe);
    if !probe_c.starts_with(&root) { bail!("路径越出库根，已拒绝：{s}"); }
    Ok(abs)
}

/// 读 md 文件。
pub fn read_file(lib_root: &Path, rel: &str) -> Result<Value> {
    let p = resolve_md(lib_root, rel)?;
    if !p.is_file() { bail!("文件不存在：{rel}"); }
    let text = std::fs::read_to_string(&p)?;
    let meta = std::fs::metadata(&p)?;
    let mtime = meta.modified().ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64).unwrap_or(0);
    Ok(json!({
        "command": "webui pipeline file get", "path": rel,
        "bytes": meta.len(), "chars": text.chars().count(),
        "lines": text.lines().count(), "mtime": mtime, "content": text,
    }))
}

/// 写 md 文件（写前强制快照 + 回读校验）。
pub fn write_file(conn: &Connection, lib_root: &Path, rel: &str, body: &str) -> Result<Value> {
    let p = resolve_md(lib_root, rel)?;
    let v: Value = serde_json::from_str(body.trim()).unwrap_or(json!({}));
    let Some(content) = v["content"].as_str() else {
        bail!("需要 content 字段（新正文）");
    };
    if content.trim().is_empty() { bail!("content 不能为空（防误清空）"); }
    let snap = match super::diff_snapshot::create(
        conn, lib_root, &format!("webui-file-{}", rel.replace('/', "_"))) {
        Ok((id, _n)) => id,
        Err(e) => bail!("写前快照失败，已中止：{e}"),
    };
    let mut backup = String::new();
    if p.is_file() {
        let b = p.with_extension("md.bak");
        std::fs::copy(&p, &b)?;
        backup = b.strip_prefix(lib_root).unwrap_or(&b).display().to_string();
    } else if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(&p, content)?;
    let back = std::fs::read_to_string(&p)?;
    if back != content {
        let _ = category_store::log_op(conn, "file.write.VERIFY_FAIL", rel, "回读不一致", &snap);
        bail!("写入后回读不一致，已中止（请检查磁盘）");
    }
    let chars = content.chars().count();
    category_store::log_op(conn, "file.write", rel, &format!("chars={chars} backup={backup}"), &snap)?;
    Ok(json!({"ok": true, "path": rel, "chars": chars, "backup": backup, "snapshot_id": snap}))
}

/// 列层文件（L0/L1/L2/L3）。
pub fn list_layer(lib_root: &Path, layer: &str) -> Result<Value> {
    let items: Vec<String> = match layer {
        "L0" => vec!["03_叙事规划/00_整本概括.md".to_string()],
        "L1" => vec![
            "03_叙事规划/00_叙事总纲.md".to_string(),
            "03_叙事规划/00_宏大对抗总规划.md".to_string(),
        ],
        "L2" => list_dir_md(lib_root, "03_叙事规划/分卷大纲")?,
        "L3" => list_dir_md(lib_root, "03_叙事规划/细纲")?,
        _ => bail!("不支持列出的层：{layer}（支持 L0/L1/L2/L3）"),
    };
    let rows: Vec<Value> = items.into_iter().map(|rel| {
        let p = lib_root.join(&rel);
        let (exists, bytes, chars) = if p.is_file() {
            let t = std::fs::read_to_string(&p).unwrap_or_default();
            (true, std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0) as i64, t.chars().count() as i64)
        } else { (false, 0, 0) };
        json!({"path": rel, "exists": exists, "bytes": bytes, "chars": chars})
    }).collect();
    Ok(json!({"command": "webui pipeline layer list", "layer": layer, "count": rows.len(), "files": rows}))
}

fn list_dir_md(lib_root: &Path, rel_dir: &str) -> Result<Vec<String>> {
    let dir = lib_root.join(rel_dir);
    let mut v = Vec::new();
    if dir.is_dir() {
        let mut names: Vec<String> = std::fs::read_dir(&dir)?.flatten()
            .filter(|e| e.path().is_file() && e.path().extension().map(|x| x == "md").unwrap_or(false))
            .map(|e| e.file_name().to_string_lossy().to_string()).collect();
        names.sort();
        for n in names { v.push(format!("{rel_dir}/{n}")); }
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal_and_non_md() {
        let d = std::env::temp_dir().join("pf_guard");
        let _ = std::fs::create_dir_all(d.join("03_叙事规划"));
        assert!(resolve_md(&d, "../../etc/passwd").is_err());
        assert!(resolve_md(&d, "03_叙事规划/x.txt").is_err());
        assert!(resolve_md(&d, "01_别的/x.md").is_err());
        assert!(resolve_md(&d, "03_叙事规划/ok.md").is_ok());
    }

    #[test]
    fn read_missing_reports_error() {
        let d = std::env::temp_dir().join("pf_read");
        let _ = std::fs::create_dir_all(&d);
        assert!(read_file(&d, "03_叙事规划/nope.md").is_err());
    }

    #[test]
    fn list_layer_l1_l2() {
        let d = std::env::temp_dir().join("pf_list");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("03_叙事规划/分卷大纲")).unwrap();
        std::fs::write(d.join("03_叙事规划/分卷大纲/卷01.md"), "x").unwrap();
        assert_eq!(list_layer(&d, "L1").unwrap()["count"], 2);
        assert_eq!(list_layer(&d, "L2").unwrap()["count"], 1);
        assert!(list_layer(&d, "L9").is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}

// ─────────────────────────── L3 章级读写（页面⑤）

/// 列出细纲里的章（180 章/卷）。
///
/// 细纲格式：`### 第N章 标题`。返回章号 + 标题 + 字数。
pub fn list_chapters(lib_root: &Path, volume: &str) -> Result<Value> {
    let rel = format!("03_叙事规划/细纲/{volume}.md");
    let p = lib_root.join(&rel);
    if !p.is_file() {
        bail!("找不到细纲：{rel}");
    }
    let text = std::fs::read_to_string(&p)?;
    let mut out: Vec<Value> = Vec::new();
    let mut cur: Option<(u32, String, usize)> = None; // (章号, 标题, 字数)
    for line in text.lines() {
        let l = line.trim_start();
        if let Some(rest) = l.strip_prefix("### 第") {
            if let Some(c) = cur.take() {
                out.push(json!({"no": c.0, "title": c.1, "chars": c.2}));
            }
            // 解析「N章 标题」
            let (num_s, title) = match rest.split_once('章') {
                Some((n, t)) => (n.trim().to_string(), t.trim().to_string()),
                None => (rest.trim().to_string(), String::new()),
            };
            let num = num_s.parse::<u32>().unwrap_or(0);
            cur = Some((num, title, 0));
        } else if let Some(c) = cur.as_mut() {
            c.2 += line.chars().count() + 1;
        }
    }
    if let Some(c) = cur.take() {
        out.push(json!({"no": c.0, "title": c.1, "chars": c.2}));
    }
    let total_chars = text.chars().count();
    Ok(json!({
        "command": "webui pipeline l3 chapters",
        "volume": volume, "file": rel,
        "count": out.len(),
        "total_chars": total_chars,
        "chapters": out,
    }))
}

/// 读单章细纲（### 第N章 到下一 ### 之间）。
pub fn read_chapter(lib_root: &Path, volume: &str, no: u32) -> Result<Value> {
    let rel = format!("03_叙事规划/细纲/{volume}.md");
    let p = lib_root.join(&rel);
    if !p.is_file() {
        bail!("找不到细纲：{rel}");
    }
    let text = std::fs::read_to_string(&p)?;
    let head = format!("### 第{no}章");
    let Some(start) = text.find(&head) else {
        bail!("{volume} 里找不到第{no}章");
    };
    let after = &text[start..];
    let end_rel = after[head.len()..].find("\n### 第").map(|i| i + head.len() + 1);
    let body = match end_rel {
        Some(e) => &after[..e],
        None => after,
    };
    Ok(json!({
        "command": "webui pipeline l3 chapter read",
        "volume": volume, "chapter": no, "file": rel,
        "chars": body.chars().count(),
        "content": body,
    }))
}

/// 写单章细纲（替换该章段落；写前快照）。
pub fn write_chapter(
    conn: &Connection, lib_root: &Path, volume: &str, no: u32, body: &str,
) -> Result<Value> {
    let rel = format!("03_叙事规划/细纲/{volume}.md");
    let p = lib_root.join(&rel);
    if !p.is_file() {
        bail!("找不到细纲：{rel}");
    }
    let v: Value = serde_json::from_str(body.trim()).unwrap_or(json!({}));
    let Some(new_body) = v["content"].as_str() else {
        bail!("需要 content 字段");
    };
    if new_body.trim().is_empty() {
        bail!("content 不能为空");
    }
    if !new_body.trim_start().starts_with("### 第") {
        bail!("content 必须以「### 第N章」开头（保持格式）");
    }
    let text = std::fs::read_to_string(&p)?;
    let head = format!("### 第{no}章");
    let Some(start) = text.find(&head) else {
        bail!("{volume} 里找不到第{no}章");
    };
    let after = &text[start..];
    let end_rel = after[head.len()..].find("\n### 第").map(|i| i + head.len() + 1);
    let end = match end_rel { Some(e) => start + e, None => text.len() };
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..start]);
    out.push_str(new_body.trim_end());
    out.push('\n');
    if end < text.len() {
        out.push('\n');
        out.push_str(&text[end..]);
    }
    // 写前快照
    let snap = match super::diff_snapshot::create(
        conn, lib_root, &format!("webui-l3-{volume}-ch{no}")) {
        Ok((id, _)) => id,
        Err(e) => bail!("写前快照失败，已中止：{e}"),
    };
    let bak = p.with_extension("md.bak");
    std::fs::copy(&p, &bak)?;
    std::fs::write(&p, &out)?;
    let chars = new_body.chars().count();
    category_store::log_op(conn, "l3.chapter.write", &rel,
        &format!("ch={no} chars={chars}"), &snap)?;
    Ok(json!({"ok": true, "volume": volume, "chapter": no,
              "chars": chars, "snapshot_id": snap}))
}
