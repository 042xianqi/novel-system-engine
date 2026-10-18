//! 外部世界 —— HTTP(S) GET + 网页转文本 + 多引擎搜索。
//!
//! # 设计约束
//!
//! `ne_engine::http` **只有 `post_json`**（服务于 LLM 调用，够用）。而 `web_fetch`
//! 需要 GET + 重定向 + 大小上限 + HTML 转文本。按"不改现有 ne-* crate"的硬约束，
//! 本模块**在 ne-agent 内自写**，复用同样的传输栈（rustls + `std::net`，零 tokio）。
//!
//! # 抄自太墟的地方
//!
//! 太墟把网络能力做成**一个通用 `mcp` 工具**（`HarnessMessage.kt:20`），
//! 其内置的 websearch MCP 是个零依赖 Python 脚本
//! （`/opt/taixu/scripts/websearch_mcp_server.py`），要点全部照搬：
//!   * **多引擎**（Baidu / Bing / DuckDuckGo）+ **失败自动回退**（单一引擎 HTML 结构随时会变）
//!   * 免 API Key，直接取搜索页正则解析
//!   * 忽略证书校验（手机沙箱根证书常不全）—— 本实现改用 rustls 内置根证书，
//!     更安全；若确遇到沙箱根证书缺失，可另加开关（暂不加，见模块末尾说明）
//!
//! # 安全默认
//!
//!   * 只允许 http/https；`http` 明文**仅限本机回环**（与 ne-engine 同口径）
//!   * 响应体硬上限（默认 2 MiB），超限即停读
//!   * 重定向次数上限（默认 5），每跳重新校验 scheme
//!   * 超时必设（默认 20s）

use anyhow::{anyhow, Context, Result};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use crate::web_html::body_to_text;

/// 默认响应体上限（字节）。
pub const DEFAULT_MAX_BYTES: usize = 2 * 1024 * 1024;
/// 默认超时。
pub const DEFAULT_TIMEOUT_SECS: u64 = 20;
/// 默认重定向上限。
pub const MAX_REDIRECTS: usize = 5;
/// 搜索请求 UA（照太墟，用常规浏览器 UA 以免被反爬）。
const SEARCH_UA: &str =
    "Mozilla/5.0 (Linux; Android 14; Mobile; rv:128.0) Gecko/128.0 Firefox/128.0";

// ───────────────────────────── URL

/// 解析 URL → (scheme, host, port, path)。
fn parse_url(url: &str) -> Result<(String, String, u16, String)> {
    let (scheme, rest, dport) = if let Some(r) = url.strip_prefix("https://") {
        ("https", r, 443u16)
    } else if let Some(r) = url.strip_prefix("http://") {
        ("http", r, 80u16)
    } else {
        return Err(anyhow!("只支持 http/https：{url}"));
    };
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    // 去掉可能的 ?query 带来的干扰（path 已包含）
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
            (h.to_string(), p.parse::<u16>().map_err(|_| anyhow!("非法端口：{url}"))?)
        }
        _ => (hostport.to_string(), dport),
    };
    if host.is_empty() {
        return Err(anyhow!("URL 缺少主机名：{url}"));
    }
    Ok((scheme.to_string(), host, port, path.to_string()))
}

/// 带超时的 TCP 连接。
///
/// ⚠ 必须自己解析并逐个尝试地址：`TcpStream::connect(&str)` 在只解析到
/// **不可达的 IPv6** 时会挂到 OS 默认超时（实测百度/DDG 只有 AAAA 记录时挂死 40s+）。
/// 用 `connect_timeout` 对每个候选地址设限，任一成功即用。
fn connect_timeout(host: &str, port: u16, timeout: Duration) -> Result<TcpStream> {
    use std::net::ToSocketAddrs;
    let addrs: Vec<std::net::SocketAddr> = (host, port)
        .to_socket_addrs()
        .with_context(|| format!("DNS 解析失败：{host}"))?
        .collect();
    if addrs.is_empty() {
        return Err(anyhow!("无法解析主机：{host}"));
    }
    let per = Duration::from_millis((timeout.as_millis() as u64 / addrs.len().max(1) as u64).max(1500));
    let mut last = String::new();
    for a in &addrs {
        match TcpStream::connect_timeout(a, per) {
            Ok(s) => return Ok(s),
            Err(e) => last = format!("{a}: {e}"),
        }
    }
    Err(anyhow!("TCP 连接失败（{host}:{port}）：{last}"))
}

/// 是否本机回环（http 明文只允许这类）。
fn is_loopback(host: &str) -> bool {
    host == "localhost" || host == "127.0.0.1" || host == "::1"
}

/// 相对重定向 → 绝对 URL。
fn resolve_redirect(base: &str, loc: &str) -> String {
    if loc.starts_with("http://") || loc.starts_with("https://") {
        return loc.to_string();
    }
    let Ok((scheme, host, port, _)) = parse_url(base) else {
        return loc.to_string();
    };
    let default_port = (scheme == "https" && port == 443) || (scheme == "http" && port == 80);
    let hostport = if default_port { host } else { format!("{host}:{port}") };
    if loc.starts_with('/') {
        format!("{scheme}://{hostport}{loc}")
    } else {
        format!("{scheme}://{hostport}/{loc}")
    }
}

