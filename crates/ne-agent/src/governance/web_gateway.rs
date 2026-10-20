//! Web 面板网关（world-gov）—— **facade / 门面**。
//!
//! # Phase-1 架构重构（2026-10-19）
//! 原 1714 行单文件已按职责拆为三个子模块，本文件只做**重导出**，
//! 因此所有对外调用路径（`web_gateway::serve`、`web_gateway::route_for_test`、
//! `web_gateway::overview` …）**保持不变，对外契约零变化**。
//!
//! - [`gov_service`] —— 业务逻辑（DB 查询 / 会话落库），不感知 HTTP
//! - [`http_router`] —— 路由（path+query → `Resp`），不持有 TcpStream
//! - [`transport`]  —— 传输（HTTP/1.1 收发 / SSE / 静态资源托管）
//!
//! # 架构决策（与仓库一致，原样保留）
//! 仓库刻意保持「无 tokio / std 阻塞 IO / 单二进制」。
//! 本模块**不引入 axum/tokio**，而是用 `std::net::TcpListener` 手写入站 HTTP/1.1，
//! 与既有出站栈 `web.rs`（rustls + std::net + 手写请求/响应解析）同构。
//!
//! # 安全铁则（原样保留）
//! - ✅ **仅绑定 127.0.0.1**（不可改 0.0.0.0；本机外连不上）。
//! - ✅ 所有接口**等价调用内部函数**，不绕过门控/校验；不把 DB 直暴露给 HTTP。
//! - ✅ 无状态：状态全部来自 novel.db 与 07_勘误工坊 目录。
//! - ✅ 前端内联（零 CDN、零外网资源），完全离线可用。
//! - ✅ 无后台常驻：`serve` 阻塞于前台，Ctrl-C 即退出，不留进程。
//! - ✅ 请求路径严格校验（拒绝 `..`、绝对路径）；查询参数白名单。

pub mod gov_service;
pub mod http_router;
pub mod transport;

// ── 对外契约（与原单文件时代的 `web_gateway::*` 完全一致）──
pub use gov_service::{
    archive_view, browse_items, layers_list, llm_config_view, overview, queue_view, system_status,
};
pub use http_router::{route_for_test, safe_request_path};
pub use transport::{fe_dist_available, serve, DEFAULT_PORT};
