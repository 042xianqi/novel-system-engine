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
    // 2026-10-17 L4 修复：补查 category 字段（前端分类标签改造为读真实分类，不再靠 key 前缀硬编码猜）。
    let mut sql = String::from(
        "SELECT layer,key,IFNULL(title,''),chars,dirty,IFNULL(category,'') FROM library_index",
    );
    if layer.is_some() {
        sql.push_str(" WHERE layer=?1");
    }
    sql.push_str(" ORDER BY layer,key");
    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<(String, String, String, i64, i64, String)> = if let Some(l) = layer {
        stmt.query_map(rusqlite::params![l], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })?
        .filter_map(|x| x.ok()).collect()
    } else {
        stmt.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })?
        .filter_map(|x| x.ok()).collect()
    };
    let mut items: Vec<Value> = Vec::new();
    // [*] 注解一次性取全量。原实现放在 for 循环内，每行都跑一次
    //     all_annotations()（内含两次全表查询 + ensure_tables），
    //     L4 有 701 行 → 1400+ 次冗余查询，是本接口最大热点。
    let ann = note_tag_store::all_annotations(conn).unwrap_or_default();
    for (layer, key, title, chars, dirty, category) in rows {
        if let Some(set) = &tag_set {
            if !set.contains(&key) {
                continue;
            }
        }
        let tags = ann.get(&key).map(|a| a["tags"].clone()).unwrap_or(json!([]));
        let note_status = ann
            .get(&key)
            .and_then(|a| a["note"]["status"].as_str().map(|s| s.to_string()));
        items.push(json!({
            "layer": layer, "key": key, "title": title, "chars": chars,
            "category": category, "dirty": dirty, "tags": tags, "note_status": note_status,
        }));
    }
    // [*] 先全量计数再截断。原实现边 push 边 break，导致：
    //     1) 调用方无法知道「真实命中数」，被截断也看不出来（前端曾因此静默丢 201 张卡）
    //     2) matched 若在 break 后统计必然偏小。这里改为构造完再 truncate，两个数都准。
    let matched = items.len();
    items.truncate(limit);
    Ok(json!({
        "command": "world-gov web browse",
        "layer": layer, "with_tag": with_tag,
        "count": items.len(), "matched": matched,
        "truncated": matched > items.len(), "limit": limit,
        "items": items,
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
    /// 指定状态码的 JSON 响应（U-04：任务不存在应返 404，不能 200 带 error 字段）。
    fn json_status(code: u16, v: &Value) -> Self {
        Resp {
            status: code,
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
    /// 任意 content-type + 原始字节（静态资源托管用）。
    fn bytes(ctype: &'static str, body: Vec<u8>) -> Self {
        Resp { status: 200, ctype, body }
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
        // ── 首页优先级（2026-10-17 前端工程化）──
        // 1) frontend/dist 存在 → 发新前端（Vue 工程产物）
        // 2) 不存在 → 回落旧内联面板（零回归，`npm` 没装也能用）
        "/" | "/index.html" => dist_index().unwrap_or_else(|| Resp::html(WEBUI_PANEL_HTML)),
        // 旧单文件面板永久保留在 /legacy（对照 / 回滚用）
        "/legacy" | "/legacy.html" | "/old" | "/old.html" => Resp::html(WEBUI_PANEL_HTML),
        // 更早的 P0 只读快照
        "/new" | "/new.html" => Resp::html(PANEL_HTML),
        "/api/overview" => match overview(conn, lib_root) {
            Ok(v) => Resp::json(&v),
            Err(e) => Resp::json(&json!({"error": e.to_string()})),
        },
        "/api/items" => {
            let layer = get("layer");
            let tag = get("tag");
            // 2026-10-17 L4 修复：支持 all=true 全量返回；limit 上限从 500 放宽到 5000。
            let all_flag = get("all").map(|s| s == "true" || s == "1").unwrap_or(false);
            let limit = if all_flag {
                usize::MAX
            } else {
                get("limit").and_then(|s| s.parse::<usize>().ok()).unwrap_or(50).min(5000)
            };
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
            let limit = get("limit").and_then(|s| s.parse::<usize>().ok()).unwrap_or(100).min(2000);
            match browse_items(conn, layer.as_deref(), tag.as_deref(), limit) {
                Ok(v) => Resp::json(&v),
                Err(e) => Resp::json(&json!({"error": e.to_string()})),
            }
        }
        "/api/config/llm" => match llm_config_view(lib_root) {
            Ok(v) => Resp::json(&v),
            Err(e) => Resp::json(&json!({"error": e.to_string()})),
        },
        // 静态资源（/assets/xxx.js、favicon 等）：先查 dist 再 404。
        // 注意顺序——必须在 404 之前，且不拦 /api（/api 已由 route_dyn 处理完）。
        _ => read_dist_file(path).unwrap_or_else(Resp::not_found),
    }
}


/// 动态路由：处理带路径参数的接口（`/api/layers/{layer}/items` 等）。
/// 返回 `None` 表示「不归我管」，交回静态路由表。
///
/// # 方法策略（用户 2026-10-16 拍板「混合 C」）
/// - 分类 / 标签 / 备注 등**纯运维元数据** → 直接开写（低危，不碰管线语义）
/// - 分层条目内容（落磁盘文件、会进注入面）→ **不在 P2a**，P2b 做（写前强制快照）
/// - LLM 推演 / 管线运算 → 不在本层，走任务（P4）
///
/// 把一轮对话（user + assistant）落进会话库。
///
/// # parent 为什么必须接上 head
///
/// `SessionStore` 是**会话树**结构：`history()` 从 `sessions.head_id` 出发，
/// 沿每条消息的 `parent_id` 往上回溯。所以每次 append 都必须显式指定
/// 前驱消息，否则链断开，`history()` 只能读到断点之后的那条，
/// 表现为"历史莫名其妙丢了"。
///
/// 正确做法：先读当前 head 作为 user 消息的 parent，再用 user 消息的 id
/// 作为 assistant 的 parent，形成 `... → head → user → assistant` 的新链。
///
/// # 只落 user + assistant，不落 tool
///
/// 工具中间态（assistant.tool_calls / role:"tool"）不落库。
/// 原因：回填时若把 `role:"tool"` 塞回 messages，OpenAI 协议要求它必须
/// 紧跟对应的 `assistant.tool_calls` 且 `tool_call_id` 对得上，
/// 而历史里没保存 tool_call_id，硬塞必然 400。只留最终答复最安全。
fn persist_turn(
    lib_root: &Path,
    session: &str,
    user_msg: &str,
    reply_json: &Value,
) -> anyhow::Result<()> {
    let store = super::open_session(lib_root)?;
    super::ensure_session(&store, session);

    // 取 assistant 的正文。前端显示的是 reply 字段；
    // 若为空的（极端情况），退化成整段 JSON，至少留痕。
    let assistant_text = reply_json
        .get("reply")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim();
    if assistant_text.is_empty() {
        return Ok(());
    }

    // 当前 head：新 user 消息挂它后面
    let head = store.head(session).ok().flatten();
    let uid = store.append(session, head, "user", user_msg)?;
    store.append(session, Some(uid), "assistant", assistant_text)?;
    Ok(())
}

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
        // ══════ 对话中枢（L1 规则层）
        //
        // POST /api/chat  {"message":"...","session":"default"}
        // → {ok, reply, matched, plan, task_ids, needs_unlock, hint}
        //
        // 这一层**不调 LLM**，纯正则解析（<1ms）。契约与将来的 LLM 层一致，
        // 所以前端永远不用改。真正的执行仍走 webui_task + 执行器。
        ("chat", 2) if method == "POST" => {
            let v: Value = serde_json::from_str(body.trim()).unwrap_or(json!({}));
            let msg = v["message"].as_str().unwrap_or("");
            let session = v["session"]
                .as_str()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .unwrap_or(super::DEFAULT_SESSION)
                .to_string();

            // 注意：route_dyn 的每个 arm 返回 Result<Value>，由末尾 Some(ok(r)) 统一包成 Resp。
            // 所以这里**不能**直接构 Resp（那是静态路由表的写法）。
            let out = super::chat_intent::handle(conn, lib_root, msg, &session).unwrap_or_else(|e| {
                json!({
                    "ok": false,
                    "reply": format!("内部错误：{e}"),
                    "matched": "error",
                    "plan": [], "task_ids": [], "needs_unlock": false,
                    "hint": null,
                })
            });

            // ── 落库：user + assistant 两条 ──
            //
            // 【为什么落库放在这里，而不是 chat_intent::handle 里面】
            //
            // `chat_llm.rs:350/355` 会在 L2 内部**递归调用 handle()**，
            // 传入的合成消息是「看当前任务状态」「跑一次系统自检」——
            // 那是系统在借用 L1 的实现，不是用户说的话。
            // 若落库写在 handle 里，这些合成句子会被当成用户发言写进历史，
            // 下一轮回填时把脏上下文喂回模型（模型会疑惑"我什么时候说过这句"）。
            //
            // 而 `route_dyn` 是**唯一的用户请求入口**，只在这里落库
            // 就能保证"落进去的都是用户真说过的"。子调用不经过这里，天然干净。
            //
            // 【失败不阻断】会话库写失败不该让用户拿不到回复。
            // 最多是这轮没记住，下一轮少一条上下文——比整个报错好。
            if !msg.trim().is_empty() {
                if let Err(e) = persist_turn(lib_root, &session, msg, &out) {
                    eprintln!("[chat] 落库失败（不影响本次回复）：{e}");
                }
            }

            Ok(out)
        }
        // GET /api/chat/history?session=chat&limit=50 —— 拉历史（前端刷新后恢复会话）
        ("chat", 3) if segs[2] == "history" && method == "GET" => {
            let session = qget("session")
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| super::DEFAULT_SESSION.to_string());
            let limit = qget("limit")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(50)
                .min(500);
            match super::open_session(lib_root) {
                Ok(store) => {
                    super::ensure_session(&store, &session);
                    match store.history(&session) {
                        Ok(h) => {
                            // 只取尾部 limit 条
                            let start = h.len().saturating_sub(limit);
                            let items: Vec<Value> = h[start..]
                                .iter()
                                .map(|m| {
                                    json!({
                                        "role": m.role,
                                        "content": m.content,
                                        "ts": m.created_at,
                                    })
                                })
                                .collect();
                            Ok(json!({
                                "ok": true,
                                "session": session,
                                "count": items.len(),
                                "messages": items,
                            }))
                        }
                        Err(e) => Ok(json!({
                            "ok": false,
                            "error": format!("读历史失败：{e}"),
                            "messages": [],
                        })),
                    }
                }
                Err(e) => Ok(json!({
                    "ok": false,
                    "error": format!("会话库打不开：{e}"),
                    "messages": [],
                })),
            }
        }
        // GET /api/chat/help —— 静态指令清单（前端可预取做提示）
        ("chat", 3) if segs[2] == "help" && method == "GET" => {
            Ok(json!({
                "ok": true,
                "commands": [
                    {"pattern": "生成 N 张<文明>的卡", "desc": "登记建卡任务（预演）", "example": "生成 10 张混沌帝国的卡"},
                    {"pattern": "写第 N 章 / 跑卷0X 第 N 章", "desc": "登记写正文任务（需解锁）", "example": "写第 4 章"},
                    {"pattern": "检查第 N 章的冲突", "desc": "登记一致性检查", "example": "检查第 4 章的冲突"},
                    {"pattern": "看当前任务状态", "desc": "直接读实时状态，不登记任务", "example": "看当前任务状态"},
                    {"pattern": "跑一次系统自检", "desc": "登记系统体检", "example": "跑一次系统自检"},
                    {"pattern": "资料库里有哪些卡片", "desc": "查索引分布", "example": "资料库里有哪些卡片"},
                    {"pattern": "第 N 卷有哪些章节", "desc": "查卷内条目", "example": "第 1 卷有哪些章节"}
                ]
            }))
        }
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
        // 【第1轮修复 B-02】原实现只接受 POST，而前端 `api.system.check()` 用 GET，
        // 实测 GET /api/system/self-check → 404，前端自检按钮点了没反应。
        // 现在 GET/POST 都接受；GET 等价于 fix=false（只体检不改库）。
        ("system", 3) if segs[2] == "self-check" && (method == "GET" || method == "POST") => {
            let fix = if method == "GET" {
                false
            } else {
                serde_json::from_str::<Value>(body.trim()).ok()
                    .and_then(|v| v["fix"].as_bool()).unwrap_or(false)
            };
            super::self_check::run(conn, lib_root, fix, 30)
        }
        // 【第1轮修复 B-03】`unlocked` 字段名极易误读：
        // 它表示「语义门控 gate_l1 && gate_l2」，与真实 LLM 文件锁毫无关系。
        // 实测该字段恒为 false，而 /api/system/llm-lock 同时报 exists=true，
        // 两个接口看起来互相矛盾（用户会以为"锁没生效"）。
        // 现改名 `semantic_gate_unlocked`，并保留 `unlocked` 作兼容字段但加注说明。
        ("system", 3) if segs[2] == "gate" && method == "GET" => {
            let (l1, l2) = super::task_store::gate_state();
            let file_lock = std::path::Path::new("/sdcard/wogov_real_llm.allow").exists();
            Ok(json!({
                "gate_l1": l1,
                "gate_l2": l2,
                // 语义检视通路（world-gov），与出网授权无关
                "semantic_gate_unlocked": l1 && l2,
                // 真实 LLM 出网文件锁（这才是日常说的"解锁"）
                "llm_file_lock_unlocked": file_lock,
                // 兼容旧前端：语义含义，勿当作出网授权
                "unlocked": l1 && l2,
                "_note": "unlocked=语义门控；出网授权请看 llm_file_lock_unlocked",
            }))
        }
        // ══════ 真实 LLM 授权锁（面板开关 · 2026-10-17）══════
        // 这是**文件锁** `/sdcard/wogov_real_llm.allow` 的面板视图，与上面
        // gate_l1/gate_l2（语义门控）是**两套完全不同的东西**，不要混淆：
        //   · gate_l1/l2 管 world-gov 语义检视通路（默认永远关）
        //   · 本锁管 cardgen.py / l5.py 的真实出网（日常操作对象）
        //
        //   GET  /api/system/llm-lock           segs.len=3 → 状态
        //   POST /api/system/llm-lock/unlock    segs.len=4 → 解锁（= touch）
        //   POST /api/system/llm-lock/lock      segs.len=4 → 上锁（= rm）
        //
        // 解锁必须由**人显式点开关**；任务流程只在结束时自动上锁（fail-safe）。
        ("system", 3) if segs[2] == "llm-lock" && method == "GET" => {
            Ok(super::real_llm_lock::status())
        }
        ("system", 4) if segs[2] == "llm-lock" && segs[3] == "unlock" && method == "POST" => {
            super::real_llm_lock::unlock(conn, "webui:unlock-button")
        }
        ("system", 4) if segs[2] == "llm-lock" && segs[3] == "lock" && method == "POST" => {
            super::real_llm_lock::lock(conn, "webui:lock-button")
        }
        ("system", 3) if segs[2] == "llm-lock" && method == "POST" => {
            // 容错：POST /api/system/llm-lock {"action":"unlock"|"lock"}
            let act = serde_json::from_str::<Value>(body.trim()).ok()
                .and_then(|v| v["action"].as_str().map(|x| x.to_string()))
                .unwrap_or_default();
            match act.as_str() {
                "unlock" => super::real_llm_lock::unlock(conn, "webui:unlock-post"),
                "lock" => super::real_llm_lock::lock(conn, "webui:lock-post"),
                _ => return Some(Resp::bad(r#"system/llm-lock: POST body 需 {"action":"unlock"|"lock"}"#)),
            }
        }

        // ══════ P4：LLM 配置写（受控）
        ("config", 3) if segs[2] == "llm" => match method {
            "GET" => llm_config_view(lib_root),
            "PUT" => super::llm_config_write::write_config(conn, lib_root, body),
            _ => return Some(Resp::bad("config/llm: GET / PUT")),
        },
        // ══════ 模型配置（models.yml）—— 2026-10-17 新增
        //
        // 这是一个"真能改"的接口，与上面的 config/llm 完全不同：
        //   · config/llm  → 资料库/config/llm.toml（超时/重试/缓存等治理参数）
        //   · config/model → .neagent/models.yml（model / baseUrl / apiKey，**真正生效的**）
        //
        // 为什么之前"只能看不能改"：providers.rs 只有解析、没有序列化，
        // 物理上无法写 models.yml。本轮补上 to_models_yml() 才有这个接口。
        //
        // GET  → 返回结构化配置，**api_key 只给掩码**
        // POST → 接受明文 api_key 写入；传空或传掩码 = 保留原值（防误覆盖）
        ("config", 3) if segs[2] == "model" => match method {
            "GET" => super::model_config::read(lib_root)
                .map(Ok)
                .unwrap_or_else(|e| Ok(json!({
                    "ok": false,
                    "error": e.to_string(),
                    "hint": "检查 .neagent/models.yml 是否存在且格式正确",
                }))),
            "POST" | "PUT" => super::model_config::write(conn, lib_root, body)
                .map(Ok)
                .unwrap_or_else(|e| Ok(json!({
                    "ok": false,
                    "error": e.to_string(),
                }))),
            _ => return Some(Resp::bad("config/model: GET / POST")),
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
        // ══════ 活跃任务摘要（2026-10-17 UI 重构）══════
        // 供右下角"全局任务胶囊"3 秒轮询。刻意极轻：一次 GROUP BY + 5 条。
        // 不返回全量列表（那会最多 500 条 × 200 字预览，手机扛不住）。
        ("tasks", 3) if segs[2] == "active" && method == "GET" => {
            super::task_store::active_summary(conn)
        }
        // ══════ 派发执行器（2026-10-17 UI 重构 · 修 P0-1）══════
        // 改造前前端点「触发执行」是把任务改成 running，而执行器只扫 pending
        // → 越点越死。现在改成真正 spawn 执行器。
        //
        // ⚠️ serve() 是单线程阻塞循环，这里必须 spawn 不能 output()。
        // ⚠️ 不改任务状态：保持 pending 让执行器扫到。
        ("tasks", 4) if segs[3] == "dispatch" && method == "POST" => {
            // limit 可选：body {"limit": N}，默认 1
            let lim = serde_json::from_str::<Value>(body.trim()).ok()
                .and_then(|v| v["limit"].as_u64())
                .unwrap_or(1) as u32;
            // 【第1轮修复 B-01】原实现不校验任务是否存在：
            // 实测 POST /api/tasks/nonexistent-id/dispatch 返回
            //   {"dispatched": true, "lock_token": "..."}
            // 前端会误以为派发成功，实际什么都没派——连执行器都被拉起来了。
            // 现在先查任务，不存在则明确报错，不启动执行器。
            let task_id = segs[2].to_string();
            let exists = super::task_store::task_exists(conn, &task_id).unwrap_or(false);
            if !exists {
                // route_dyn 返回 Option<Resp>，用 Resp::bad 构造错误响应
                return Some(Resp::bad(&format!("任务不存在：{task_id}（未启动执行器）")));
            }
            let _ = super::category_store::log_op(conn, "task.dispatch", segs[2],
                &format!("limit={lim}"), "");
            super::task_store::dispatch_executor(lib_root, lim)
        }
        // 任务书 #6：失败归档列表
        ("tasks", 3) if segs[2] == "failures" && method == "GET" => {
            super::task_store::failures(conn, limit_of())
        }
        // 失败类型分布报表（dsh 阶段 2-4）
        //   GET /api/tasks/failure-stats          → 全窗口
        //   GET /api/tasks/failure-stats?days=7   → 近 7 天
        // 改造前 502 无法归因，就是因为只有自由文本 reason；
        // 现在一句 GROUP BY code 就能出「这次挂的到底是网关还是 max_tokens」。
        ("tasks", 3) if segs[2] == "failure-stats" && method == "GET" => {
            let days = qget("days").and_then(|x| x.parse::<i64>().ok()).unwrap_or(0);
            super::task_store::failure_stats(conn, days)
        }
        ("tasks", 3) => {
            let tid = segs[2];
            match method {
                // 【第2轮修复 U-04】原实现直接把 `task_store::result()` 的错误当成
                // JSON body 返回，HTTP 状态仍是 200 → 前端 `if (r.ok)` 永远为真，
                // 「任务不存在」被当成一次成功的查询，展示成空白详情。
                // 现在先判存在性，不存在返回 404；存在仍走原逻辑。
                "GET" => {
                    if !super::task_store::task_exists(conn, tid).unwrap_or(false) {
                        return Some(Resp::json_status(
                            404,
                            &json!({"ok": false, "error": format!("任务不存在：{tid}"), "id": tid}),
                        ));
                    }
                    super::task_store::result(conn, tid)
                }
                "DELETE" => super::task_store::cancel(conn, tid),
                _ => return Some(Resp::bad("tasks: GET / POST(submit) / DELETE")),
            }
        },
        ("tasks", 4) if segs[3] == "result" && method == "GET" => {
            // 同上：/result 子路径也要 404 语义，不能 200 带 error 字段。
            if !super::task_store::task_exists(conn, segs[2]).unwrap_or(false) {
                return Some(Resp::json_status(
                    404,
                    &json!({"ok": false, "error": format!("任务不存在：{}", segs[2]), "id": segs[2]}),
                ));
            }
            super::task_store::result(conn, segs[2])
        }
        // 结果回写（2026-10-17 · 路径A）：执行方（ToolPkg/MCP）执行完调用
        ("tasks", 4) if segs[3] == "result" && method == "POST" => {
            super::task_store::update_result(conn, segs[2], body)
        }
        // 细粒度事件推送（2026-10-17 SSE）：exec_pending.py 逐行读 stdout 后转发。
        // 只广播不写库 —— 高频日志写 SQLite 会拖慢建卡主链路。
        ("tasks", 4) if segs[3] == "event" && method == "POST" => {
            super::task_store::push_event(conn, segs[2], body)
        }

        // 思考/正文回放（2026-10-17 全流式）：
        //   GET /api/tasks/{id}/stream             → 该任务最近 50 张
        //   GET /api/tasks/{id}/stream?code=XXX    → 指定卡
        // 数据源是 task_stream 表（cardgen.py 写的全量思考+正文）。
        // 【为什么不走 result】task_store::list() 对 result 做 substr(...,1,2000)，
        // 一张卡上万字必被截断。
        ("tasks", 4) if segs[3] == "stream" && method == "GET" => {
            let code = qget("code").unwrap_or_default();
            super::task_store::list_stream(conn, segs[2], &code)
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
    // ── SSE：任务事件流 ──
    //
    // 必须在 route() 之前拦下来。原因：route 返回 Resp（带 Content-Length，
    // 一次性写完就关），而 SSE 要求 chunked + 长连接 + 不写 Content-Length。
    // 硬塞进 Resp 模型会写死连接，前端只能收到第一批数据。
    //
    // 所以这里单独走一条路径：直接操作 TcpStream，写完响应头后进入循环，
    // 每收到一条广播就 write 一帧。
    if path == "/api/tasks/stream" && method == "GET" {
        sse_task_stream(&mut stream);
        return;
    }
    // 每请求打开连接（无状态；与命令行同源）
    let resp = match crate::governance::open_gov(lib_root) {
        Ok(conn) => route(&conn, lib_root, &method, &path, &query, &body),
        Err(e) => Resp::json(&json!({"error": format!("打开治理库失败: {e}")})),
    };
    let _ = write_resp(&mut stream, &resp);
}

/// SSE 任务事件流。
///
/// # 协议要点（浏览器 EventSource 的硬要求）
/// - `Content-Type: text/event-stream`
/// - **不能有 Content-Length**（有就会在写完后关闭连接）
/// - `Cache-Control: no-cache` + `Connection: keep-alive`
/// - 每帧必须形如 `data: <payload>\n\n`（双换行分隔）
/// - 空闲时定期发注释行 `: ping\n\n` 保活，防止中间层超时断链
///
/// # 退出条件
/// - 订阅的 receiver 被 drop（广播侧发现 send 失败会摘除订阅者）
/// - 写 socket 失败（前端关了页面 / 切了 tab）
/// - 超过 MAX_SSE_SECS 强制回收，防止僵尸连接无限堆积
fn sse_task_stream(stream: &mut TcpStream) {
    // 订阅必须在发头之前完成：首帧要报真实订阅数（含自己），且
    // subscribe() 会把 recent 历史补发进 channel，先订阅才能不漏事件。
    let (rx, sub_id) = super::task_events::subscribe();
    // 【R12】无论从哪条 return/break 退出，都必须注销订阅。
    //   原实现只靠 publish() 里的 retain 间接清理，无事件时永不执行，
    //   实测 curl 断开后 subscribers 计数只增不减（2→3→4→5）。
    sse_stream_body(stream, &rx);
    super::task_events::unsubscribe(sub_id);
    // 注销后打一行（仅 NE_SSE_DEBUG=1 时输出，避免污染生产日志）。
    //   本轮靠这行实测确认了「死订阅是否真的被摘除」：
    //   修复前 0 条，修复后 5 条且 remaining 递减到 0。
    if std::env::var("NE_SSE_DEBUG").ok().as_deref() == Some("1") {
        eprintln!("[sse] unsubscribed id={} remaining={}", sub_id,
                  super::task_events::subscriber_count());
    }
}

/// SSE 主循环（与订阅生命周期绑定）。
fn sse_stream_body(stream: &mut TcpStream, rx: &std::sync::mpsc::Receiver<String>) {
    use std::io::Write;
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::{Duration, Instant};
    const MAX_SSE_SECS: u64 = 3600;      // 单连接最长 1 小时
    const PING_EVERY_SECS: u64 = 15;     // 空闲保活间隔

    let head = "HTTP/1.1 200 OK\r\n\
                Content-Type: text/event-stream; charset=utf-8\r\n\
                Cache-Control: no-cache, no-store\r\n\
                Connection: keep-alive\r\n\
                X-Accel-Buffering: no\r\n\
                Access-Control-Allow-Origin: *\r\n\r\n";
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    let _ = stream.flush();

    // 首帧：立刻告诉前端「连上了」，避免它以为失败而重连
    // 【R12 修复】原为硬编码 `"subscribers":0` —— 看起来像实时监控数字，
    //   实际永远恒为 0，排障时会误判成「订阅者全没了」。
    //   现在接真实计数（订阅者已在上面 subscribe() 注册，故此处含当前连接自身）。
    let n_subs = super::task_events::subscriber_count();
    let hello = format!(
        "data: {{\"type\":\"hello\",\"data\":{{\"subscribers\":{}}}}}\n\n",
        n_subs
    );
    if stream.write_all(hello.as_bytes()).is_err() {
        return;
    }
    let _ = stream.flush();

    let start = Instant::now();
    loop {
        if start.elapsed() > Duration::from_secs(MAX_SSE_SECS) {
            break;
        }
        match rx.recv_timeout(Duration::from_secs(PING_EVERY_SECS)) {
            Ok(frame) => {
                if stream.write_all(frame.as_bytes()).is_err() {
                    break;
                }
                let _ = stream.flush();
            }
            Err(RecvTimeoutError::Timeout) => {
                // 保活注释帧（浏览器会忽略，但能防止 NAT/代理断链）
                if stream.write_all(b": ping\n\n").is_err() {
                    break;
                }
                let _ = stream.flush();
                // 【R12】主动探活：写成功不代表对端还活着。
                //   实测 curl 客户端断开后，向已 RST 的 socket 写 ping
                //   在 Linux 上第一次仍会成功（数据进了内核缓冲），
                //   于是订阅者最长要挂 15s（下一个 ping）才被摘除。
                //   这里用 peek 非阻塞读一次：对端正常关闭时返回 Ok(0)，
                //   RST 时返回错误，两者都说明连接已死 → 立刻退出循环。
                if peer_closed(stream) {
                    break;
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

/// 探测对端是否已断开（SSE 用）。
///
/// 语义：**只在确定断开时返回 true**，拿不准一律返回 false（保守）。
///   - peek 返回 Ok(0)      → 对端正常关闭（FIN），确认断开
///   - peek 返回 Err(WouldBlock) → 无数据，连接还活着（正常情况）
///   - peek 返回其它 Err     → RST 等异常，确认断开
/// 不用 recv 是为了不消费数据（SSE 上行本来就没数据，但 peek 更安全）。
fn peer_closed(stream: &TcpStream) -> bool {
    use std::io::ErrorKind;
    let _ = stream.set_nonblocking(true);
    let mut buf = [0u8; 1];
    let closed = match stream.peek(&mut buf) {
        Ok(0) => true,
        Ok(_) => false,
        Err(ref e) if e.kind() == ErrorKind::WouldBlock => false,
        Err(_) => true,
    };
    let _ = stream.set_nonblocking(false);
    closed
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
    // ⚠【2026-10-17 SSE 重构】必须每连接一线程。
    //
    // 改造前是 `Ok(s) => handle_conn(s, lib_root)` —— 主线程串行处理，
    // 一个请求处理完才 accept 下一个。普通 JSON 请求毫秒级返回无所谓，
    // 但 SSE 是**长连接**（挂着几十分钟不返回），一旦挂上去：
    //   ① 主线程永远卡在这条连接的 recv 上
    //   ② 前面板 HTML/API 全部打不开
    //   ③ 更糟：SSE 线程自己也要读库，和主线程抢 lib_root 必然死锁
    //
    // 所以必须 spawn。lib_root 是 &Path，需要转成 PathBuf 才能 move 进线程。
    let lib_root_owned = lib_root.to_path_buf();
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let root = lib_root_owned.clone();
                std::thread::spawn(move || handle_conn(s, &root));
            }
            Err(_) => continue,
        }
    }
    Ok(())
}

/// 内联面板 HTML（零 CDN、零外网资源）。
const PANEL_HTML: &str = include_str!("web_panel.html");
/// P1 管理台（管线管理 UI）——同为零依赖内联。
const WEBUI_PANEL_HTML: &str = include_str!("webui_panel.html");

// ══════════════════════════════════ 前端工程化：静态资源托管
//
// # 为什么要换掉 include_str!
// 旧模式把 98KB 单文件 HTML 编译进二进制（`include_str!`），改一行 UI 就要
// `cargo build` 重编 + 重启。现在前端独立成 `frontend/` 工程，`npm run build`
// 产出 `dist/`，Rust 侧只负责「把文件读出来发出去」，改 UI 不必再碰 Rust。
//
// # dist 位置怎么找
// 不写死绝对路径（否则 bundle 换机器就废）。按候选列表顺序取**第一个存在**的：
//   1. 环境变量 `NOVEL_FE_DIST`（显式指定，最高优先级）
//   2. 可执行文件旁的常见布局（target/release → 仓库根 → frontend/dist）
//   3. 当前工作目录及其上两级
// 全部找不到 → 返回 None，路由回落到旧内联面板（**零回归**）。

/// 候选 dist 目录（惰性求值一次）。
fn fe_dist_dir() -> Option<&'static std::path::PathBuf> {
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;
    static D: OnceLock<Option<PathBuf>> = OnceLock::new();
    D.get_or_init(|| {
        let mut cands: Vec<PathBuf> = Vec::new();

        // ① 显式指定（最高优先级；测试与换部署位置时用）
        if let Ok(p) = std::env::var("NOVEL_FE_DIST") {
            cands.push(PathBuf::from(p));
        }

        // ② **编译期常量**：最可靠的一条。
        //
        // env!("CARGO_MANIFEST_DIR") 在编译时就展开成
        // /root/novel-system/crates/ne-agent（构建机绝对路径），
        // 于是 ../.. 必然是仓库根，frontend/dist 必然正确。
        //
        // 为什么必须要这条：proot 沙箱里 `current_exe()` 可能返回宿主侧路径
        // （/data/data/.../ubuntu/root/...），导致 exe 相对候选全部落空；
        // 而启动时 cwd 常常是「资料库」（/sdcard/...），与仓库毫无关系。
        // 实测过：只用 exe/cwd 推断时，不设 NOVEL_FE_DIST 就找不到 dist，
        // 面板静默回落到旧版 —— 用户会以为新前端没生效。
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        if let Some(root) = manifest.parent().and_then(|p| p.parent()) {
            cands.push(root.join("frontend/dist"));
        }
        // 兜底：crates/ne-agent 内部的相对位置（万一仓库布局变了）
        cands.push(manifest.join("../../frontend/dist"));

        // ③ exe 相对候选（正常安装布局 / 非 proot 环境）
        if let Ok(exe) = std::env::current_exe() {
            if let Some(d) = exe.parent() {
                cands.push(d.join("../../frontend/dist"));
                cands.push(d.join("../frontend/dist"));
                cands.push(d.join("frontend/dist"));
                // 逐级上溯最多 5 层（防 target/release 层级变化）
                let mut cur = Some(d);
                for _ in 0..5 {
                    cur = cur.and_then(|c| c.parent());
                    match cur {
                        Some(c) => cands.push(c.join("frontend/dist")),
                        None => break,
                    }
                }
            }
        }

        // ④ cwd 相对候选（在仓库根启动时生效）
        if let Ok(cwd) = std::env::current_dir() {
            cands.push(cwd.join("frontend/dist"));
            cands.push(cwd.join("../../frontend/dist"));
            cands.push(cwd.join("../frontend/dist"));
        }

        // 诊断：NOVEL_FE_DEBUG=1 时逐条打印，排障不用猜
        let debug = std::env::var("NOVEL_FE_DEBUG").is_ok();
        let found = cands.iter().find(|p| p.join("index.html").is_file()).cloned();

        if debug {
            eprintln!("[fe_dist] 候选列表（* = 命中）：");
            for c in &cands {
                let hit = c.join("index.html").is_file();
                eprintln!("  {} {}", if hit { "*" } else { " " }, c.display());
            }
            match &found {
                Some(p) => eprintln!("[fe_dist] 使用：{}", p.display()),
                None => eprintln!("[fe_dist] 全部未命中 → 回落旧内联面板"),
            }
        }

        // 求值一次就把结果打一行（不进 debug 也打，只打一次，便于确认版本）
        match &found {
            Some(p) => eprintln!("[web] 前端 dist：{}", p.display()),
            None => eprintln!("[web] 未找到 frontend/dist，使用旧内联面板（/legacy 同款）"),
        }

        found
    })
    .as_ref()
}

/// 前端 dist 是否可用（供 `/api/system/status` 等诊断用）。
pub fn fe_dist_available() -> bool {
    fe_dist_dir().is_some()
}

/// 扩展名 → content-type（只列前端会产出的类型，不做通用 MIME 库）。
fn mime_of(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "map" => "application/json; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// 读 dist 下某个相对路径的文件。
///
/// 安全：路径先规范化，拒绝任何形式的逃逸（`..` 已在 `safe_request_path`
/// 阶段拦掉一层，这里再兜一层——因为静态托管是「把磁盘内容发给客户端」，
/// 一旦逃逸后果比读 DB 严重得多）。
fn read_dist_file(rel: &str) -> Option<Resp> {
    let root = fe_dist_dir()?;
    let rel = rel.trim_start_matches('/');
    if rel.is_empty() {
        return None;
    }
    // 逐段校验，只允许普通文件名
    let mut p = root.clone();
    for seg in rel.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return None;
        }
        if seg.contains('\\') || seg.contains('\0') {
            return None;
        }
        p.push(seg);
    }
    // 规范化后再确认仍在 root 之内（防御符号链接与奇怪编码）
    let canon = p.canonicalize().ok()?;
    let root_canon = root.canonicalize().ok()?;
    if !canon.starts_with(&root_canon) {
        return None;
    }
    let body = std::fs::read(&canon).ok()?;
    Some(Resp::bytes(mime_of(rel), body))
}

/// 首页：有 dist 就发 dist/index.html，否则 None（回落旧面板）。
fn dist_index() -> Option<Resp> {
    read_dist_file("index.html")
}


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
        // 【2026-10-18 修】原断言 body.contains("world-gov")，那是旧内嵌面板
        // （WEBUI_PANEL_HTML）的特征。现在 `/` 优先发 frontend/dist/index.html
        // （Vue 产物，标题是「残响之河 · 世界治理台」，不含 "world-gov"），
        // 旧断言因此必挂 —— 是"前端迁移后测试没跟上"，不是路由坏了。
        // 改为认「页面是个有 html 外壳的文档」这个本质特征，
        // 不绑定具体品牌词，否则每次改标题都要动测试。
        assert!(
            body.contains("<html") || body.contains("<!DOCTYPE") || body.contains("<div id="),
            "首页应返回 HTML 文档，实际前 80 字：{}",
            body.chars().take(80).collect::<String>()
        );
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
            // 【2026-10-18 修】必须带 category 列：browse_items 的 SQL 里
            // 有 IFNULL(category,'')，旧 CREATE TABLE 漏了这一列 →
            // prepare 后按列索引 r.get(5) 失败 → filter_map 把整行静默丢掉
            // → count 变 null。这个失败是"测试表结构落后于实现"，
            // 不是 browse_items 的 bug。
            "CREATE TABLE library_index(layer TEXT,key TEXT,title TEXT,chars INTEGER,source TEXT,src_mtime INTEGER DEFAULT 0,dirty INTEGER DEFAULT 0,category TEXT);",
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