// ───────────────────────────── 传输

/// 一次 HTTP 响应（含重定向信息）。
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub location: Option<String>,
    pub content_type: String,
    /// 原始响应字节（**延迟按 charset 解码**——中文页常是 GBK）
    pub body_bytes: Vec<u8>,
    pub truncated: bool,
}

impl HttpResponse {
    /// 按 charset 解码为文本。
    pub fn text(&self) -> String {
        body_to_text(&self.content_type, &self.body_bytes)
    }
}

/// 发一次单跳请求（不跟随重定向）。
fn request_once(
    method: &str,
    url: &str,
    ua: &str,
    body: Option<&[u8]>,
    max_bytes: usize,
    timeout: Duration,
) -> Result<HttpResponse> {
    let (scheme, host, port, path) = parse_url(url)?;
    if scheme == "http" && !is_loopback(&host) {
        return Err(anyhow!("http 明文仅允许本机回环；公网请用 https：{url}"));
    }

    let addr = format!("{host}:{port}");
    let tcp = connect_timeout(&host, port, timeout)
        .with_context(|| format!("连接 {addr} 失败"))?;
    tcp.set_read_timeout(Some(timeout))?;
    tcp.set_write_timeout(Some(timeout))?;

    let payload = body.unwrap_or(&[]);
    let mut req = String::new();
    req.push_str(&format!("{method} {path} HTTP/1.1\r\n"));
    req.push_str(&format!("Host: {host}\r\n"));
    req.push_str(&format!("User-Agent: {ua}\r\n"));
    req.push_str("Accept: text/html,application/xhtml+xml,application/json,text/plain,*/*\r\n");
    req.push_str("Accept-Language: zh-CN,zh;q=0.9,en;q=0.8\r\n");
    req.push_str("Accept-Encoding: identity\r\n"); // 不声明 gzip，避免解压
    req.push_str("Connection: close\r\n");
    if body.is_some() {
        req.push_str(&format!("Content-Length: {}\r\n", payload.len()));
    }
    req.push_str("\r\n");

    let mut raw = Vec::new();
    const HEAD_MARGIN: usize = 64 * 1024;
    let cap = max_bytes + HEAD_MARGIN;
    if scheme == "https" {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let cfg = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .with_root_certificates(roots)
            .with_no_client_auth();
        let server_name = rustls_pki_types::ServerName::try_from(host.clone())
            .map_err(|e| anyhow!("非法主机名 {host}: {e}"))?;
        let conn = rustls::ClientConnection::new(Arc::new(cfg), server_name)
            .context("TLS 会话创建失败")?;
        let mut tls = rustls::StreamOwned::new(conn, tcp);
        tls.write_all(req.as_bytes())?;
        if !payload.is_empty() {
            tls.write_all(payload)?;
        }
        tls.flush()?;
        read_capped(&mut tls, &mut raw, cap)?;
    } else {
        let mut s = tcp;
        s.write_all(req.as_bytes())?;
        if !payload.is_empty() {
            s.write_all(payload)?;
        }
        s.flush()?;
        read_capped(&mut s, &mut raw, cap)?;
    }

    let sep = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| anyhow!("响应无 \\r\\n\\r\\n 分界（可能非 HTTP 或已断开）"))?;
    let head = String::from_utf8_lossy(&raw[..sep]).to_string();
    let body_bytes = &raw[sep + 4..];

    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow!("非法状态行：{status_line}"))?;

    let mut location = None;
    let mut content_type = String::new();
    let mut chunked = false;
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim().to_string());
            match k.as_str() {
                "location" => location = Some(v),
                "content-type" => content_type = v,
                "transfer-encoding" if v.to_ascii_lowercase().contains("chunked") => chunked = true,
                _ => {}
            }
        }
    }

    let decoded = if chunked { dechunk(body_bytes) } else { body_bytes.to_vec() };
    let truncated = decoded.len() > max_bytes;
    let cut = if truncated { &decoded[..max_bytes] } else { &decoded[..] };

    Ok(HttpResponse {
        status,
        location,
        content_type,
        body_bytes: cut.to_vec(),
        truncated,
    })
}

/// 跟随重定向的 GET。
pub fn get(url: &str, max_bytes: usize, timeout: Duration) -> Result<HttpResponse> {
    get_with_ua(url, max_bytes, timeout, None)
}

