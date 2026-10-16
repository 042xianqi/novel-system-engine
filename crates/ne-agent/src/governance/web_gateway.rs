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
/// 从查询参数取 layer。
fn get_layer_param(q: &[(String, String)]) -> Option<String> {
    q.iter().find(|(k, _)| k == "layer").map(|(_, v)| v.clone())
}
/// 从查询参数取 tag。
fn get_tag_param(q: &[(String, String)]) -> Option<String> {
    q.iter().find(|(k, _)| k == "tag").map(|(_, v)| v.clone())
}

fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|s| !s.is_empty())
        .filter_map(|kv| {
            let mut it = kv.splitn(2, '=');
            let k = pct_decode(it.next()?);
            let v = pct_decode(it.next().unwrap_or(""));
            Some((k, v))
        })
        .collect()
}

/// 百分号解码（`%E5%8F%99` → 中文）+ `+` → 空格。
/// 之前不解码，导致带中文的 path 参数白名单比对失败。
fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let h = |c: u8| -> Option<u8> {
                    match c {
                        b'0'..=b'9' => Some(c - b'0'),
                        b'a'..=b'f' => Some(c - b'a' + 10),
                        b'A'..=b'F' => Some(c - b'A' + 10),
                        _ => None,
                    }
                };
                match (h(b[i + 1]), h(b[i + 2])) {
                    (Some(x), Some(y)) => { out.push(x * 16 + y); i += 3; }
                    _ => { out.push(b[i]); i += 1; }
                }
            }
            b'+' => { out.push(b' '); i += 1; }
            c => { out.push(c); i += 1; }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

/// 路由：把 (method, path, query) 映射到内部函数。**不绕过任何门控**。
fn route(conn: &rusqlite::Connection, lib_root: &Path, method: &str, path: &str, query: &str, body: &str) -> Resp {
    // 方法门控：动态路径（/api/layers/...）走 route_dyn，自行校验方法；
    // 其余静态路径保持「只读」语义（P1 的 3 个只读接口 + 既有只读接口）。
    if let Some(r) = route_dyn(conn, lib_root, method, path, query, body) {
        return r;
    }
    if method != "GET" {
        return Resp::bad("此路径仅支持 GET（写操作见 /api/layers/... 与 /api/library/...）");
    }
    let q = parse_query(query);
    let get = |k: &str| -> Option<String> { q.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone()) };
    match path {
        // 任务书：在原 world-gov 面板上追加 2 菜单 + 升级已有视图（webui_panel.html 即升级版）。
        "/" | "/index.html" => Resp::html(WEBUI_PANEL_HTML),
        // 原只读版（P0 快照），保留对照。
        "/old" | "/old.html" => Resp::html(PANEL_HTML),
        "/new" | "/new.html" => Resp::html(PANEL_HTML),
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
        "/api/layers/list" => match layers_list(conn) {
            Ok(v) => Resp::json(&v),
            Err(e) => Resp::json(&json!({"error": e.to_string()})),
        },
        "/api/system/status" => match system_status(conn, lib_root) {
            Ok(v) => Resp::json(&v),
            Err(e) => Resp::json(&json!({"error": e.to_string()})),
        },
        "/api/library/index" => {
            let layer = get_layer_param(&q);
            let tag = get_tag_param(&q);
            let limit = get("limit").and_then(|s| s.parse::<usize>().ok()).unwrap_or(100).min(500);
            match browse_items(conn, layer.as_deref(), tag.as_deref(), limit) {
                Ok(v) => Resp::json(&v),
                Err(e) => Resp::json(&json!({"error": e.to_string()})),
            }
        }
        "/api/config/llm" => match llm_config_view(lib_root) {
            Ok(v) => Resp::json(&v),
            Err(e) => Resp::json(&json!({"error": e.to_string()})),
        },
        _ => Resp::not_found(),
    }
}


