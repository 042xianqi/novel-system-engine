//! HTML → 纯文本（**无依赖**的保守实现）。
//!
//! 从 `web.rs` 拆出（架构门禁：单文件 ≤ 700 行）。
//! 这些函数只做字符串层处理，与网络无关，故独立成模块。

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

pub(crate) fn find_ci(hay: &str, needle: &str) -> Option<usize> {
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

pub(crate) fn decode_entities(s: &str) -> String {
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
pub(crate) fn body_to_text(content_type: &str, bytes: &[u8]) -> String {
    let cs = detect_charset(content_type, bytes);
    decode_bytes(bytes, &cs)
}



#[cfg(test)]
mod tests {
    use super::*;

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
    fn strip_between_removes_comments() {
        assert_eq!(strip_between("a<!-- x -->b", "<!--", "-->"), "ab");
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
}
