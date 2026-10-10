//! 阻塞式 HTTPS 客户端 —— **彻底替代 omp-rs-7k（tokio + reqwest）**。
//!
//! 设计目标：零异步运行时、零第三方 HTTP 栈，只用 `rustls` + `std::net`。
//! 为什么可行：本引擎的调用模式是「一次一条请求、同步等待」（写章节时串行调 LLM），
//! 不需要连接池/并发/HTTP2，异步运行时纯属负担。
//!
//! 支持：TLS（rustls + webpki-roots）、HTTP/1.1、Content-Length 与 chunked 两种响应体。
//! 不支持（明确声明）：HTTP/2、重定向（LLM 端点不需要）、cookie。

use anyhow::{anyhow, Context, Result};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

/// 一次 HTTP 响应。
#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        let lname = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| k.to_ascii_lowercase() == lname)
            .map(|(_, v)| v.as_str())
    }
}

/// 从 `https://host[:port]/path` 解析出 (host, port, path)。
fn parse_url(url: &str) -> Result<(String, u16, String)> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| anyhow!("只支持 https:// 端点（收到 {url}）"))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse::<u16>().context("非法端口")?),
        None => (authority.to_string(), 443),
    };
    Ok((host, port, path.to_string()))
}

/// 用 rustls 建立 TLS 连接并发一条 POST（application/json）。
pub fn post_json(url: &str, headers: &[(String, String)], body: &[u8], timeout: Duration) -> Result<Response> {
    let (host, port, path) = parse_url(url)?;

    // 1) TLS 配置（webpki-roots 内置根证书，无需系统 CA）
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let server_name = rustls_pki_types::ServerName::try_from(host.clone())
        .map_err(|e| anyhow!("非法主机名 {host}: {e}"))?;

    // 2) TCP 连接（带超时）
    let addr = format!("{host}:{port}");

    // 3) TLS 握手（StreamOwned 需持有连接的所有权）
    let conn = rustls::ClientConnection::new(Arc::new(cfg), server_name)
        .context("TLS 会话创建失败")?;
    let tcp_owned = TcpStream::connect(&addr).with_context(|| format!("TCP 连接失败：{addr}"))?;
    tcp_owned.set_read_timeout(Some(timeout))?;
    tcp_owned.set_write_timeout(Some(timeout))?;
    let mut tls = rustls::StreamOwned::new(conn, tcp_owned);

    // 4) 组装并发送 HTTP/1.1 请求
    let mut req = String::new();
    req.push_str(&format!("POST {path} HTTP/1.1\r\n"));
    req.push_str(&format!("Host: {host}\r\n"));
    req.push_str("User-Agent: ne-engine/0.1\r\n");
    req.push_str("Accept: */*\r\n");
    req.push_str("Connection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str(&format!("Content-Length: {}\r\n", body.len()));
    req.push_str("\r\n");
    tls.write_all(req.as_bytes())?;
    tls.write_all(body)?;
    tls.flush()?;

    // 5) 读全量响应
    let mut raw = Vec::new();
    tls.read_to_end(&mut raw).context("读取响应失败")?;
    parse_response(&raw)
}

/// 解析 HTTP/1.1 响应（状态行 + 头 + body，支持 chunked）。
fn parse_response(raw: &[u8]) -> Result<Response> {
    // 找头体分界
    let sep = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| anyhow!("响应无 \\r\\n\\r\\n 分界"))?;
    let head = String::from_utf8_lossy(&raw[..sep]).to_string();
    let mut body_bytes = raw[sep + 4..].to_vec();

    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow!("非法状态行：{status_line}"))?;

    let mut headers = Vec::new();
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }

    let chunked = headers
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("transfer-encoding") && v.to_ascii_lowercase().contains("chunked"));
    if chunked {
        body_bytes = decode_chunked(&body_bytes)?;
    }

    Ok(Response {
        status,
        headers,
        body: String::from_utf8_lossy(&body_bytes).to_string(),
    })
}

/// 解码 chunked 传输编码。
fn decode_chunked(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0;
    loop {
        // 读 size 行
        let line_end = find_crlf(data, i).ok_or_else(|| anyhow!("chunk 头未闭合"))?;
        let size_line = String::from_utf8_lossy(&data[i..line_end]).to_string();
        let size_hex = size_line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_hex, 16)
            .map_err(|e| anyhow!("非法 chunk 大小 `{size_hex}`: {e}"))?;
        i = line_end + 2;
        if size == 0 {
            break;
        }
        if i + size > data.len() {
            return Err(anyhow!("chunk 数据超出响应长度"));
        }
        out.extend_from_slice(&data[i..i + size]);
        i += size + 2; // 跳过 chunk 后的 CRLF
    }
    Ok(out)
}

fn find_crlf(data: &[u8], from: usize) -> Option<usize> {
    data[from..]
        .windows(2)
        .position(|w| w == b"\r\n")
        .map(|p| from + p)
}

/// 便捷：POST JSON 并解析响应 JSON（非 2xx 返回 Err，带响应体前 200 字）。
pub fn post_json_value(
    url: &str,
    headers: &[(String, String)],
    body: &serde_json::Value,
    timeout: Duration,
) -> Result<serde_json::Value> {
    let bytes = serde_json::to_vec(body)?;
    let resp = post_json(url, headers, &bytes, timeout)?;
    if !(200..300).contains(&resp.status) {
        let snippet: String = resp.body.chars().take(200).collect();
        return Err(anyhow!("HTTP {}: {snippet}", resp.status));
    }
    Ok(serde_json::from_str(&resp.body).context("响应非合法 JSON")?)
}
