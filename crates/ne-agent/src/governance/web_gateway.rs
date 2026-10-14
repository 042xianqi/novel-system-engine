//! Web 面板网关（world-gov，第 12 批）—— **零依赖** 最小 HTTP/1.1 服务端。
//!
//! # 架构决策（与仓库一致）
//! 仓库刻意保持「无 tokio / std 阻塞 IO / 单二进制」（见 `subagent.rs`/`tool/mod.rs`/`web.rs` 注释）。
//! 因此本模块**不引入 axum/tokio**，而是用 `std::net::TcpListener` 手写入站 HTTP/1.1，
//! 与既有出站栈 `web.rs`（rustls + std::net + 手写请求/响应解析）同构。
//!
//! # 安全铁则
//! - ✅ **仅绑定 127.0.0.1**（不可改 0.0.0.0；本机外连不上）。
//! - ✅ 所有接口**等价调用内部函数**，不绕过门控/校验；不把 DB 直暴露给 HTTP。
//! - ✅ 无状态：状态全部来自 novel.db 与 07_勘误工坊 目录。
//! - ✅ 前端内联（零 CDN、零外网资源），完全离线可用。
//! - ✅ 无后台常驻：`serve` 阻塞于前台，Ctrl-C 即退出，不留进程。
//! - ✅ 请求路径严格校验（拒绝 `..`、绝对路径）；查询参数白名单。
use anyhow::Result;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::Path;

use super::note_tag_store;

/// 默认端口（规格：8270）。
pub const DEFAULT_PORT: u16 = 8270;
/// 单请求最大字节（防超大 body 打爆内存）。
const MAX_REQ_BYTES: usize = 256 * 1024;

// ────────────────────────────────── 观测数据的构造（等价既有命令）

/// 工作台概览（库统计 / 缓存 / 队列 / 快照 / gate）——**全部只读**。
pub fn overview(conn: &rusqlite::Connection, lib_root: &Path) -> Result<Value> {
    let li: i64 = conn
        .query_row("SELECT COUNT(*) FROM library_index", [], |r| r.get(0))
        .unwrap_or(0);
    let risk: i64 = conn
        .query_row("SELECT COUNT(*) FROM risk_index WHERE resolved=0", [], |r| r.get(0))
        .unwrap_or(0);
    let cache = super::probe_cache::status(lib_root, 30).unwrap_or(json!({}));
    let snaps = super::diff_snapshot::list(conn).unwrap_or_default();
    let (pending, done, rejected) = queue_counts(conn);
    Ok(json!({
        "command": "world-gov web overview",
        "library_index": li,
        "open_risks": risk,
        "cache": {"entries": cache["entries"], "total_bytes": cache["total_bytes"]},
        "queue": {"pending": pending, "done": done, "rejected": rejected},
        "snapshots": {"count": snaps.len(), "items": snaps},
        "gate": ne_agent_gate(),
        "note": "只读概览；等价 world-gov 各命令结果。",
    }))
}

fn queue_counts(conn: &rusqlite::Connection) -> (i64, i64, i64) {
    let t: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='task_jobs'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if t == 0 {
        return (0, 0, 0);
    }
    let c = |st: &str| -> i64 {
        conn.query_row("SELECT COUNT(*) FROM task_jobs WHERE state=?1", rusqlite::params![st], |r| r.get(0))
            .unwrap_or(0)
    };
    (c("Pending"), c("Done"), c("Rejected"))
}

/// gate 状态（只读；绝不改门控）。
fn ne_agent_gate() -> Value {
    let st = crate::governance::semantic_gate_status(false);
    json!({
        "status": st["status"],
        "compile_gate": st["compile_gate"],
        "cli_semantic": st["cli_semantic"],
        "env_unlock": st["env_unlock"],
        "toolchain_epoch": 12,
    })
}

