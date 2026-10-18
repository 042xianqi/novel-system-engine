//! 细纲解析（共享底层）——**唯一真值源**。
//!
//! `health`（雷同检测/启动自检）与 `outline_rebuild`（拍重建）都必须走这里，
//! 避免两套解析口径漂移（历史教训：Rust/Python 双解析分叉）。
//!
//! 章节块 = `### 第N章 标题` 起、到下一个 `### 第` 前一行止的原始文本。

/// 一章：(章号, 标题, 原始块文本)。
pub type Chunk = (u32, String, String);

/// 找第一个 `### 第` 的字节偏移。
pub fn find_chapter_start(txt: &str) -> Option<usize> {
    let mut off = 0usize;
    for line in txt.split_inclusive('\n') {
        if line.starts_with("### 第") {
            return Some(off);
        }
        off += line.len();
    }
    None
}

/// 按行首 `### 第` 切块（保留每块自身的标题行）。
pub fn split_at_chapter_heads(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for line in s.split_inclusive('\n') {
        if line.starts_with("### 第") && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        cur.push_str(line);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// 从块首行解析 (章号, 标题)。
pub fn parse_head(block: &str) -> Option<(u32, String)> {
    let first = block.lines().next()?;
    let rest = first.strip_prefix("### 第")?;
    let cpos = rest.find('章')?;
    let no: u32 = rest[..cpos].trim().parse().ok()?;
    let title = rest[cpos + '章'.len_utf8()..].trim().to_string();
    Some((no, title))
}

/// 拆分细纲 → (header, [章块])。header 为第一个章标题之前的内容。
pub fn split_chapters(txt: &str) -> (String, Vec<Chunk>) {
    let Some(pos) = find_chapter_start(txt) else {
        return (txt.to_string(), Vec::new());
    };
    let header = txt[..pos].to_string();
    let mut chunks = Vec::new();
    for part in split_at_chapter_heads(&txt[pos..]) {
        if let Some((no, title)) = parse_head(&part) {
            chunks.push((no, title, part));
        }
    }
    (header, chunks)
}

/// 取 `**键**：值`（bold）或 `- 键：值`（非 bold）。
///
/// bold 用**子串查找**：实际行首是 `- **目标**：…`（`**` 前有 `- `）。
pub fn grab(body: &str, key: &str, bold: bool) -> String {
    for line in body.lines() {
        if bold {
            let needle = format!("**{key}**");
            if let Some(pos) = line.find(&needle) {
                let after = &line[pos + needle.len()..];
                let trimmed = after.trim_start_matches(['：', ':', ' ', '\u{3000}']);
                if trimmed.len() != after.len() {
                    return trimmed.trim().to_string();
                }
            }
        } else {
            let t = line.trim();
            let needle = format!("- {key}");
            if let Some(rest) = t.strip_prefix(&needle) {
                let after = rest.trim_start_matches(['：', ':', ' ', '\u{3000}']);
                if after.len() != rest.len() {
                    return after.trim().to_string();
                }
            }
        }
    }
    String::new()
}

/// 「概要四字段」（目标/在场/阻碍/选择）拼接签名，供雷同检测。
/// 四字段全空则该章不进签名表。
pub fn chapter_sigs(txt: &str) -> Vec<(u32, String, String)> {
    let sig_keys = ["目标", "在场", "阻碍", "选择"];
    let mut out = Vec::new();
    for (no, title, body) in split_chapters(txt).1 {
        let vals: Vec<String> = sig_keys.iter().map(|k| grab(&body, k, true)).collect();
        let sig = vals.join("\u{1}");
        if !sig.trim_matches('\u{1}').is_empty() {
            out.push((no, title, sig));
        }
    }
    out
}

/// 找「连续 ≥ `min_run` 章签名完全相同」的组 → (起始章, 结束章, 长度)。
pub fn dup_runs(sigs: &[(u32, String, String)], min_run: usize) -> Vec<(u32, u32, usize)> {
    let mut groups = Vec::new();
    let mut i = 0;
    while i < sigs.len() {
        let mut j = i + 1;
        while j < sigs.len() && sigs[j].2 == sigs[i].2 && !sigs[i].2.is_empty() {
            j += 1;
        }
        if j - i >= min_run {
            groups.push((sigs[i].0, sigs[j - 1].0, j - i));
        }
        i = j;
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# 卷头
### 第1章 涧底醒
**概要**
- **目标**：确认自己还活着
- **在场**：陈寂
- **阻碍**：涧水齐腰
- **选择**：上岸
**决定→代价→变化**
- 决定：只说滑倒了
- 变化：祖父开始疑心
### 第2章 涧底醒·阻碍具象
- **目标**：确认自己还活着
- **在场**：陈寂
- **阻碍**：涧水齐腰
- **选择**：上岸
";

    #[test]
    fn split_header_and_chunks() {
        let (h, chs) = split_chapters(SAMPLE);
        assert!(h.contains("卷头"));
        assert_eq!(chs.len(), 2);
        assert_eq!(chs[0].0, 1);
        assert_eq!(chs[0].1, "涧底醒");
        assert_eq!(chs[1].0, 2);
    }

    #[test]
    fn grab_handles_bold_and_plain() {
        let (_, chs) = split_chapters(SAMPLE);
        assert_eq!(grab(&chs[0].2, "目标", true), "确认自己还活着");
        assert_eq!(grab(&chs[0].2, "决定", false), "只说滑倒了");
        assert_eq!(grab(&chs[0].2, "变化", false), "祖父开始疑心");
        assert_eq!(grab(&chs[0].2, "不存在", true), "");
    }

    #[test]
    fn sigs_and_dup_detect_copied() {
        let sigs = chapter_sigs(SAMPLE);
        assert_eq!(sigs.len(), 2);
        assert_eq!(sigs[0].2, sigs[1].2, "两章四字段相同");
        assert!(dup_runs(&sigs, 3).is_empty(), "2 章不构成 ≥3 组");
    }

    #[test]
    fn writeback_roundtrip_preserves_header() {
        // header + 各块拼接应还原（除末尾换行归一）
        let (h, chs) = split_chapters(SAMPLE);
        let joined: String = h + &chs.iter().map(|c| c.2.as_str()).collect::<String>();
        assert!(joined.starts_with("# 卷头"));
        assert!(joined.contains("### 第2章 涧底醒·阻碍具象"));
    }
}