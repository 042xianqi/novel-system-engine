//! 工具输出差异化截断（纯函数）—— 逐字对照太墟 `harness/effects/ToolOutputRetention.kt`。
//!
//! 不同工具的关键信息位置不同，截断方向随工具切换：
//! - [`OutputRetention::Tail`]：命令执行 / 构建 / 进程日志——panic、断言失败、编译错误
//!   永远在末尾，保留尾部才能让模型直击报错核心。
//! - [`OutputRetention::Head`]：read / search——关键信息通常在最前面（文件头、命中项）。
//!
//! # 与 `context.rs::compact_output` 的关系
//!
//! 那是**统一档**；本模块是**按工具方向 + 行对齐 + 超长行折叠**的增量。接线进 `context.rs`
//! 属调用方职责，本模块只交纯函数。

/// 保留方向（太墟 `OutputRetention`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputRetention {
    Head,
    Tail,
}

/// 按工具名决定保留方向（太墟 `forTool`）：比较时 trim + 小写。
pub fn for_tool(tool_name: Option<&str>) -> OutputRetention {
    match tool_name.map(|s| s.trim().to_lowercase()).as_deref() {
        Some("base") | Some("process") | Some("build_script") => OutputRetention::Tail,
        _ => OutputRetention::Head,
    }
}

/// 头部截断并对齐完整行：优先在字符预算内最后一个换行处切分；单行超预算时退化为硬截断。
/// **按 char 计**（避免切坏 UTF-8）。
pub fn keep_head_whole_lines(text: &str, max_chars: usize) -> String {
    let total = text.chars().count();
    if total <= max_chars {
        return text.to_string();
    }
    // 找 ≤ max_chars 的最后一个 '\n' 的 char 下标
    let mut cut: Option<usize> = None;
    for (ci, ch) in text.chars().enumerate() {
        if ci >= max_chars {
            break;
        }
        if ch == '\n' {
            cut = Some(ci);
        }
    }
    let cut = match cut {
        Some(c) if c > 0 => c,
        _ => max_chars,
    };
    text.chars().take(cut).collect()
}

/// 尾部截断并对齐完整行：从末尾往前取整行，保证不切在行中。**按 char 计**。
pub fn keep_tail_whole_lines(text: &str, max_chars: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    if len <= max_chars {
        return text.to_string();
    }
    let from = len.saturating_sub(max_chars); // max(0, len-max)
    // 找 from 之后第一个 '\n'（含 from 位置起），取其下一位
    let mut cut: Option<usize> = None;
    for (ci, ch) in chars.iter().enumerate().skip(from) {
        if *ch == '\n' {
            cut = Some(ci + 1);
            break;
        }
    }
    let cut = cut.unwrap_or(len - max_chars);
    chars[cut.min(len)..].iter().collect()
}

/// 折叠超长单行（默认 2000）：按行截断策略对单行会退化为保留巨量字符的整行，
/// 故折叠为「头部 + 中文提示 + 尾部」，并保证结果行不超 `max_line_chars`。
pub fn fold_overlong_lines(text: &str, max_line_chars: usize) -> String {
    let max_line_chars = if max_line_chars == 0 { 2000 } else { max_line_chars };
    if !text.lines().any(|l| l.chars().count() > max_line_chars) {
        return text.to_string();
    }
    let head = max_line_chars / 2;
    let tail = max_line_chars / 4;
    text.lines()
        .map(|line| {
            let lc = line.chars().count();
            if lc <= max_line_chars {
                line.to_string()
            } else {
                let h: String = line.chars().take(head).collect();
                let t: String = line.chars().skip(lc - tail).collect();
                format!(
                    "{h}\n[单行内容过长已折叠：本行共 {lc} 字符，疑似压缩/混淆/最小化文件，整行可读性极低。如需定位内容请用 grep -o 提取匹配片段、sed -n 按行号或字节段查看，不要原样输出整行。]\n{t}"
                )
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use super::*;

    #[test]
    fn command_build_and_process_logs_keep_the_tail() {
        assert_eq!(for_tool(Some("base")), OutputRetention::Tail);
        assert_eq!(for_tool(Some("process")), OutputRetention::Tail);
        assert_eq!(for_tool(Some("build_script")), OutputRetention::Tail);
        assert_eq!(for_tool(Some("BASE")), OutputRetention::Tail);
        assert_eq!(for_tool(Some(" build_script ")), OutputRetention::Tail);
    }

    #[test]
    fn read_and_search_keep_the_head() {
        assert_eq!(for_tool(Some("read")), OutputRetention::Head);
        assert_eq!(for_tool(Some("history_search")), OutputRetention::Head);
        assert_eq!(for_tool(None), OutputRetention::Head);
    }

    #[test]
    fn head_truncation_keeps_whole_lines_within_budget() {
        let text = "aaaa\nbbbb\ncccc\n";
        // 预算 7 落在第二行内：回退到第一个换行处，只保留 "aaaa"
        assert_eq!(keep_head_whole_lines(text, 7), "aaaa");
        // 单行超预算时退化为硬截断
        assert_eq!(keep_head_whole_lines("aaaaaaaaaa", 4), "aaaa");
        // 未超预算原样返回
        assert_eq!(keep_head_whole_lines(text, 100), text);
    }

    #[test]
    fn tail_truncation_keeps_whole_lines_within_budget() {
        let text = "aaaa\nbbbb\ncccc\n";
        // 预算 7 从末尾回退：从第二个换行之后开始，保留 "cccc\n"
        assert_eq!(keep_tail_whole_lines(text, 7), "cccc\n");
        // 单行超预算时退化为硬截断
        assert_eq!(keep_tail_whole_lines("aaaaaaaaaa", 4), "aaaa");
        assert_eq!(keep_tail_whole_lines(text, 100), text);
    }

    #[test]
    fn normal_multi_line_output_passes_through_folding_untouched() {
        let text = "aaaa\nbbbb\ncccc\n";
        assert_eq!(fold_overlong_lines(text, 2000), text);
    }

    #[test]
    fn overlong_single_line_is_folded_to_head_marker_tail() {
        let line = "x".repeat(100_000);
        let folded = fold_overlong_lines(&line, 2000);
        // 折叠后不再有超预算的行（提示占的行也计入）
        assert!(folded.lines().all(|l| l.chars().count() <= 2000));
        assert!(folded.starts_with(&"x".repeat(1000)));
        assert!(folded.ends_with(&"x".repeat(500)));
        assert!(folded.contains("已折叠") && folded.contains("100000"));
        // 多行文本只折叠超长行，其余原样保留
        let mixed = format!("ok\n{line}\nfine");
        let folded_mixed = fold_overlong_lines(&mixed, 2000);
        assert!(folded_mixed.starts_with("ok\n"));
        assert!(folded_mixed.ends_with("fine"));
    }
}