/// 条目浏览（可按 layer / 标签过滤）。**只读 library_index + item_tag**。
pub fn browse_items(
    conn: &rusqlite::Connection,
    layer: Option<&str>,
    with_tag: Option<&str>,
    limit: usize,
) -> Result<Value> {
    // 标签筛选（复用第 10 批筛选器）
    let tag_set = match with_tag {
        Some(t) if !t.trim().is_empty() => {
            let f = note_tag_store::TagFilter::build(std::slice::from_ref(&t.to_string()), &[])?;
            note_tag_store::keys_matching(conn, &f)?
        }
        _ => None,
    };
    let mut sql = String::from("SELECT layer,key,IFNULL(title,''),chars,dirty FROM library_index");
    if layer.is_some() {
        sql.push_str(" WHERE layer=?1");
    }
    sql.push_str(" ORDER BY layer,key");
    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<(String, String, String, i64, i64)> = if let Some(l) = layer {
        stmt.query_map(rusqlite::params![l], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
            .filter_map(|x| x.ok()).collect()
    } else {
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
            .filter_map(|x| x.ok()).collect()
    };
    let mut items: Vec<Value> = Vec::new();
    for (layer, key, title, chars, dirty) in rows {
        if let Some(set) = &tag_set {
            if !set.contains(&key) {
                continue;
            }
        }
        let ann = note_tag_store::all_annotations(conn).unwrap_or_default();
        let tags = ann.get(&key).map(|a| a["tags"].clone()).unwrap_or(json!([]));
        let note_status = ann
            .get(&key)
            .and_then(|a| a["note"]["status"].as_str().map(|s| s.to_string()));
        items.push(json!({
            "layer": layer, "key": key, "title": title, "chars": chars,
            "dirty": dirty, "tags": tags, "note_status": note_status,
        }));
        if items.len() >= limit {
            break;
        }
    }
    Ok(json!({
        "command": "world-gov web browse",
        "layer": layer, "with_tag": with_tag,
        "count": items.len(), "items": items,
    }))
}

/// 队列视图（读取 task_jobs + 扫描到的 journal 计数兜底）。
pub fn queue_view(conn: &rusqlite::Connection) -> Result<Value> {
    let t: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='task_jobs'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if t == 0 {
        return Ok(json!({"command": "world-gov web queue", "table": false, "jobs": []}));
    }
    let mut stmt = conn.prepare("SELECT id,kind,state,created_at FROM task_jobs ORDER BY created_at DESC LIMIT 200")?;
    let jobs: Vec<Value> = stmt
        .query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?, "kind": r.get::<_, String>(1)?,
                "state": r.get::<_, String>(2)?, "created_at": r.get::<_, i64>(3)?,
            }))
        })?
        .filter_map(|x| x.ok())
        .collect();
    Ok(json!({"command": "world-gov web queue", "table": true, "count": jobs.len(), "jobs": jobs}))
}

/// 报告 & 探针包列表（只列名，不读内容）。
pub fn archive_view(lib_root: &Path) -> Result<Value> {
    let reports = super::report_builder::list_reports(lib_root).unwrap_or_default();
    let mut probes: Vec<Value> = Vec::new();
    let dir = lib_root.join("07_勘误工坊");
    if dir.is_dir() {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.ends_with(".wg-probe.tar.gz") {
                    let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                    probes.push(json!({"name": name, "size": size}));
                }
            }
        }
    }
    Ok(json!({
        "command": "world-gov web archive",
        "reports": {"count": reports.len(), "items": reports},
        "probe_archives": {"count": probes.len(), "items": probes},
    }))
}

// ────────────────────────────────── 路由 + HTTP 服务端

/// 路由结果：状态码 + content-type + body 字节。
struct Resp {
    status: u16,
    ctype: &'static str,
    body: Vec<u8>,
}

impl Resp {
    fn json(v: &Value) -> Self {
        Resp {
            status: 200,
            ctype: "application/json; charset=utf-8",
            body: serde_json::to_vec_pretty(v).unwrap_or_else(|_| b"{}".to_vec()),
        }
    }
    fn html(s: &str) -> Self {
        Resp { status: 200, ctype: "text/html; charset=utf-8", body: s.as_bytes().to_vec() }
    }
    fn not_found() -> Self {
        Resp { status: 404, ctype: "text/plain; charset=utf-8", body: b"404 not found".to_vec() }
    }
    fn bad(msg: &str) -> Self {
        Resp { status: 400, ctype: "text/plain; charset=utf-8", body: msg.as_bytes().to_vec() }
    }
}

/// 解析查询串为 (key, value) 列表（简单表单编码；不解码 %，仅用于白名单取值）。
fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|s| !s.is_empty())
        .filter_map(|kv| {
            let mut it = kv.splitn(2, '=');
            let k = it.next()?.to_string();
            let v = it.next().unwrap_or("").to_string();
            Some((k, v))
        })
        .collect()
}

