//! HTTP 路由层（自 `web_gateway.rs` 拆出，Phase-1 架构重构）。
//!
//! # 职责边界
//! 只做**路径/查询参数解析与分发**：把请求映射到 `gov_service` 查询，产出 `Resp`。
//! **不持有 TcpStream**（传输归 `transport`）。
//!
//! # 兼容性
//! `route_for_test` 签名与原实现完全一致，保留为对外测试入口。
use anyhow::Result;
use serde_json::{json, Value};
use super::super::note_tag_store;
use std::path::Path;

use super::gov_service::*;
use super::transport::{read_dist_file, dist_index, PANEL_HTML, WEBUI_PANEL_HTML};

// ────────────────────────────────── 路由 + HTTP 服务端

/// 路由结果：状态码 + content-type + body 字节。
pub(crate) struct Resp {
    pub(crate) status: u16,
    pub(crate) ctype: &'static str,
    pub(crate) body: Vec<u8>,
}

impl Resp {
    pub(crate) fn json(v: &Value) -> Self {
        Resp {
            status: 200,
            ctype: "application/json; charset=utf-8",
            body: serde_json::to_vec_pretty(v).unwrap_or_else(|_| b"{}".to_vec()),
        }
    }
    /// 指定状态码的 JSON 响应（U-04：任务不存在应返 404，不能 200 带 error 字段）。
    pub(crate) fn json_status(code: u16, v: &Value) -> Self {
        Resp {
            status: code,
            ctype: "application/json; charset=utf-8",
            body: serde_json::to_vec_pretty(v).unwrap_or_else(|_| b"{}".to_vec()),
        }
    }
    pub(crate) fn html(s: &str) -> Self {
        Resp { status: 200, ctype: "text/html; charset=utf-8", body: s.as_bytes().to_vec() }
    }
    pub(crate) fn not_found() -> Self {
        Resp { status: 404, ctype: "text/plain; charset=utf-8", body: b"404 not found".to_vec() }
    }
    pub(crate) fn bad(msg: &str) -> Self {
        Resp { status: 400, ctype: "text/plain; charset=utf-8", body: msg.as_bytes().to_vec() }
    }
    /// 任意 content-type + 原始字节（静态资源托管用）。
    pub(crate) fn bytes(ctype: &'static str, body: Vec<u8>) -> Self {
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

/// 单轮对话：解析请求 → L1 产出回复 → 落库。返回完整回复 JSON。
///
/// 【Phase-1 抽取】原来内联在 route_dyn 的 chat arm 里。现在
/// `?stream=1` 的 SSE 路径（transport::sse_chat_stream）也要走同一逻辑，
/// 抽出来保证「落库只有这一处」，两种传输方式行为 100% 一致。
///
/// 【为什么落库放在这里，而不是 chat_intent::handle 里面】
/// `chat_llm.rs` 会在 L2 内部**递归调用 handle()**，传入的合成消息是
/// 系统在借用 L1 的实现，不是用户说的话；只在本函数（真正的用户请求
/// 入口）落库，才能保证"落进去的都是用户真说过的"。
/// 【失败不阻断】会话库写失败不让用户拿不到回复。
pub(crate) fn chat_once(
    conn: &rusqlite::Connection,
    lib_root: &Path,
    body: &str,
) -> Result<Value> {
    let v: Value = serde_json::from_str(body.trim()).unwrap_or(json!({}));
    let msg = v["message"].as_str().unwrap_or("");
    let session = v["session"]
        .as_str()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or(super::super::DEFAULT_SESSION)
        .to_string();

    let out = super::super::chat_intent::handle(conn, lib_root, msg, &session).unwrap_or_else(|e| {
        json!({
            "ok": false,
            "reply": format!("内部错误：{e}"),
            "matched": "error",
            "plan": [], "task_ids": [], "needs_unlock": false,
            "hint": null,
        })
    });

    if !msg.trim().is_empty() {
        if let Err(e) = persist_turn(lib_root, &session, msg, &out) {
            eprintln!("[chat] 落库失败（不影响本次回复）：{e}");
        }
    }
    Ok(out)
}

/// 路由：把 (method, path, query) 映射到内部函数。**不绕过任何门控**。
pub(crate) fn route(conn: &rusqlite::Connection, lib_root: &Path, method: &str, path: &str, query: &str, body: &str) -> Resp {
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
        "/api/snapshots" => match super::super::diff_snapshot::list(conn) {
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
        // 【任务一·星图】实体关系网：一次性吐 nodes + edges（374 边规模，
        // 直查 entity_edge + library_index 补标题，无分页必要）。
        // 只读探针，绝不写库（gov 铁则）。
        "/api/graph/entity" => match super::super::rel_graph::entity_graph(conn) {
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

pub(crate) fn route_dyn(
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
        ("chat", 2) if method == "POST" => chat_once(conn, lib_root, body),
        // GET /api/chat/history?session=chat&limit=50 —— 拉历史（前端刷新后恢复会话）
        ("chat", 3) if segs[2] == "history" && method == "GET" => {
            let session = qget("session")
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| super::super::DEFAULT_SESSION.to_string());
            let limit = qget("limit")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(50)
                .min(500);
            match super::super::open_session(lib_root) {
                Ok(store) => {
                    super::super::ensure_session(&store, &session);
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
            "GET" => super::super::layer_store::list_items(conn, None, qget("tag").as_deref(),
                qget("status").as_deref(), qget("q").as_deref(), limit_of(),
                qget("offset").and_then(|s| s.parse::<usize>().ok()).unwrap_or(0)),
            _ => return Some(Resp::bad("layers/items: GET（按层用 /api/layers/{layer}/items）")),
        },
        ("layers", 4) if segs[3] == "items" => match method {
            "GET" => {
                let layer = segs[2];
                if let Err(e) = super::super::layer_store::check_layer(layer) {
                    return Some(Resp::json(&json!({"error": e.to_string()})));
                }
                super::super::layer_store::list_items(conn, Some(layer), qget("tag").as_deref(),
                    qget("status").as_deref(), qget("q").as_deref(), limit_of(),
                    qget("offset").and_then(|s| s.parse::<usize>().ok()).unwrap_or(0))
            }
            "POST" => super::super::layer_store::create_item(conn, lib_root, segs[2], body),
            _ => return Some(Resp::bad("layers/{layer}/items: GET / POST")),
        },
        ("layers", 4) if segs[3] == "batch" => match method {
            "POST" => super::super::layer_store::batch(conn, lib_root, segs[2], body),
            _ => return Some(Resp::bad("layers/{layer}/batch: POST")),
        },
        ("layers", 5) if segs[3] == "item" => {
            let layer = segs[2];
            let key = segs[4];
            match method {
                "GET" => super::super::layer_store::get_item(conn, lib_root, layer, key),
                "PUT" => super::super::layer_store::update_item(conn, lib_root, layer, key, body),
                "DELETE" => super::super::layer_store::delete_item(conn, lib_root, layer, key),
                _ => return Some(Resp::bad("layers/{layer}/item/{key}: GET / PUT / DELETE")),
            }
        }
        ("layers", 5) if segs[3] == "graph" => match method {
            "GET" => super::super::layer_store::graph(conn, segs[2], segs[4]),
            _ => return Some(Resp::bad("layers/{layer}/graph/{key}: GET")),
        },

        // ══════ P7：管线视图（6 层漏斗，只读）
        // ── 文件读写（页面②③④）
        ("pipeline", 3) if segs[2] == "file" && method == "GET" => {
            let path = qget("path").unwrap_or_default();
            super::super::pipeline_file::read_file(lib_root, &path)
        }
        ("pipeline", 3) if segs[2] == "file" && method == "PUT" => {
            let path = qget("path").unwrap_or_default();
            super::super::pipeline_file::write_file(conn, lib_root, &path, body)
        }
        // ── L3 章级（页面⑤）
        ("pipeline", 6) if segs[2] == "layer" && segs[4] == "chapters" && method == "GET" => {
            super::super::pipeline_file::list_chapters(lib_root, segs[3])
        }
        ("pipeline", 7) if segs[2] == "layer" && segs[4] == "chapter" => {
            // /api/pipeline/layer/L3/chapter/{卷号}/{章号}
            let vol = segs[5];
            let no: u32 = segs[6].parse().unwrap_or(0);
            if no == 0 { return Some(Resp::bad("章号非法（需第 7 段为数字）")); }
            match method {
                "GET" => super::super::pipeline_file::read_chapter(lib_root, vol, no),
                "PUT" => super::super::pipeline_file::write_chapter(conn, lib_root, vol, no, body),
                _ => return Some(Resp::bad("chapter: GET / PUT")),
            }
        }
        ("pipeline", 5) if segs[2] == "layer" && segs[4] == "chapter" => {
            let vol = qget("volume").unwrap_or_else(|| "卷01".to_string());
            let no = qget("no").and_then(|x| x.parse::<u32>().ok()).unwrap_or(0);
            if no == 0 { return Some(Resp::bad(concat!("缺 no=参数"))); }
            match method {
                "GET" => super::super::pipeline_file::read_chapter(lib_root, &vol, no),
                "PUT" => super::super::pipeline_file::write_chapter(conn, lib_root, &vol, no, body),
                _ => return Some(Resp::bad("chapter: GET / PUT")),
            }
        }
        ("pipeline", 5) if segs[2] == "layer" && segs[4] == "chapters" && method == "GET" => {
            let vol = qget("volume").unwrap_or_else(|| "卷01".to_string());
            super::super::pipeline_file::list_chapters(lib_root, &vol)
        }
("pipeline", 4) if segs[2] == "layer" && method == "GET" => {
            super::super::pipeline_file::list_layer(lib_root, segs[3])
        }
        // ── 注入包 + 门禁（页面⑦）
        ("pipeline", 3) if segs[2] == "inject" && method == "GET" => {
            let vol = qget("volume").unwrap_or_else(|| "卷01".to_string());
            let ch = qget("chapter").and_then(|x| x.parse::<u32>().ok()).unwrap_or(1);
            super::super::pipeline_engine::inject_view(lib_root, &vol, ch)
        }
        ("pipeline", 3) if segs[2] == "check" && method == "POST" => {
            let v: Value = serde_json::from_str(body.trim()).unwrap_or(json!({}));
            let vol = v["volume"].as_str().unwrap_or("卷01").to_string();
            let ch = v["chapter"].as_u64().unwrap_or(1) as u32;
            match v["text"].as_str() {
                Some(t) => super::super::pipeline_engine::check_view(lib_root, t, ch, &vol),
                None => super::super::pipeline_engine::check_chapter_file(lib_root, &vol, ch),
            }
        }
        ("pipeline", 3) if segs[2] == "overview" && method == "GET" => {
            let vol = qget("volume").unwrap_or_else(|| "卷01".to_string());
            super::super::pipeline_view::overview_view(conn, lib_root, &vol)
        }
        ("pipeline", 3) if segs[2] == "funnel" && method == "GET" => {
            super::super::pipeline_view::funnel_view(lib_root)
        }
        ("pipeline", 3) if segs[2] == "materials" && method == "GET" => {
            super::super::pipeline_view::materials_view(lib_root)
        }
        ("pipeline", 3) if segs[2] == "cardgen" && method == "GET" => {
            let vol = qget("volume").unwrap_or_else(|| "卷01".to_string());
            super::super::pipeline_view::cardgen_view(lib_root, &vol)
        }
        ("pipeline", 3) if segs[2] == "chapters" && method == "GET" => {
            let vol = qget("volume").unwrap_or_else(|| "卷01".to_string());
            super::super::pipeline_view::chapters_view(conn, lib_root, &vol)
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
            super::super::self_check::run(conn, lib_root, fix, 30)
        }
        // 【第1轮修复 B-03】`unlocked` 字段名极易误读：
        // 它表示「语义门控 gate_l1 && gate_l2」，与真实 LLM 文件锁毫无关系。
        // 实测该字段恒为 false，而 /api/system/llm-lock 同时报 exists=true，
        // 两个接口看起来互相矛盾（用户会以为"锁没生效"）。
        // 现改名 `semantic_gate_unlocked`，并保留 `unlocked` 作兼容字段但加注说明。
        ("system", 3) if segs[2] == "gate" && method == "GET" => {
            let (l1, l2) = super::super::task_store::gate_state();
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
            Ok(super::super::real_llm_lock::status())
        }
        ("system", 4) if segs[2] == "llm-lock" && segs[3] == "unlock" && method == "POST" => {
            super::super::real_llm_lock::unlock(conn, "webui:unlock-button")
        }
        ("system", 4) if segs[2] == "llm-lock" && segs[3] == "lock" && method == "POST" => {
            super::super::real_llm_lock::lock(conn, "webui:lock-button")
        }
        ("system", 3) if segs[2] == "llm-lock" && method == "POST" => {
            // 容错：POST /api/system/llm-lock {"action":"unlock"|"lock"}
            let act = serde_json::from_str::<Value>(body.trim()).ok()
                .and_then(|v| v["action"].as_str().map(|x| x.to_string()))
                .unwrap_or_default();
            match act.as_str() {
                "unlock" => super::super::real_llm_lock::unlock(conn, "webui:unlock-post"),
                "lock" => super::super::real_llm_lock::lock(conn, "webui:lock-post"),
                _ => return Some(Resp::bad(r#"system/llm-lock: POST body 需 {"action":"unlock"|"lock"}"#)),
            }
        }

        // ══════ P4：LLM 配置写（受控）
        ("config", 3) if segs[2] == "llm" => match method {
            "GET" => llm_config_view(lib_root),
            "PUT" => super::super::llm_config_write::write_config(conn, lib_root, body),
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
            "GET" => super::super::model_config::read(lib_root)
                .map(Ok)
                .unwrap_or_else(|e| Ok(json!({
                    "ok": false,
                    "error": e.to_string(),
                    "hint": "检查 .neagent/models.yml 是否存在且格式正确",
                }))),
            "POST" | "PUT" => super::super::model_config::write(conn, lib_root, body)
                .map(Ok)
                .unwrap_or_else(|e| Ok(json!({
                    "ok": false,
                    "error": e.to_string(),
                }))),
            _ => return Some(Resp::bad("config/model: GET / POST")),
        },
        ("config", 4) if segs[2] == "llm" && segs[3] == "test-connect" && method == "POST" => {
            Ok(super::super::llm_config_write::test_connect(body))
        }
        ("config", 4) if segs[2] == "llm" && segs[3] == "reset" && method == "POST" => {
            super::super::llm_config_write::reset_config(conn, lib_root)
        }
        ("config", 4) if segs[2] == "llm" && segs[3] == "reload" && method == "POST" => {
            let mut out = json!({"ok": false});
            if let Ok(cfg) = super::super::llm_config::load(lib_root) {
                let snap = super::super::llm_config::safe_snapshot(&cfg);
                let _ = super::super::category_store::log_op(conn, "llm.config.reload",
                    super::super::llm_config::CONFIG_REL, "reloaded", "");
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
            "GET" => super::super::task_store::list(conn, limit_of()),
            _ => return Some(Resp::bad("tasks: GET（提交用 /api/tasks/submit）")),
        },
        ("tasks", 3) if segs[2] == "submit" && method == "POST" => {
            super::super::task_store::submit(conn, body)
        }
        ("tasks", 3) if segs[2] == "list" && method == "GET" => {
            super::super::task_store::list(conn, limit_of())
        }
        // ══════ 活跃任务摘要（2026-10-17 UI 重构）══════
        // 供右下角"全局任务胶囊"3 秒轮询。刻意极轻：一次 GROUP BY + 5 条。
        // 不返回全量列表（那会最多 500 条 × 200 字预览，手机扛不住）。
        ("tasks", 3) if segs[2] == "active" && method == "GET" => {
            super::super::task_store::active_summary(conn)
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
            let exists = super::super::task_store::task_exists(conn, &task_id).unwrap_or(false);
            if !exists {
                // route_dyn 返回 Option<Resp>，用 Resp::bad 构造错误响应
                return Some(Resp::bad(&format!("任务不存在：{task_id}（未启动执行器）")));
            }
            let _ = super::super::category_store::log_op(conn, "task.dispatch", segs[2],
                &format!("limit={lim}"), "");
            super::super::task_store::dispatch_executor(lib_root, lim)
        }
        // 任务书 #6：失败归档列表
        ("tasks", 3) if segs[2] == "failures" && method == "GET" => {
            super::super::task_store::failures(conn, limit_of())
        }
        // 失败类型分布报表（dsh 阶段 2-4）
        //   GET /api/tasks/failure-stats          → 全窗口
        //   GET /api/tasks/failure-stats?days=7   → 近 7 天
        // 改造前 502 无法归因，就是因为只有自由文本 reason；
        // 现在一句 GROUP BY code 就能出「这次挂的到底是网关还是 max_tokens」。
        ("tasks", 3) if segs[2] == "failure-stats" && method == "GET" => {
            let days = qget("days").and_then(|x| x.parse::<i64>().ok()).unwrap_or(0);
            super::super::task_store::failure_stats(conn, days)
        }
        ("tasks", 3) => {
            let tid = segs[2];
            match method {
                // 【第2轮修复 U-04】原实现直接把 `task_store::result()` 的错误当成
                // JSON body 返回，HTTP 状态仍是 200 → 前端 `if (r.ok)` 永远为真，
                // 「任务不存在」被当成一次成功的查询，展示成空白详情。
                // 现在先判存在性，不存在返回 404；存在仍走原逻辑。
                "GET" => {
                    if !super::super::task_store::task_exists(conn, tid).unwrap_or(false) {
                        return Some(Resp::json_status(
                            404,
                            &json!({"ok": false, "error": format!("任务不存在：{tid}"), "id": tid}),
                        ));
                    }
                    super::super::task_store::result(conn, tid)
                }
                "DELETE" => super::super::task_store::cancel(conn, tid),
                _ => return Some(Resp::bad("tasks: GET / POST(submit) / DELETE")),
            }
        },
        ("tasks", 4) if segs[3] == "result" && method == "GET" => {
            // 同上：/result 子路径也要 404 语义，不能 200 带 error 字段。
            if !super::super::task_store::task_exists(conn, segs[2]).unwrap_or(false) {
                return Some(Resp::json_status(
                    404,
                    &json!({"ok": false, "error": format!("任务不存在：{}", segs[2]), "id": segs[2]}),
                ));
            }
            super::super::task_store::result(conn, segs[2])
        }
        // 结果回写（2026-10-17 · 路径A）：执行方（ToolPkg/MCP）执行完调用
        ("tasks", 4) if segs[3] == "result" && method == "POST" => {
            super::super::task_store::update_result(conn, segs[2], body)
        }
        // 细粒度事件推送（2026-10-17 SSE）：exec_pending.py 逐行读 stdout 后转发。
        // 只广播不写库 —— 高频日志写 SQLite 会拖慢建卡主链路。
        ("tasks", 4) if segs[3] == "event" && method == "POST" => {
            super::super::task_store::push_event(conn, segs[2], body)
        }

        // 思考/正文回放（2026-10-17 全流式）：
        //   GET /api/tasks/{id}/stream             → 该任务最近 50 张
        //   GET /api/tasks/{id}/stream?code=XXX    → 指定卡
        // 数据源是 task_stream 表（cardgen.py 写的全量思考+正文）。
        // 【为什么不走 result】task_store::list() 对 result 做 substr(...,1,2000)，
        // 一张卡上万字必被截断。
        ("tasks", 4) if segs[3] == "stream" && method == "GET" => {
            let code = qget("code").unwrap_or_default();
            super::super::task_store::list_stream(conn, segs[2], &code)
        }

        // ══════ P2a：分类树（直接开写）
        ("library", 3) if segs[2] == "categories" => match method {
            "GET" => super::super::category_store::list_tree(conn),
            "POST" => super::super::category_store::create(conn, body),
            _ => return Some(Resp::bad("categories: GET / POST")),
        },
        ("library", 4) if segs[2] == "categories" => {
            let id: i64 = segs[3].parse().unwrap_or(-1);
            if id < 0 {
                return Some(Resp::bad("分类 id 非法"));
            }
            match method {
                "GET" => super::super::category_store::docs(conn, id),
                "PUT" => super::super::category_store::update(conn, id, body),
                "DELETE" => super::super::category_store::delete(conn, id),
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
                "POST" => super::super::category_store::add_member(conn, id, &layer, &key),
                "DELETE" => super::super::category_store::rm_member(conn, id, &layer, &key),
                _ => return Some(Resp::bad("members: POST / DELETE")),
            }
        }

        // ══════ P2a：标签（复用 note_tag_store，直接开写）
        ("tags", 2) => match method {
            "GET" => super::super::note_tag_store::list_tags(conn, None, None)
                .map(|rows| json!({"count": rows.len(), "tags": rows})),
            _ => return Some(Resp::bad("tags: GET（写用 /api/items/{key}/tags）")),
        },

        // ══════ P2a：备注（直接开写）
        ("notes", 2) => match method {
            "GET" => super::super::note_tag_store::list_notes(conn, qget("status").as_deref())
                .map(|rows| json!({"count": rows.len(), "notes": rows})),
            _ => return Some(Resp::bad("notes: GET（写用 /api/items/{key}/note）")),
        },

        // ══════ P2a：单条目的 标签 / 备注（写）
        ("items", 3) => {
            let key = segs[2];
            match method {
                "GET" => super::super::note_tag_store::get_note(conn, key)
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
                super::super::note_tag_store::add_tags(conn, segs[2], &tags, "manual")
                    .map(|(a, b)| json!({"ok": true, "added": a, "existing": b}))
            }
            "DELETE" => {
                let tag = qget("tag").or_else(|| {
                    serde_json::from_str::<Value>(body.trim()).ok()
                        .and_then(|v| v["tag"].as_str().map(|s| s.to_string()))
                });
                match tag {
                    Some(t) => super::super::note_tag_store::rm_tag(conn, segs[2], &t)
                        .map(|n| json!({"ok": true, "removed": n})),
                    None => return Some(Resp::bad("需要 tag 参数")),
                }
            }
            _ => return Some(Resp::bad("items/{key}/tags: POST / DELETE")),
        },
        ("items", 4) if segs[3] == "note" => match method {
            "PUT" | "POST" => {
                let v: Value = serde_json::from_str(body.trim()).unwrap_or(json!({}));
                super::super::note_tag_store::set_note(
                    conn, segs[2], v["status"].as_str(), v["content"].as_str(),
                )
            }
            "GET" => super::super::note_tag_store::get_note(conn, segs[2])
                .map(|n| json!({"key": segs[2], "note": n})),
            _ => return Some(Resp::bad("items/{key}/note: GET / PUT")),
        },

        // ══════ P2a：审计（只读）
        ("audit", 3) if segs[2] == "op-logs" && method == "GET" => {
            super::super::category_store::op_logs(conn, limit_of())
        }
        ("audit", 3) if segs[2] == "llm-logs" && method == "GET" => super::super::llm_obs::stats(conn),
        ("audit", 3) if segs[2] == "snapshots" && method == "GET" => {
            super::super::diff_snapshot::list(conn).map(|rows| json!({"count": rows.len(), "snapshots": rows}))
        }

        // ⚠ 关键：不认识的 /api/* 必须返回 None，交回静态路由表。
        // （曾误写 Some(not_found())，导致 /api/overview /api/items /api/queue
        //   /api/snapshots /api/layers/list /api/system/status 全部 404。）
        _ => return None,
    };
    Some(ok(r))
}

/// 路径安全校验：拒绝 `..`、绝对路径逃逸、NUL。
pub fn safe_request_path(path: &str) -> bool {
    if path.contains("..") || path.contains('\0') {
        return false;
    }
    path.starts_with('/')
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
        c.execute_batch(include_str!("../../governance_schema.sql")).unwrap();
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