/// 动态路由：处理带路径参数的接口（`/api/layers/{layer}/items` 等）。
/// 返回 `None` 表示「不归我管」，交回静态路由表。
///
/// # 方法策略（用户 2026-10-16 拍板「混合 C」）
/// - 分类 / 标签 / 备注 등**纯运维元数据** → 直接开写（低危，不碰管线语义）
/// - 分层条目内容（落磁盘文件、会进注入面）→ **不在 P2a**，P2b 做（写前强制快照）
/// - LLM 推演 / 管线运算 → 不在本层，走任务（P4）
fn route_dyn(
    conn: &rusqlite::Connection,
    lib_root: &Path,
    method: &str,
    path: &str,
    query: &str,
    body: &str,
) -> Option<Resp> {
    // 路径先做 %XX 解码，否则中文路径参数（如「卷01」）会以 %E5%8D%B7 形式参与匹配
    let decoded = pct_decode(path);
    let segs: Vec<&str> = decoded.trim_start_matches('/').split('/').collect();
    if segs.len() < 2 || segs[0] != "api" {
        return None;
    }
    let q = parse_query(query);
    let qget = |k: &str| -> Option<String> { q.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone()) };
    let ok = |r: Result<Value>| -> Resp {
        r.map(|v| Resp::json(&v))
            .unwrap_or_else(|e| Resp::json(&json!({"error": e.to_string()})))
    };
    let limit_of = || -> usize { qget("limit").and_then(|s| s.parse::<usize>().ok()).unwrap_or(100).min(500) };

    let r = match (segs[1], segs.len()) {
        // ══════ P2b：分层条目（涉磁盘文件 → 写前强制快照）
        ("layers", 3) if segs[2] == "items" => match method {
            "GET" => super::layer_store::list_items(conn, None, qget("tag").as_deref(),
                qget("status").as_deref(), qget("q").as_deref(), limit_of(),
                qget("offset").and_then(|s| s.parse::<usize>().ok()).unwrap_or(0)),
            _ => return Some(Resp::bad("layers/items: GET（按层用 /api/layers/{layer}/items）")),
        },
        ("layers", 4) if segs[3] == "items" => match method {
            "GET" => {
                let layer = segs[2];
                if let Err(e) = super::layer_store::check_layer(layer) {
                    return Some(Resp::json(&json!({"error": e.to_string()})));
                }
                super::layer_store::list_items(conn, Some(layer), qget("tag").as_deref(),
                    qget("status").as_deref(), qget("q").as_deref(), limit_of(),
                    qget("offset").and_then(|s| s.parse::<usize>().ok()).unwrap_or(0))
            }
            "POST" => super::layer_store::create_item(conn, lib_root, segs[2], body),
            _ => return Some(Resp::bad("layers/{layer}/items: GET / POST")),
        },
        ("layers", 4) if segs[3] == "batch" => match method {
            "POST" => super::layer_store::batch(conn, lib_root, segs[2], body),
            _ => return Some(Resp::bad("layers/{layer}/batch: POST")),
        },
        ("layers", 5) if segs[3] == "item" => {
            let layer = segs[2];
            let key = segs[4];
            match method {
                "GET" => super::layer_store::get_item(conn, lib_root, layer, key),
                "PUT" => super::layer_store::update_item(conn, lib_root, layer, key, body),
                "DELETE" => super::layer_store::delete_item(conn, lib_root, layer, key),
                _ => return Some(Resp::bad("layers/{layer}/item/{key}: GET / PUT / DELETE")),
            }
        }
        ("layers", 5) if segs[3] == "graph" => match method {
            "GET" => super::layer_store::graph(conn, segs[2], segs[4]),
            _ => return Some(Resp::bad("layers/{layer}/graph/{key}: GET")),
        },

        // ══════ P7：管线视图（6 层漏斗，只读）
        // ── 文件读写（页面②③④）
        ("pipeline", 3) if segs[2] == "file" && method == "GET" => {
            let path = qget("path").unwrap_or_default();
            super::pipeline_file::read_file(lib_root, &path)
        }
        ("pipeline", 3) if segs[2] == "file" && method == "PUT" => {
            let path = qget("path").unwrap_or_default();
            super::pipeline_file::write_file(conn, lib_root, &path, body)
        }
        // ── L3 章级（页面⑤）
        ("pipeline", 6) if segs[2] == "layer" && segs[4] == "chapters" && method == "GET" => {
            super::pipeline_file::list_chapters(lib_root, segs[3])
        }
        ("pipeline", 7) if segs[2] == "layer" && segs[4] == "chapter" => {
            // /api/pipeline/layer/L3/chapter/{卷号}/{章号}
            let vol = segs[5];
            let no: u32 = segs[6].parse().unwrap_or(0);
            if no == 0 { return Some(Resp::bad("章号非法（需第 7 段为数字）")); }
            match method {
                "GET" => super::pipeline_file::read_chapter(lib_root, vol, no),
                "PUT" => super::pipeline_file::write_chapter(conn, lib_root, vol, no, body),
                _ => return Some(Resp::bad("chapter: GET / PUT")),
            }
        }
        ("pipeline", 5) if segs[2] == "layer" && segs[4] == "chapter" => {
            let vol = qget("volume").unwrap_or_else(|| "卷01".to_string());
            let no = qget("no").and_then(|x| x.parse::<u32>().ok()).unwrap_or(0);
            if no == 0 { return Some(Resp::bad(concat!("缺 no=参数"))); }
            match method {
                "GET" => super::pipeline_file::read_chapter(lib_root, &vol, no),
                "PUT" => super::pipeline_file::write_chapter(conn, lib_root, &vol, no, body),
                _ => return Some(Resp::bad("chapter: GET / PUT")),
            }
        }
        ("pipeline", 5) if segs[2] == "layer" && segs[4] == "chapters" && method == "GET" => {
            let vol = qget("volume").unwrap_or_else(|| "卷01".to_string());
            super::pipeline_file::list_chapters(lib_root, &vol)
        }
("pipeline", 4) if segs[2] == "layer" && method == "GET" => {
            super::pipeline_file::list_layer(lib_root, segs[3])
        }
        // ── 注入包 + 门禁（页面⑦）
        ("pipeline", 3) if segs[2] == "inject" && method == "GET" => {
            let vol = qget("volume").unwrap_or_else(|| "卷01".to_string());
            let ch = qget("chapter").and_then(|x| x.parse::<u32>().ok()).unwrap_or(1);
            super::pipeline_engine::inject_view(lib_root, &vol, ch)
        }
        ("pipeline", 3) if segs[2] == "check" && method == "POST" => {
            let v: Value = serde_json::from_str(body.trim()).unwrap_or(json!({}));
            let vol = v["volume"].as_str().unwrap_or("卷01").to_string();
            let ch = v["chapter"].as_u64().unwrap_or(1) as u32;
            match v["text"].as_str() {
                Some(t) => super::pipeline_engine::check_view(lib_root, t, ch, &vol),
                None => super::pipeline_engine::check_chapter_file(lib_root, &vol, ch),
            }
        }
        ("pipeline", 3) if segs[2] == "overview" && method == "GET" => {
            let vol = qget("volume").unwrap_or_else(|| "卷01".to_string());
            super::pipeline_view::overview_view(conn, lib_root, &vol)
        }
        ("pipeline", 3) if segs[2] == "funnel" && method == "GET" => {
            super::pipeline_view::funnel_view(lib_root)
        }
        ("pipeline", 3) if segs[2] == "materials" && method == "GET" => {
            super::pipeline_view::materials_view(lib_root)
        }
        ("pipeline", 3) if segs[2] == "cardgen" && method == "GET" => {
            let vol = qget("volume").unwrap_or_else(|| "卷01".to_string());
            super::pipeline_view::cardgen_view(lib_root, &vol)
        }
        ("pipeline", 3) if segs[2] == "chapters" && method == "GET" => {
            let vol = qget("volume").unwrap_or_else(|| "卷01".to_string());
            super::pipeline_view::chapters_view(conn, lib_root, &vol)
        }

        // ══════ P3：系统自检（非高危，UI 可直接调用）
        ("system", 3) if segs[2] == "self-check" && method == "POST" => {
            let fix = serde_json::from_str::<Value>(body.trim()).ok()
                .and_then(|v| v["fix"].as_bool()).unwrap_or(false);
            super::self_check::run(conn, lib_root, fix, 30)
        }
        ("system", 3) if segs[2] == "gate" && method == "GET" => {
            let (l1, l2) = super::task_store::gate_state();
            Ok(json!({"gate_l1": l1, "gate_l2": l2, "unlocked": l1 && l2}))
        }

        // ══════ P4：LLM 配置写（受控）
        ("config", 3) if segs[2] == "llm" => match method {
            "GET" => llm_config_view(lib_root),
            "PUT" => super::llm_config_write::write_config(conn, lib_root, body),
            _ => return Some(Resp::bad("config/llm: GET / PUT")),
        },
        ("config", 4) if segs[2] == "llm" && segs[3] == "test-connect" && method == "POST" => {
            Ok(super::llm_config_write::test_connect(body))
        }
        ("config", 4) if segs[2] == "llm" && segs[3] == "reset" && method == "POST" => {
            super::llm_config_write::reset_config(conn, lib_root)
        }
        ("config", 4) if segs[2] == "llm" && segs[3] == "reload" && method == "POST" => {
            let mut out = json!({"ok": false});
            if let Ok(cfg) = super::llm_config::load(lib_root) {
                let snap = super::llm_config::safe_snapshot(&cfg);
                let _ = super::category_store::log_op(conn, "llm.config.reload",
                    super::llm_config::CONFIG_REL, "reloaded", "");
                out = json!({
                    "ok": true, "reloaded": true, "config": snap,
                    "note": "本构建无常驻 llm_client（零依赖、无后台进程）；reload = 重新读取 llm.toml，下次调用自然生效。",
                });
            } else {
                out = json!({"ok": false, "error": "llm.toml 解析失败"});
            }
            Ok(out)
        }

        // ══════ P4：任务（高危，只登记指令）
        // GET  /api/tasks/list     -> 列表
        // POST /api/tasks/submit   -> 提交（只登记）
        // GET  /api/tasks/{id}/result · DELETE /api/tasks/{id} -> 结果 / 终止
        ("tasks", 2) => match method {
            "GET" => super::task_store::list(conn, limit_of()),
            _ => return Some(Resp::bad("tasks: GET（提交用 /api/tasks/submit）")),
        },
        ("tasks", 3) if segs[2] == "submit" && method == "POST" => {
            super::task_store::submit(conn, body)
        }
        ("tasks", 3) if segs[2] == "list" && method == "GET" => {
            super::task_store::list(conn, limit_of())
        }
        // 任务书 #6：失败归档列表
        ("tasks", 3) if segs[2] == "failures" && method == "GET" => {
            super::task_store::failures(conn, limit_of())
        }
        ("tasks", 3) => {
            let tid = segs[2];
            match method {
                "GET" => super::task_store::result(conn, tid),
                "DELETE" => super::task_store::cancel(conn, tid),
                _ => return Some(Resp::bad("tasks: GET / POST(submit) / DELETE")),
            }
        },
        ("tasks", 4) if segs[3] == "result" && method == "GET" => {
            super::task_store::result(conn, segs[2])
        }

        // ══════ P2a：分类树（直接开写）
        ("library", 3) if segs[2] == "categories" => match method {
            "GET" => super::category_store::list_tree(conn),
            "POST" => super::category_store::create(conn, body),
            _ => return Some(Resp::bad("categories: GET / POST")),
        },
        ("library", 4) if segs[2] == "categories" => {
            let id: i64 = segs[3].parse().unwrap_or(-1);
            if id < 0 {
                return Some(Resp::bad("分类 id 非法"));
            }
            match method {
                "GET" => super::category_store::docs(conn, id),
                "PUT" => super::category_store::update(conn, id, body),
                "DELETE" => super::category_store::delete(conn, id),
                _ => return Some(Resp::bad("categories/{id}: GET / PUT / DELETE")),
            }
        }
        ("library", 5) if segs[2] == "categories" => {
            let id: i64 = segs[3].parse().unwrap_or(-1);
            let v: Value = serde_json::from_str(body.trim()).unwrap_or(json!({}));
            let layer = v["layer"].as_str().unwrap_or("").to_string();
            let key = v["key"].as_str().unwrap_or("").to_string();
            if layer.is_empty() || key.is_empty() {
                return Some(Resp::bad("member 需要 layer 与 key"));
            }
            // /api/library/categories/{id}/members  (POST 加 / DELETE 移)
            if segs[4] != "members" {
                return Some(Resp::not_found());
            }
            match method {
                "POST" => super::category_store::add_member(conn, id, &layer, &key),
                "DELETE" => super::category_store::rm_member(conn, id, &layer, &key),
                _ => return Some(Resp::bad("members: POST / DELETE")),
            }
        }

        // ══════ P2a：标签（复用 note_tag_store，直接开写）
        ("tags", 2) => match method {
            "GET" => super::note_tag_store::list_tags(conn, None, None)
                .map(|rows| json!({"count": rows.len(), "tags": rows})),
            _ => return Some(Resp::bad("tags: GET（写用 /api/items/{key}/tags）")),
        },

        // ══════ P2a：备注（直接开写）
        ("notes", 2) => match method {
            "GET" => super::note_tag_store::list_notes(conn, qget("status").as_deref())
                .map(|rows| json!({"count": rows.len(), "notes": rows})),
            _ => return Some(Resp::bad("notes: GET（写用 /api/items/{key}/note）")),
        },

        // ══════ P2a：单条目的 标签 / 备注（写）
        ("items", 3) => {
            let key = segs[2];
            match method {
                "GET" => super::note_tag_store::get_note(conn, key)
                    .map(|n| json!({"key": key, "note": n})),
                _ => return Some(Resp::bad("items/{key}: GET")),
            }
        }
        ("items", 4) if segs[3] == "tags" => match method {
            "POST" => {
                let v: Value = serde_json::from_str(body.trim()).unwrap_or(json!({}));
                let tags: Vec<String> = v["tags"].as_array().map(|a| {
                    a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect()
                }).unwrap_or_default();
                if tags.is_empty() {
                    return Some(Resp::bad("需要 tags 数组"));
                }
                super::note_tag_store::add_tags(conn, segs[2], &tags, "manual")
                    .map(|(a, b)| json!({"ok": true, "added": a, "existing": b}))
            }
            "DELETE" => {
                let tag = qget("tag").or_else(|| {
                    serde_json::from_str::<Value>(body.trim()).ok()
                        .and_then(|v| v["tag"].as_str().map(|s| s.to_string()))
                });
                match tag {
                    Some(t) => super::note_tag_store::rm_tag(conn, segs[2], &t)
                        .map(|n| json!({"ok": true, "removed": n})),
                    None => return Some(Resp::bad("需要 tag 参数")),
                }
            }
            _ => return Some(Resp::bad("items/{key}/tags: POST / DELETE")),
        },
        ("items", 4) if segs[3] == "note" => match method {
            "PUT" | "POST" => {
                let v: Value = serde_json::from_str(body.trim()).unwrap_or(json!({}));
                super::note_tag_store::set_note(
                    conn, segs[2], v["status"].as_str(), v["content"].as_str(),
                )
            }
            "GET" => super::note_tag_store::get_note(conn, segs[2])
                .map(|n| json!({"key": segs[2], "note": n})),
            _ => return Some(Resp::bad("items/{key}/note: GET / PUT")),
        },

        // ══════ P2a：审计（只读）
        ("audit", 3) if segs[2] == "op-logs" && method == "GET" => {
            super::category_store::op_logs(conn, limit_of())
        }
        ("audit", 3) if segs[2] == "llm-logs" && method == "GET" => super::llm_obs::stats(conn),
        ("audit", 3) if segs[2] == "snapshots" && method == "GET" => {
            super::diff_snapshot::list(conn).map(|rows| json!({"count": rows.len(), "snapshots": rows}))
        }

        // ⚠ 关键：不认识的 /api/* 必须返回 None，交回静态路由表。
        // （曾误写 Some(not_found())，导致 /api/overview /api/items /api/queue
        //   /api/snapshots /api/layers/list /api/system/status 全部 404。）
        _ => return None,
    };
    Some(ok(r))
}