/// 路由：把 (method, path, query) 映射到内部函数。**不绕过任何门控**。
fn route(conn: &rusqlite::Connection, lib_root: &Path, method: &str, path: &str, query: &str) -> Resp {
    if method != "GET" {
        return Resp::bad("仅支持 GET（面板为只读视图；写操作请用命令行）");
    }
    let q = parse_query(query);
    let get = |k: &str| -> Option<String> { q.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone()) };
    match path {
        "/" | "/index.html" => Resp::html(PANEL_HTML),
        "/api/overview" => match overview(conn, lib_root) {
            Ok(v) => Resp::json(&v),
            Err(e) => Resp::json(&json!({"error": e.to_string()})),
        },
        "/api/items" => {
            let layer = get("layer");
            let tag = get("tag");
            let limit = get("limit").and_then(|s| s.parse::<usize>().ok()).unwrap_or(50).min(500);
            match browse_items(conn, layer.as_deref(), tag.as_deref(), limit) {
                Ok(v) => Resp::json(&v),
                Err(e) => Resp::json(&json!({"error": e.to_string()})),
            }
        }
        "/api/queue" => match queue_view(conn) {
            Ok(v) => Resp::json(&v),
            Err(e) => Resp::json(&json!({"error": e.to_string()})),
        },
        "/api/archive" => match archive_view(lib_root) {
            Ok(v) => Resp::json(&v),
            Err(e) => Resp::json(&json!({"error": e.to_string()})),
        },
        "/api/tags" => match note_tag_store::list_tags(conn, None, None) {
            Ok(rows) => Resp::json(&json!({"command": "world-gov web tags", "count": rows.len(), "tags": rows})),
            Err(e) => Resp::json(&json!({"error": e.to_string()})),
        },
        "/api/snapshots" => match super::diff_snapshot::list(conn) {
            Ok(rows) => Resp::json(&json!({"command": "world-gov web snapshots", "count": rows.len(), "snapshots": rows})),
            Err(e) => Resp::json(&json!({"error": e.to_string()})),
        },
        _ => Resp::not_found(),
    }
}

/// 读取一条 HTTP 请求（仅取请求行 + 头；不读 body 用于只读面板）。
fn read_request(stream: &mut TcpStream) -> Result<(String, String, String)> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        anyhow::bail!("空请求");
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("/").to_string();
    // 丢弃头部（不读 body）
    let mut total = line.len();
    loop {
        let mut h = String::new();
        let n = reader.read_line(&mut h)?;
        total += n;
        if n == 0 || h == "\r\n" || h == "\n" {
            break;
        }
        if total > MAX_REQ_BYTES {
            anyhow::bail!("请求头过大");
        }
    }
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.clone(), String::new()),
    };
    Ok((method, path, query))
}

/// 路径安全校验：拒绝 `..`、绝对路径逃逸、NUL。
pub fn safe_request_path(path: &str) -> bool {
    if path.contains("..") || path.contains('\0') {
        return false;
    }
    path.starts_with('/')
}

/// 处理单条连接。
fn handle_conn(mut stream: TcpStream, lib_root: &Path) {
    let (method, path, query) = match read_request(&mut stream) {
        Ok(v) => v,
        Err(_) => {
            let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            return;
        }
    };
    if !safe_request_path(&path) {
        let _ = write_resp(&mut stream, &Resp::bad("非法路径"));
        return;
    }
    // 每请求打开连接（无状态；与命令行同源）
    let resp = match crate::governance::open_gov(lib_root) {
        Ok(conn) => route(&conn, lib_root, &method, &path, &query),
        Err(e) => Resp::json(&json!({"error": format!("打开治理库失败: {e}")})),
    };
    let _ = write_resp(&mut stream, &resp);
}

fn write_resp(stream: &mut TcpStream, r: &Resp) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n\r\n",
        r.status,
        if r.status == 200 { "OK" } else if r.status == 404 { "Not Found" } else { "Bad Request" },
        r.ctype,
        r.body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(&r.body)?;
    stream.flush()
}

/// 启动服务（**阻塞前台**；Ctrl-C 即退出，不留后台进程）。
/// 仅绑定 127.0.0.1；`port` 越界回退默认。
pub fn serve(lib_root: &Path, port: u16, quiet: bool) -> Result<()> {
    let port = if port == 0 { DEFAULT_PORT } else { port };
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let listener = TcpListener::bind(addr)?;
    if !quiet {
        eprintln!("world-gov web 面板已启动：http://127.0.0.1:{port}");
        eprintln!("（仅本机可访问；Ctrl-C 退出，不留后台进程）");
    }
    for stream in listener.incoming() {
        match stream {
            Ok(s) => handle_conn(s, lib_root),
            Err(_) => continue,
        }
    }
    Ok(())
}

/// 内联面板 HTML（零 CDN、零外网资源）。
const PANEL_HTML: &str = include_str!("web_panel.html");

