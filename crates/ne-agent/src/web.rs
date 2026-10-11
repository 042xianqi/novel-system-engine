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
    let ua = format!("ne-agent/{} (novel-system)", env!("CARGO_PKG_VERSION"));
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

// ───────────────────────────── HTML → 文本

/// HTML → 纯文本（**无依赖**的保守实现，够用于读文章正文）。
pub fn html_to_text(html: &str) -> String {
    let mut s = html.to_string();
    s = strip_between(&s, "<!--", "-->");
    for (open, close) in [
        ("<script", "</script>"),
        ("<style", "</style>"),
        ("<noscript", "</noscript>"),
        ("<head", "</head>"),
        ("<svg", "</svg>"),
        ("<nav", "</nav>"),
        ("<footer", "</footer>"),
    ] {
        s = strip_tag_blocks(&s, open, close);
    }
    for t in [
        "p", "div", "br", "li", "tr", "h1", "h2", "h3", "h4", "h5", "h6", "section", "article",
        "header", "blockquote", "pre", "table",
    ] {
        s = replace_ci(&s, &format!("<{t}"), &format!("\n<{t}"));
        s = replace_ci(&s, &format!("</{t}>"), "\n");
    }
    s = strip_tags(&s);
    s = decode_entities(&s);
    let mut out = String::new();
    let mut blank = 0;
    for line in s.lines() {
        let t = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if t.is_empty() {
            blank += 1;
            if blank <= 1 {
                out.push('\n');
            }
        } else {
            blank = 0;
            out.push_str(&t);
            out.push('\n');
        }
    }
    out.trim().to_string()
}