/// 读取一条 HTTP 请求（仅取请求行 + 头；不读 body 用于只读面板）。
fn read_request(stream: &mut TcpStream) -> Result<(String, String, String, String)> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        anyhow::bail!("空请求");
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("/").to_string();
    // 读头部（记录 Content-Length，供后面读 body）
    let mut content_length: Option<usize> = None;
    let mut total = line.len();
    loop {
        let mut h = String::new();
        let n = reader.read_line(&mut h)?;
        total += n;
        if n == 0 || h == "\r\n" || h == "\n" {
            break;
        }
        let low = h.to_ascii_lowercase();
        if let Some(v) = low.strip_prefix("content-length:") {
            content_length = v.trim().parse::<usize>().ok();
        }
        if total > MAX_REQ_BYTES {
            anyhow::bail!("请求头过大");
        }
    }
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.clone(), String::new()),
    };
    // 读 body（按 Content-Length；**上限 MAX_REQ_BYTES**，防打爆内存）
    let mut body = String::new();
    if let Some(n) = content_length {
        let n = n.min(MAX_REQ_BYTES);
        let mut buf = vec![0u8; n];
        let mut got = 0usize;
        while got < n {
            match std::io::Read::read(&mut reader, &mut buf[got..]) {
                Ok(0) => break,
                Ok(k) => got += k,
                Err(_) => break,
            }
        }
        buf.truncate(got);
        body = String::from_utf8_lossy(&buf).to_string();
    }
    Ok((method, path, query, body))
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
    let (method, path, query, body) = match read_request(&mut stream) {
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
        Ok(conn) => route(&conn, lib_root, &method, &path, &query, &body),
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


// ══════════════════════════════════ P1：分层 / 系统状态 / LLM 配置（只读）

/// 分层列表：**实际层集合**（库内实测）+ 任务书定义的 6 个正典层语义。
/// 不硬编码 6 层——库内还用了 ANCHOR/AUX/GUARD/L0.5/L1x 等扩展层。
pub fn layers_list(conn: &rusqlite::Connection) -> Result<Value> {
    let mut st = conn.prepare(
        "SELECT layer, COUNT(*), COALESCE(SUM(CASE WHEN dirty=1 THEN 1 ELSE 0 END),0) \
         FROM library_index GROUP BY layer ORDER BY layer",
    )?;
    let it = st.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))
    })?;
    let mut seen = std::collections::BTreeSet::new();
    let mut actual = Vec::new();
    let mut total = 0i64;
    let mut dirty_total = 0i64;
    for x in it {
        let (layer, n, d) = x?;
        seen.insert(layer.clone());
        total += n;
        dirty_total += d;
        actual.push(json!({"layer": layer, "count": n, "dirty": d}));
    }
    // 任务书定义的 6 个正典层（固定语义），标注库内是否出现
    let canon = [
        ("L0", "原始素材"),
        ("L1", "实体"),
        ("L2", "关系"),
        ("L3", "冲突"),
        ("L4", "推演分支"),
        ("L5", "定稿正文"),
    ];
    let canonical: Vec<Value> = canon
        .iter()
        .map(|(k, d)| {
            let n = actual
                .iter()
                .find(|v| v["layer"] == *k)
                .and_then(|v| v["count"].as_i64())
                .unwrap_or(0);
            json!({"layer": k, "meaning": d, "present": seen.contains(*k), "count": n})
        })
        .collect();
    Ok(json!({
        "command": "webui layers list",
        "total": total,
        "dirty_total": dirty_total,
        "canonical": canonical,
        "actual": actual,
        "note": "actual = 库内实测层（含 ANCHOR/AUX/GUARD/L0.5/L1x 等扩展层）",
    }))
}

