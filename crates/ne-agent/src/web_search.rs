//! 免 API Key 搜索（照太墟 `/opt/taixu/scripts/websearch_mcp_server.py`）。
//!
//! 从 `web.rs` 拆出（架构门禁）。只依赖 `web::get`/`web_html::html_to_text`，
//! 与 HTML 解析解耦，便于单独演进（引擎改版只动这里）。

use anyhow::{anyhow, Result};

use crate::web::{get_with_ua, DEFAULT_MAX_BYTES};
use crate::web_html::{decode_entities, find_ci, html_to_text};

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
    // 用浏览器 UA（搜索引擎会拒普通 UA；照太墟做法）
    let r = get_with_ua(
        url,
        DEFAULT_MAX_BYTES,
        std::time::Duration::from_secs(15),
        Some(crate::web::search_user_agent()),
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