fn strip_between(s: &str, open: &str, close: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(open) {
        out.push_str(&rest[..i]);
        match rest[i..].find(close) {
            Some(j) => rest = &rest[i + j + close.len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

fn strip_tag_blocks(s: &str, open: &str, close: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = find_ci(rest, open) {
        out.push_str(&rest[..i]);
        match find_ci(&rest[i..], close) {
            Some(j) => rest = &rest[i + j + close.len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

fn find_ci(hay: &str, needle: &str) -> Option<usize> {
    hay.to_ascii_lowercase().find(&needle.to_ascii_lowercase())
}

fn replace_ci(s: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        match find_ci(rest, from) {
            Some(i) => {
                out.push_str(&rest[..i]);
                out.push_str(to);
                rest = &rest[i + from.len()..];
            }
            None => {
                out.push_str(rest);
                break;
            }
        }
    }
    out
}

fn decode_entities(s: &str) -> String {
    let mut out = s.to_string();
    for (e, c) in [
        ("&nbsp;", " "),
        ("&amp;", "&"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&apos;", "'"),
        ("&mdash;", "—"),
        ("&ndash;", "–"),
        ("&hellip;", "…"),
        ("&ldquo;", "\u{201c}"),
        ("&rdquo;", "\u{201d}"),
        ("&middot;", "·"),
        ("&times;", "×"),
    ] {
        out = out.replace(e, c);
    }
    let chars: Vec<char> = out.chars().collect();
    let mut res = String::with_capacity(out.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '&' && i + 2 < chars.len() && chars[i + 1] == '#' {
            let mut j = i + 2;
            let hex = j < chars.len() && (chars[j] == 'x' || chars[j] == 'X');
            if hex {
                j += 1;
            }
            let start = j;
            while j < chars.len() && chars[j] != ';' && j - start < 8 {
                j += 1;
            }
            if j < chars.len() && chars[j] == ';' && j > start {
                let digits: String = chars[start..j].iter().collect();
                let code = if hex {
                    u32::from_str_radix(&digits, 16).ok()
                } else {
                    digits.parse::<u32>().ok()
                };
                if let Some(cp) = code.and_then(char::from_u32) {
                    res.push(cp);
                    i = j + 1;
                    continue;
                }
            }
        }
        res.push(chars[i]);
        i += 1;
    }
    res
}

/// 从 Content-Type 头或 HTML meta 里探测字符集。
fn detect_charset(content_type: &str, body_bytes: &[u8]) -> String {
    // 1) HTTP 头
    if let Some(i) = content_type.to_ascii_lowercase().find("charset=") {
        let cs = content_type[i + 8..]
            .trim()
            .trim_matches('"')
            .split(|c: char| c == ';' || c == ' ')
            .next()
            .unwrap_or("")
            .to_string();
        if !cs.is_empty() {
            return cs.to_ascii_lowercase();
        }
    }
    // 2) HTML <meta charset=...> / <meta http-equiv ... content="...charset=...">
    let head = &body_bytes[..body_bytes.len().min(4096)];
    let head = String::from_utf8_lossy(head).to_ascii_lowercase();
    if let Some(i) = head.find("charset=") {
        // 跳过 `=` 与其后的引号/空白
        let after = head[i + 8..].trim_start_matches(['"', '\'', ' ', '=']);
        let cs: String = after
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if !cs.is_empty() {
            return cs;
        }
    }
    "utf-8".to_string()
}

/// 按字符集解码字节（支持 utf-8 / gbk / gb18030 / big5 等）。
///
/// ⚠ 这一步很关键：**中文搜索页常是 GBK**，用 `from_utf8_lossy` 会把整页中文
/// 变成乱码 → 后续正则一条都匹配不到。
/// 用 **`encoding_rs`**（成熟库，reqwest/浏览器同源）而非手搓字符表。
fn decode_bytes(bytes: &[u8], charset: &str) -> String {
    let enc = encoding_rs::Encoding::for_label(charset.trim().as_bytes())
        .unwrap_or(encoding_rs::UTF_8);
    let (cow, _, _) = enc.decode(bytes);
    cow.into_owned()
}

/// 把响应体字节按其字符集解码成文本。
fn body_to_text(content_type: &str, bytes: &[u8]) -> String {
    let cs = detect_charset(content_type, bytes);
    decode_bytes(bytes, &cs)
}

#[cfg(test)]
fn decode_for_test() {}

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

// ───────────────────────────── 搜索（照太墟 websearch_mcp_server.py）

/// 一条搜索结果。
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// 免 API Key 搜索：按引擎取结果页、正则解析；**首选失败自动回退其余引擎**。
pub fn search(query: &str, engine: &str, limit: usize) -> Result<Vec<SearchHit>> {
    if query.trim().is_empty() {
        return Err(anyhow!("搜索词为空"));
    }
    let limit = limit.clamp(1, 15);
    let primary = engine.to_ascii_lowercase();
    let mut order = vec![primary.clone()];
    for fb in ["bing", "baidu", "duckduckgo"] {
        if !order.iter().any(|e| e == fb) {
            order.push(fb.to_string());
        }
    }
    let mut last_err = String::new();
    for e in &order {
        let r = match e.as_str() {
            "bing" => search_bing(query, limit),
            "duckduckgo" | "ddg" => search_ddg(query, limit),
            _ => search_baidu(query, limit),
        };
        match r {
            Ok(hits) if !hits.is_empty() => return Ok(hits),
            Ok(_) => last_err = format!("{e} 无结果"),
            Err(err) => last_err = format!("{e}: {err}"),
        }
    }
    Err(anyhow!("全部引擎均未取得结果（{last_err}）"))
}

fn search_get(url: &str) -> Result<String> {
    let r = request_once(
        "GET",
        url,
        SEARCH_UA,
        None,
        DEFAULT_MAX_BYTES,
        Duration::from_secs(15),
    )?;
    if r.status >= 400 {
        return Err(anyhow!("HTTP {}", r.status));
    }
    Ok(r.text())
}

fn search_baidu(query: &str, limit: usize) -> Result<Vec<SearchHit>> {
    let url = format!(
        "https://www.baidu.com/s?wd={}&rn={}",
        url_encode(query),
        limit.max(10)
    );
    let html = search_get(&url)?;
    let mut hits = extract_baidu_cards(&html, limit);
    // 精确卡片未命中 → 回退通用 h3 抽取（太墟同法）
    if hits.is_empty() {
        hits = extract_h3_generic(&html, limit, Some("baidu.com"));
    }
    Ok(hits)
}

/// 百度搜索卡片：`<h3 class="t"><a href=...>标题</a></h3>...摘要 div`
fn extract_baidu_cards(html: &str, limit: usize) -> Vec<SearchHit> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(i) = find_ci(rest, "<h3") {
        let after = &rest[i..];
        let Some(gt) = after.find('>') else { break };
        let h3tag = &after[..gt];
        // h3 需带 class="t"（搜索结果标题），否则跳过
        if !h3tag.to_ascii_lowercase().contains("class=\"t") && !h3tag.contains("class=\"t") {
            rest = &after[gt + 1..];
            continue;
        }
        let seg = &after[gt..];
        if let Some((link, inner, consumed)) = anchor_in(seg) {
            let title = html_to_text(&inner);
            if !title.is_empty() && !link.is_empty() {
                out.push(SearchHit { title, url: link, snippet: String::new() });
            }
            rest = &seg[consumed..];
        } else {
            rest = &after[gt + 1..];
        }
        if out.len() >= limit {
            break;
        }
    }
    out
}

fn search_bing(query: &str, limit: usize) -> Result<Vec<SearchHit>> {
    let url = format!(
        "https://cn.bing.com/search?q={}&count={}",
        url_encode(query),
        limit.max(10)
    );
    let html = search_get(&url)?;
    // Bing：<li class="b_algo"> ... <h2><a href=...>标题</a></h2> ... <p>摘要</p>
    let mut hits = extract_bing_algo(&html, limit);
    if hits.is_empty() {
        hits = extract_h3_generic(&html, limit, None);
    }
    Ok(hits)
}

/// Bing `b_algo` 块：取 h2 里的锚点 + 首个 `<p>` 摘要。
fn extract_bing_algo(html: &str, limit: usize) -> Vec<SearchHit> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(i) = find_ci(rest, "class=\"b_algo\"") {
        let seg = &rest[i..];
        // 在该块内找 h2 锚点
        if let Some(h2i) = find_ci(seg, "<h2>") {
            let h2seg = &seg[h2i..];
            if let Some(h2gt) = h2seg.find('>') {
                let inner = &h2seg[h2gt..];
                if let Some((link, title_html, _)) = anchor_in(inner) {
                    let title = html_to_text(&title_html);
                    let snippet = first_p_text(inner);
                    if !title.is_empty() && !link.is_empty() {
                        out.push(SearchHit { title, url: link, snippet });
                    }
                }
            }
        }
        // 前进到本块之后
        rest = &seg["class=\"b_algo\"".len()..];
        if out.len() >= limit {
            break;
        }
    }
    out
}

/// 取片段里第一个 `<p>...</p>` 的文本（Bing 摘要）。
fn first_p_text(s: &str) -> String {
    if let Some(i) = find_ci(s, "<p") {
        let seg = &s[i..];
        if let Some(gt) = seg.find('>') {
            let after = &seg[gt + 1..]; // 跳过 '>'
            if let Some(e) = find_ci(after, "</p>") {
                return html_to_text(&after[..e]);
            }
        }
    }
    String::new()
}

/// 通用兜底：任意 `<h3>...<a href=...>标题</a>`（可排除某域名标题）。
fn extract_h3_generic(html: &str, limit: usize, exclude_host: Option<&str>) -> Vec<SearchHit> {
    let mut out = Vec::new();
    for (link, inner) in h3_links(html) {
        let title = html_to_text(&inner);
        if title.is_empty() || link.is_empty() {
            continue;
        }
        if let Some(h) = exclude_host {
            if title.contains(h) {
                continue;
            }
        }
        out.push(SearchHit { title, url: link, snippet: String::new() });
        if out.len() >= limit {
            break;
        }
    }
    out
}

fn search_ddg(query: &str, limit: usize) -> Result<Vec<SearchHit>> {
    let url = format!("https://html.duckduckgo.com/html/?q={}", url_encode(query));
    let html = search_get(&url)?;
    let mut hits = Vec::new();
    for (raw_link, title_raw) in class_links(&html, "result__a") {
        let link = extract_uddg(&raw_link).unwrap_or(raw_link);
        let title = html_to_text(&title_raw);
        if !title.is_empty() && !link.is_empty() {
            hits.push(SearchHit { title, url: link, snippet: String::new() });
        }
        if hits.len() >= limit {
            break;
        }
    }
    Ok(hits)
}

fn extract_uddg(link: &str) -> Option<String> {
    let i = link.find("uddg=")?;
    let rest = &link[i + 5..];
    let end = rest.find('&').unwrap_or(rest.len());
    Some(url_decode(&rest[..end]))
}

/// 百分号编码（空格用 `+`，搜索引擎惯例）。
fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

/// 抽 `<h3 ...><a href="X">T</a>` 形式的 (link, inner_html) 对（Bing/Baidu 通用）。
fn h3_links(html: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(i) = find_ci(rest, "<h3") {
        let after = &rest[i..];
        let Some(gt) = after.find('>') else { break };
        let seg = &after[gt..];
        if let Some((link, inner, consumed)) = anchor_in(seg) {
            out.push((link, inner));
            rest = &seg[consumed..];
            continue;
        }
        rest = &after[gt + 1..];
    }
    out
}

/// 抽 `class="<cls>"` 的 `<a href=X>inner</a>`。
fn class_links(html: &str, cls: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(i) = find_ci(rest, "<a") {
        let after = &rest[i..];
        let Some(gt) = after.find('>') else { break };
        let tag = &after[..gt];
        if tag.to_ascii_lowercase().contains(&cls.to_ascii_lowercase()) {
            if let Some((link, inner, consumed)) = anchor_in(after) {
                out.push((link, inner));
                rest = &after[consumed..];
                continue;
            }
        }
        rest = &after[gt + 1..];
    }
    out
}

/// 从一个以 `<a` 开头的片段抽 (href, inner, 已消耗长度)。
fn anchor_in(seg: &str) -> Option<(String, String, usize)> {
    let href = extract_href(seg)?;
    let open_gt = seg.find('>')?;
    let close = "</a>";
    let ci = find_ci(&seg[open_gt..], close)?;
    let start = open_gt + 1;
    let end = open_gt + ci;
    Some((
        decode_entities(&href),
        seg[start..end].to_string(),
        end + close.len(),
    ))
}

fn extract_href(seg: &str) -> Option<String> {
    let lower = seg.to_ascii_lowercase();
    let h = lower.find("href=")?;
    let rest = seg[h + 5..].trim_start();
    let (quote, body) = if let Some(r) = rest.strip_prefix('"') {
        ('"', r)
    } else if let Some(r) = rest.strip_prefix('\'') {
        ('\'', r)
    } else {
        return None;
    };
    let end = body.find(quote)?;
    Some(body[..end].to_string())
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
    fn html_to_text_strips_tags_scripts_and_entities() {
        let html = r#"<html><head><title>T</title></head>
            <body><script>var x=1;</script>
            <h1>标题</h1><p>第一段 &amp; 实体</p>
            <style>.a{color:red}</style>
            <p>第二段&nbsp;结尾</p></body></html>"#;
        let t = html_to_text(html);
        assert!(t.contains("标题"), "{t}");
        assert!(t.contains("第一段 & 实体"), "{t}");
        assert!(t.contains("第二段 结尾"), "{t}");
        assert!(!t.contains("var x=1"), "script 内容必须去掉：{t}");
        assert!(!t.contains("color:red"), "style 内容必须去掉：{t}");
        assert!(!t.contains('<'), "不得残留标签：{t}");
    }

    #[test]
    fn html_to_text_numeric_entities() {
        let t = html_to_text("<p>&#65;&#x4e2d;文</p>");
        assert!(t.contains("A中文"), "{t}");
    }

    #[test]
    fn dechunk_basic() {
        let raw = b"5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        assert_eq!(String::from_utf8_lossy(&dechunk(raw)), "hello world");
    }

    #[test]
    fn strip_between_removes_comments() {
        assert_eq!(strip_between("a<!-- x -->b", "<!--", "-->"), "ab");
    }

    #[test]
    fn extract_href_quoted() {
        assert_eq!(extract_href(r#"<a class="x" href="https://a.com""#).as_deref(), Some("https://a.com"));
        assert_eq!(extract_href("<a href='https://b.com'>").as_deref(), Some("https://b.com"));
    }

    #[test]
    fn anchor_in_gets_href_and_inner() {
        let (link, inner, _) = anchor_in(r#"<a href="https://a.com"><b>标题</b></a>"#).unwrap();
        assert_eq!(link, "https://a.com");
        assert!(inner.contains("标题"));
    }

    #[test]
    fn h3_links_finds_result() {
        let html = r#"<h3 class="t"><a href="https://x.com/p">结果标题</a></h3>"#;
        let links = h3_links(html);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].0, "https://x.com/p");
        assert!(links[0].1.contains("结果标题"));
    }

    #[test]
    fn url_encode_cjk_and_space() {
        assert_eq!(url_encode("残响 之河"), "%E6%AE%8B%E5%93%8D+%E4%B9%8B%E6%B2%B3");
    }

    #[test]
    fn extract_uddg_decodes() {
        let s = "https://duckduckgo.com/l/?uddg=https%3A%2F%2Fa.com%2Fx&rut=1";
        assert_eq!(extract_uddg(s).as_deref(), Some("https://a.com/x"));
    }

    #[test]
    fn fetch_text_rejects_bad_scheme() {
        assert!(fetch_text("ftp://x", 100).is_err());
    }

    #[test]
    fn detect_charset_from_header_and_meta() {
        assert_eq!(detect_charset("text/html; charset=gbk", b""), "gbk");
        assert_eq!(detect_charset("text/html; charset=\"GB2312\"", b""), "gb2312");
        let meta = b"<html><head><meta charset=\"gbk\"></head>";
        assert_eq!(detect_charset("text/html", meta), "gbk");
        assert_eq!(detect_charset("text/html", b"<html>"), "utf-8");
    }

    #[test]
    fn decode_bytes_handles_gbk() {
        // "第一章"（GBK 编码）必须能正确解码，而不是乱码
        let gbk = [
            0xb5, 0xda, 0xd2, 0xbb, 0xd5, 0xc2, // 第一章
        ];
        assert_eq!(decode_bytes(&gbk, "gbk"), "第一章");
        assert_eq!(decode_bytes(&gbk, "gb2312"), "第一章");
        // UTF-8 正常路径
        assert_eq!(decode_bytes("残响".as_bytes(), "utf-8"), "残响");
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

    #[test]
    fn search_rejects_empty_query() {
        assert!(search("  ", "baidu", 5).is_err());
    }

    #[test]
    fn extract_baidu_cards_matches_h3_class_t() {
        let html = r#"<div><h3 class="t"><a href="https://x.com/a">结果一</a></h3></div>"#;
        let hits = extract_baidu_cards(html, 5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "结果一");
        assert_eq!(hits[0].url, "https://x.com/a");
    }

    #[test]
    fn extract_bing_algo_gets_title_and_snippet() {
        let html = r#"<li class="b_algo"><h2><a href="https://y.com/b">标题乙</a></h2>
                      <div class="b_caption"><p>这是摘要文字</p></div></li>"#;
        let hits = extract_bing_algo(html, 5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "标题乙");
        assert_eq!(hits[0].snippet, "这是摘要文字");
    }
}