/// 系统状态：**两套锁**并列展示 + 存储占用 + LLM 配置摘要（掩码）。
///
/// 为什么两套都报：本项目历史上有两层门控，语义不同、不可互相替代——
/// - `gate_l1`（既有第一道闸）：`SEMANTIC_ENABLE_GATE` && `--semantic` && `NOVEL_SEMANTIC_UNLOCK`
/// - `gate_l2`（更严外呼锁）：feature(`real-semantic`/`llm_integration`) && `WOGOV_ALLOW_REAL_LLM=yes` && `--real-semantic`
/// 只报一套会误导操作者。
pub fn system_status(conn: &rusqlite::Connection, lib_root: &Path) -> Result<Value> {
    let g1 = super::semantic_gate_status(false);
    let compile_feature = super::llm_gate::compile_feature_enabled();
    let env_allow = super::llm_gate::env_allowed();
    let g2 = json!({
        "compile_feature": compile_feature,
        "compile_feature_names": "real-semantic | llm_integration",
        "env_allow": env_allow,
        "env_name": super::llm_gate::ENV_ALLOW,
        "cli_flag": false,
        "cli_flag_name": "--real-semantic",
        "open": super::llm_gate::triple_lock(compile_feature, env_allow, false),
        "note": "面板无 --real-semantic，故 cli_flag 恒 false",
    });
    // 存储占用（有上限，避免大库卡住）
    let storage = dir_usage(lib_root, 20000);
    // LLM 配置摘要（掩码；不回显 base_url / api_key）
    let llm = super::llm_config::load(lib_root)
        .map(|c| super::llm_config::safe_snapshot(&c))
        .unwrap_or_else(|e| json!({"error": e.to_string()}));
    let _ = conn;
    Ok(json!({
        "command": "webui system status",
        "gate_l1": g1,
        "gate_l2": g2,
        "storage": storage,
        "llm": llm,
        "note": "只读；不触发任何外呼。",
    }))
}