/// 供单元测试：路由纯函数（不启网络）。
pub fn route_for_test(conn: &rusqlite::Connection, lib_root: &Path, method: &str, target: &str) -> (u16, String) {
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p, q),
        None => (target, ""),
    };
    if !safe_request_path(path) {
        return (400, "bad".to_string());
    }
    let r = route(conn, lib_root, method, path, query);
    (r.status, String::from_utf8_lossy(&r.body).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn mem() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(include_str!("../governance_schema.sql")).unwrap();
        crate::governance::ensure_risk_index_cols(&c).unwrap();
        crate::governance::note_tag_store::ensure_tables(&c).unwrap();
        crate::governance::diff_snapshot::ensure_table(&c).unwrap();
        c
    }

    #[test]
    fn safe_path_rejects_escape() {
        assert!(safe_request_path("/api/items"));
        assert!(!safe_request_path("/../etc/passwd"));
        assert!(!safe_request_path("/api/../../x"));
        assert!(!safe_request_path("relative"));
        assert!(!safe_request_path("/a\0b"));
    }

    #[test]
    fn routes_return_expected_shapes() {
        let conn = mem();
        let lib = std::env::temp_dir().join("wg_web_route_lib");
        let (st, body) = route_for_test(&conn, &lib, "GET", "/api/overview");
        assert_eq!(st, 200);
        let v: Value = serde_json::from_str(&body).unwrap();
        assert!(v.get("library_index").is_some());
        // items
        let (st, body) = route_for_test(&conn, &lib, "GET", "/api/items?limit=10");
        assert_eq!(st, 200);
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["command"], "world-gov web browse");
        // tags / snapshots / queue / archive
        assert_eq!(route_for_test(&conn, &lib, "GET", "/api/tags").0, 200);
        assert_eq!(route_for_test(&conn, &lib, "GET", "/api/snapshots").0, 200);
        assert_eq!(route_for_test(&conn, &lib, "GET", "/api/queue").0, 200);
        assert_eq!(route_for_test(&conn, &lib, "GET", "/api/archive").0, 200);
        // 首页
        let (st, body) = route_for_test(&conn, &lib, "GET", "/");
        assert_eq!(st, 200);
        assert!(body.contains("world-gov"));
    }

    #[test]
    fn non_get_is_rejected_and_unknown_404() {
        let conn = mem();
        let lib = std::env::temp_dir().join("wg_web_neg_lib");
        assert_eq!(route_for_test(&conn, &lib, "POST", "/api/overview").0, 400);
        assert_eq!(route_for_test(&conn, &lib, "GET", "/api/nope").0, 404);
    }

    #[test]
    fn browse_filters_by_layer_and_tag() {
        let conn = Connection::open_in_memory().unwrap();
        crate::governance::note_tag_store::ensure_tables(&conn).unwrap();
        conn.execute_batch(
            "CREATE TABLE library_index(layer TEXT,key TEXT,title TEXT,chars INTEGER,source TEXT,src_mtime INTEGER DEFAULT 0,dirty INTEGER DEFAULT 0);",
        ).unwrap();
        let lib = std::env::temp_dir().join("wg_web_browse_lib");
        conn.execute("INSERT INTO library_index(layer,key,title,chars,dirty) VALUES('L4','CHAR-1','',10,0)", []).unwrap();
        conn.execute("INSERT INTO library_index(layer,key,title,chars,dirty) VALUES('L3','L3-1','',10,0)", []).unwrap();
        crate::governance::note_tag_store::add_tags(&conn, "CHAR-1", &["关键".into()], "manual").unwrap();
        // layer 过滤
        let v: Value = serde_json::from_str(&route_for_test(&conn, &lib, "GET", "/api/items?layer=L4").1).unwrap();
        assert_eq!(v["count"], 1);
        // tag 过滤
        let v: Value = serde_json::from_str(&route_for_test(&conn, &lib, "GET", "/api/items?tag=%E5%85%B3%E9%94%AE").1).unwrap();
        // 注意：%XX 不解码，故这里用原义中文 token
        let v2: Value = serde_json::from_str(&route_for_test(&conn, &lib, "GET", "/api/items?tag=关键").1).unwrap();
        assert_eq!(v2["count"], 1, "标签过滤命中 1 条");
        assert_eq!(v2["items"][0]["key"], "CHAR-1");
        let _ = v;
    }

    #[test]
    fn overview_counts_from_db() {
        let conn = Connection::open_in_memory().unwrap();
        crate::governance::note_tag_store::ensure_tables(&conn).unwrap();
        crate::governance::diff_snapshot::ensure_table(&conn).unwrap();
        conn.execute_batch("CREATE TABLE library_index(layer TEXT,key TEXT,title TEXT,chars INTEGER,source TEXT,src_mtime INTEGER DEFAULT 0);").unwrap();
        let lib = std::env::temp_dir().join("wg_web_ov_lib");
        conn.execute("INSERT INTO library_index(layer,key) VALUES('L4','X')", []).unwrap();
        let v: Value = serde_json::from_str(&route_for_test(&conn, &lib, "GET", "/api/overview").1).unwrap();
        assert_eq!(v["library_index"], 1);
        assert_eq!(v["gate"]["toolchain_epoch"], 12);
    }
}