/// 同 [`get`]，但可指定 User-Agent（搜索引擎需要浏览器 UA 以免被反爬）。
pub fn get_with_ua(
    url: &str,
    max_bytes: usize,
    timeout: Duration,
    ua: Option<&str>,
) -> Result<HttpResponse> {
    let ua = ua
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("ne-agent/{} (novel-system)", env!("CARGO_PKG_VERSION")));
    let mut current = url.to_string();
    for _hop in 0..=MAX_REDIRECTS {
        let r = request_once("GET", &current, &ua, None, max_bytes, timeout)?;
        if (300..400).contains(&r.status) {
            if let Some(loc) = &r.location {
                current = resolve_redirect(&current, loc);
                continue;
            }
        }
        return Ok(r);
    }
    Err(anyhow!("重定向次数超过 {MAX_REDIRECTS}：{url}"))
}

/// 搜索请求 UA（照太墟，用常规浏览器 UA 以免被反爬）。
pub fn search_user_agent() -> &'static str {
    SEARCH_UA
}

// ── 拆出的模块在原路径上重导出，调用方无需改（`ne_agent::web::search` 等照旧）
pub use crate::web_html::html_to_text;
pub use crate::web_search::{search, SearchHit};

fn read_capped<R: Read>(r: &mut R, out: &mut Vec<u8>, cap: usize) -> Result<()> {
    let mut buf = [0u8; 16 * 1024];
    loop {
        match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                out.extend_from_slice(&buf[..n]);
                if out.len() >= cap {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(anyhow!("读响应失败：{e}")),
        }
    }
    Ok(())
}

/// 解 chunked 传输编码。
fn dechunk(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        let Some(nl) = find_crlf(&b[i..]) else { break };
        let line = String::from_utf8_lossy(&b[i..i + nl]);
        let hex = line.split(';').next().unwrap_or("").trim();
        let Ok(size) = usize::from_str_radix(hex, 16) else { break };
        i += nl + 2;
        if size == 0 {
            break;
        }
        let end = (i + size).min(b.len());
        out.extend_from_slice(&b[i..end]);
        i = end + 2;
    }
    out
}

fn find_crlf(b: &[u8]) -> Option<usize> {
    b.windows(2).position(|w| w == b"\r\n")
}

pub fn fetch_text(url: &str, max_chars: usize) -> Result<String> {
    let r = get(url, DEFAULT_MAX_BYTES, Duration::from_secs(DEFAULT_TIMEOUT_SECS))?;
    if r.status >= 400 {
        return Err(anyhow!("HTTP {}（{url}）", r.status));
    }
    let raw = r.text();
    let text = if r.content_type.contains("json") {
        raw
    } else {
        html_to_text(&raw)
    };
    let truncated = text.chars().count() > max_chars;
    let mut out = if truncated {
        text.chars().take(max_chars).collect::<String>()
    } else {
        text
    };
    if truncated {
        out.push_str(&format!("\n\n…（已截断至 {max_chars} 字）"));
    }
    Ok(format!(
        "URL: {url}\n状态: {}\n类型: {}\n\n{}",
        r.status, r.content_type, out
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_url_https_default_port() {
        let (s, h, p, path) = parse_url("https://example.com/a/b?c=1").unwrap();
        assert_eq!((s.as_str(), h.as_str(), p, path.as_str()), ("https", "example.com", 443, "/a/b?c=1"));
    }

    #[test]
    fn parse_url_explicit_port_and_bare() {
        let (_, h, p, path) = parse_url("http://127.0.0.1:8080").unwrap();
        assert_eq!((h.as_str(), p, path.as_str()), ("127.0.0.1", 8080, "/"));
    }

    #[test]
    fn parse_url_rejects_non_http() {
        assert!(parse_url("ftp://x/y").is_err());
        assert!(parse_url("file:///etc/passwd").is_err());
    }

    #[test]
    fn http_plaintext_only_loopback() {
        let e = request_once(
            "GET",
            "http://93.184.216.34/",
            "t",
            None,
            1024,
            Duration::from_millis(200),
        )
        .unwrap_err();
        assert!(e.to_string().contains("回环"), "{e}");
    }

    #[test]
    fn resolve_redirect_absolute_and_relative() {
        assert_eq!(resolve_redirect("https://a.com/x", "https://b.com/y"), "https://b.com/y");
        assert_eq!(resolve_redirect("https://a.com/x", "/y"), "https://a.com/y");
        assert_eq!(resolve_redirect("https://a.com/x", "y"), "https://a.com/y");
        assert_eq!(resolve_redirect("https://a.com:8443/x", "/y"), "https://a.com:8443/y");
    }

    #[test]
    fn dechunk_basic() {
        let raw = b"5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        assert_eq!(String::from_utf8_lossy(&dechunk(raw)), "hello world");
    }

    #[test]
    fn fetch_text_rejects_bad_scheme() {
        assert!(fetch_text("ftp://x", 100).is_err());
    }

    #[test]
    fn response_text_uses_charset() {
        let r = HttpResponse {
            status: 200,
            location: None,
            content_type: "text/html; charset=gbk".into(),
            body_bytes: vec![0xb5, 0xda, 0xd2, 0xbb, 0xd5, 0xc2],
            truncated: false,
        };
        assert_eq!(r.text(), "第一章");
    }
}