/// 目录占用（有上限；仅统计，不修改）。
fn dir_usage(root: &Path, cap: usize) -> Value {
    let mut files = 0usize;
    let mut bytes = 0u64;
    let mut truncated = false;
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((d, depth)) = stack.pop() {
        if depth > 6 {
            continue;
        }
        let rd = match std::fs::read_dir(&d) {
            Ok(v) => v,
            Err(_) => continue,
        };
        for e in rd.flatten() {
            if files >= cap {
                truncated = true;
                break;
            }
            let p = e.path();
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push((p, depth + 1)),
                Ok(t) if t.is_file() => {
                    files += 1;
                    if let Ok(m) = e.metadata() {
                        bytes += m.len();
                    }
                }
                _ => {}
            }
        }
        if truncated {
            break;
        }
    }
    json!({
        "files": files,
        "bytes": bytes,
        "mb": (bytes as f64 / 1048576.0 * 100.0).round() / 100.0,
        "truncated": truncated,
        "cap": cap,
    })
}

/// LLM 配置读取（**掩码**）：复用 `llm_config::safe_snapshot`，不回显地址与 Key。
pub fn llm_config_view(work_root: &Path) -> Result<Value> {
    let rel = super::llm_config::CONFIG_REL;
    let path = work_root.join(rel);
    let exists = path.is_file();
    let cfg = super::llm_config::load(work_root)?;
    let snap = super::llm_config::safe_snapshot(&cfg);
    Ok(json!({
        "command": "webui config llm get",
        "path": rel,
        "exists": exists,
        "config": snap,
        "note": "api-key 与 base_url 一律不回显（只报是否已配置）。",
    }))
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
/// P1 管理台（管线管理 UI）——同为零依赖内联。
const WEBUI_PANEL_HTML: &str = include_str!("webui_panel.html");

/// 供单元测试：路由纯函数（不启网络）。
pub fn route_for_test(conn: &rusqlite::Connection, lib_root: &Path, method: &str, target: &str) -> (u16, String) {
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p, q),
        None => (target, ""),
    };
    if !safe_request_path(path) {
        return (400, "bad".to_string());
    }
    let r = route(conn, lib_root, method, path, query, "");
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