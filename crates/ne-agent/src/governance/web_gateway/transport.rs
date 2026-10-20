//! HTTP 传输层（自 `web_gateway.rs` 拆出，Phase-1 架构重构）。
//!
//! # 职责边界
//! 只做**字节流收发**：HTTP/1.1 请求解析、响应写回、SSE 分块推送、前端静态资源托管。
//! 业务一律委托 `http_router` / `gov_service`。**零 tokio / std 阻塞 IO**（与仓库一致）。
//!
//! # 安全铁则（原样保留）
//! - 仅绑定 127.0.0.1；请求路径严格校验（拒绝 `..`、绝对路径）。
use anyhow::Result;
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::Path;

use super::http_router::{chat_once, route, safe_request_path, Resp};

/// 默认端口（规格：8270）。
pub const DEFAULT_PORT: u16 = 8270;
/// 单请求最大字节（防超大 body 打爆内存）。
const MAX_REQ_BYTES: usize = 256 * 1024;

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
    // ── SSE：聊天真流式（?stream=1，Phase-1 新增）──
    //
    // 与 /api/tasks/stream 同理：route() 的 Resp 模型带 Content-Length、
    // 一次性写完就关，塞不进「分块推送」。所以在这里拦截，直接操作流。
    // 不带 ?stream=1 时照旧走 route() → 一次性 JSON，旧客户端 100% 兼容。
    if path == "/api/chat"
        && method == "POST"
        && query.split('&').any(|kv| kv == "stream=1" || kv == "stream")
    {
        sse_chat_stream(&mut stream, lib_root, &body);
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
    let (rx, sub_id) = super::super::task_events::subscribe();
    // 【R12】无论从哪条 return/break 退出，都必须注销订阅。
    //   原实现只靠 publish() 里的 retain 间接清理，无事件时永不执行，
    //   实测 curl 断开后 subscribers 计数只增不减（2→3→4→5）。
    sse_stream_body(stream, &rx);
    super::super::task_events::unsubscribe(sub_id);
    // 注销后打一行（仅 NE_SSE_DEBUG=1 时输出，避免污染生产日志）。
    //   本轮靠这行实测确认了「死订阅是否真的被摘除」：
    //   修复前 0 条，修复后 5 条且 remaining 递减到 0。
    if std::env::var("NE_SSE_DEBUG").ok().as_deref() == Some("1") {
        eprintln!("[sse] unsubscribed id={} remaining={}", sub_id,
                  super::super::task_events::subscriber_count());
    }
}

/// SSE 主循环（与订阅生命周期绑定）。
fn sse_stream_body(stream: &mut TcpStream, rx: &std::sync::mpsc::Receiver<super::super::task_events::GovEvent>) {
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
    let n_subs = super::super::task_events::subscriber_count();
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
            Ok(ev) => {
                let frame = ev.to_sse_frame();
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
pub(crate) const PANEL_HTML: &str = include_str!("../web_panel.html");
/// P1 管理台（管线管理 UI）——同为零依赖内联。
pub(crate) const WEBUI_PANEL_HTML: &str = include_str!("../webui_panel.html");

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
pub(crate) fn fe_dist_dir() -> Option<&'static std::path::PathBuf> {
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
pub(crate) fn mime_of(path: &str) -> &'static str {
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
pub(crate) fn read_dist_file(rel: &str) -> Option<Resp> {
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
pub(crate) fn dist_index() -> Option<Resp> {
    read_dist_file("index.html")
}


// ══════════════════════════════════ Phase-1：聊天真流式（?stream=1）

/// 聊天真流式：POST /api/chat?stream=1。
///
/// 复用 `http_router::chat_once`（与一次性 JSON 路径完全同一逻辑，落库
/// 只有一处），把完整回复按块经 SSE 推送：
///   首帧 {"start":true} → 正文块 {"t":"..."} ×N → 末帧 {"done":true, ...完整回复}
///
/// 诚实边界：回复由 L1 一次性产出（LLM 层 `"stream": false`，且仓库不引入
/// tokio/reqwest），这里做的是**传输层真分块**——前端逐块上屏，不做假
/// 打字机、不改生成链路。末帧字段与一次性 JSON 100% 一致，只看最终结果
/// 的消费方完全兼容。
///
/// 协议帧格式与 `/api/tasks/stream` 同构（`data: {...}\n\n`），复用前端
/// 已有的 SSE 解析习惯。
fn sse_chat_stream(stream: &mut TcpStream, lib_root: &Path, body: &str) {
    // 响应头：text/event-stream，无 Content-Length，Connection: close
    // （照抄 sse_task_stream 的头部语义，浏览器/反代都能正确识别分块流）
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\n\r\n";
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    let _ = stream.flush();

    // 单帧发送：成功=true；客户端断开即 false，调用方停止推送
    fn send(stream: &mut TcpStream, frame: &str) -> bool {
        stream.write_all(frame.as_bytes()).is_ok() && stream.flush().is_ok()
    }

    // 首帧：meta（前端收到即知流式链路建立）
    if !send(stream, "data: {\"start\":true}\n\n") {
        return;
    }

    // 与 handle_conn 同源：每请求打开治理库连接
    let out = match crate::governance::open_gov(lib_root) {
        Ok(conn) => match chat_once(&conn, lib_root, body) {
            Ok(v) => v,
            Err(e) => json!({
                "ok": false,
                "reply": format!("内部错误：{e}"),
                "matched": "error",
                "plan": [], "task_ids": [], "needs_unlock": false,
                "hint": null,
            }),
        },
        Err(e) => json!({
            "ok": false,
            "reply": format!("打开治理库失败: {e}"),
            "matched": "error",
            "plan": [], "task_ids": [], "needs_unlock": false,
            "hint": null,
        }),
    };

    // 正文分块推送：每块 64 字符，块间 50ms（给前端留出逐块渲染节奏；
    // 也避免超长回复一口气灌满 socket 缓冲导致浏览器一次收完）
    let reply = out["reply"].as_str().unwrap_or("").to_string();
    let chars: Vec<char> = reply.chars().collect();
    let block = 64usize;
    let mut i = 0usize;
    while i < chars.len() {
        let end = (i + block).min(chars.len());
        let piece: String = chars[i..end].iter().collect();
        let frame = format!("data: {}\n\n", json!({ "t": piece }));
        if !send(stream, &frame) {
            // 客户端中途断开：停止推送即可。落库已在 chat_once 内完成，
            // 不受传输中断影响（与一次性路径语义一致）。
            return;
        }
        i = end;
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // 末帧：完整回复（字段与一次性 JSON 接口一致，前端用它校准 + 取 plan/task_ids）
    let frame = format!("data: {}\n\n", out);
    let _ = send(stream, &frame);
}
