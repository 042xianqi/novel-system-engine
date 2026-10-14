//! paragraph_gate.rs —— **P1-PARAGRAPH** 段落格式门禁（修复型，永不 P0）。
//!
//! 背景：作者模板 §1.2 硬性规定「单段 40~80 字（硬上限 100），段数 ≈ 字数÷55、
//! 逗:句 1.7~2.8」，但提示词只是**软约束**，动态注入占比高时 LLM 常违反
//! （实测第1章：18 段里 17 段 >100 字，逗:句 3.15）。本模块提供**确定性**校验
//! 与**仅排版层**修复：
//!
//! * 段 <40 汉字 → 与相邻正文段合并（独立对话短句豁免）
//! * 段 >80 汉字 → 在句末标点（`。；！？……`）处切分，**绝不拆断完整句子**
//! * 逗/句 比值 → 仅统计告警，**不改写任何文字**
//!
//! 铁律：**一字不改**。只增删段落之间的换行，不增删改任何叙事 / 对话 / 形容词。
//! 该门禁**只产 P1**，绝不 reject 整章。

/// 段落格式修复结果。
#[derive(Debug, Clone)]
pub struct ParaFix {
    /// 修复后的全文（仅分段变化；无变化时与输入逐字相同）
    pub fixed: String,
    /// 被合并 / 切分的段落数（>0 表示发生了排版修复）
    pub fixed_count: usize,
    /// 正文逗号数
    pub comma: usize,
    /// 正文句末标点数（。！？）
    pub period: usize,
    /// 逗:句 比值
    pub ratio: f64,
}

/// 单段正常区间（汉字数）
const PARA_MIN: usize = 40;
/// 单段区间上限（汉字数）
const PARA_MAX: usize = 80;

/// 统计汉字数（仅 `\u{4e00}~=~\u{9fff}`，不计标点 / 空白 / 拉丁）。
fn hanzi_count(s: &str) -> usize {
    s.chars()
        .filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c))
        .count()
}

/// 整块仅为对话（`「…」` / `"…"` / `"…"`）→ 豁免，不参与合并。
fn is_dialogue_only(s: &str) -> bool {
    let t = s.trim();
    (t.starts_with('「') && t.ends_with('」'))
        || (t.starts_with('“') && t.ends_with('”'))
        || (t.starts_with('"') && t.ends_with('"'))
        || (t.starts_with('‘') && t.ends_with('’'))
}

/// markdown 结构块（标题 / 引用 / 表格 / 列表）→ 一律跳过，不动。
fn is_structural(s: &str) -> bool {
    let t = s.trim_start();
    t.starts_with('#')
        || t.starts_with('>')
        || t.starts_with('|')
        || t.starts_with("- ")
        || t.starts_with("* ")
}

/// 按句末标点切句（保留标点）。`……` 视为一个整体，不切中间。
fn split_sentences(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for ch in s.chars() {
        cur.push(ch);
        let cut = match ch {
            '。' | '；' | '！' | '？' => true,
            '…' => !cur.ends_with("……"),
            _ => false,
        };
        if cut {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// 把正文按空行切成块（块内多行 join）。
fn split_blocks(raw: &str) -> Vec<String> {
    let mut blocks: Vec<String> = Vec::new();
    let mut cur: Vec<String> = Vec::new();
    for line in raw.lines() {
        if line.trim().is_empty() {
            if !cur.is_empty() {
                blocks.push(cur.join("\n"));
                cur.clear();
            }
        } else {
            cur.push(line.to_string());
        }
    }
    if !cur.is_empty() {
        blocks.push(cur.join("\n"));
    }
    blocks
}

/// 确定性段落修复 + 逗句比统计。
pub fn fix_paragraphs(raw: &str) -> ParaFix {
    let comma = raw.matches('，').count() + raw.matches(',').count();
    let period = raw.matches('。').count() + raw.matches('！').count() + raw.matches('？').count();
    let ratio = if period > 0 { comma as f64 / period as f64 } else { 0.0 };

    let blocks = split_blocks(raw);
    let mut out_blocks: Vec<String> = Vec::new();
    let mut fixed_count = 0usize;

    for b in blocks {
        if is_structural(&b) {
            out_blocks.push(b);
            continue;
        }
        let hz = hanzi_count(&b);
        if hz < PARA_MIN {
            // 独立对话短句豁免
            if is_dialogue_only(&b) {
                out_blocks.push(b);
                continue;
            }
            // 尝试与相邻前正文段合并
            let merged = if let Some(prev) = out_blocks.last_mut() {
                if !is_structural(prev)
                    && !is_dialogue_only(prev)
                    && hanzi_count(prev) + hz <= PARA_MAX
                {
                    prev.push_str(&b);
                    true
                } else {
                    false
                }
            } else {
                false
            };
            if merged {
                fixed_count += 1;
            } else {
                out_blocks.push(b);
            }
        } else if hz > PARA_MAX {
            // 在句末标点处切分
            let sents = split_sentences(&b);
            let mut pieces: Vec<String> = Vec::new();
            let mut buf = String::new();
            let mut bz = 0usize;
            for s in sents {
                let shz = hanzi_count(&s);
                if !buf.is_empty() && bz + shz > PARA_MAX {
                    pieces.push(std::mem::take(&mut buf));
                    bz = 0;
                }
                buf.push_str(&s);
                bz += shz;
            }
            if !buf.is_empty() {
                pieces.push(buf);
            }
            if pieces.len() > 1 {
                fixed_count += 1;
                out_blocks.extend(pieces);
            } else {
                // 单句 >80 且无法在句末标点切 → 豁免（不拆断完整句子）
                out_blocks.push(b);
            }
        } else {
            out_blocks.push(b);
        }
    }

    let mut fixed = out_blocks.join("\n\n");
    fixed.push('\n');

    ParaFix { fixed, fixed_count, comma, period, ratio }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 长段必须在句末标点处切开，且**一字不增不减**。
    #[test]
    fn splits_long_paragraph_without_changing_text() {
        let s: String = std::iter::repeat("他往前走，看着天色。").take(20).collect(); // 每句 10 汉字
        let r = fix_paragraphs(&s);
        assert!(r.fixed_count > 0, "应发生切分");
        // 去空白后逐字相等
        let norm = |t: &str| t.chars().filter(|c| !c.is_whitespace()).collect::<String>();
        assert_eq!(norm(&s), norm(&r.fixed), "切分不得增删任何字");
        // 每片 ≤80
        for p in r.fixed.split("\n\n") {
            assert!(hanzi_count(p) <= PARA_MAX, "切分后仍超 80: {}", hanzi_count(p));
        }
    }

    /// 短段与相邻段合并，且不动独立对话。
    #[test]
    fn merges_short_paragraph_skips_dialogue() {
        let s = "他点点头。\n\n“爷，是我。”\n\n他站着没动。";
        let r = fix_paragraphs(s);
        let norm = |t: &str| t.chars().filter(|c| !c.is_whitespace()).collect::<String>();
        assert_eq!(norm(s), norm(&r.fixed), "合并不得增删任何字");
        // 对话行仍独立存在
        assert!(r.fixed.contains("“爷，是我。”"));
    }

    /// 正常段落原样不动。
    #[test]
    fn normal_paragraph_untouched() {
        let p = "他扶着水底的石头一点点往岸边挪，衣服湿透了贴在身上，沉得像背着半袋粮食，手脚都不听使唤。";
        let r = fix_paragraphs(p);
        assert_eq!(r.fixed_count, 0);
        assert!(r.fixed.contains(p));
    }

    /// 逗句比统计正确。
    #[test]
    fn comma_period_ratio_counted() {
        let r = fix_paragraphs("一，二，三。四。");
        assert_eq!(r.comma, 2);
        assert_eq!(r.period, 2);
        assert!((r.ratio - 1.0).abs() < 1e-6);
    }

    /// markdown 标题 / 引用块跳过。
    #[test]
    fn structural_blocks_skipped() {
        let s = "# 标题\n\n> 引用\n\n正常的一段正文，四十个汉字以上才不会被合并处理掉这里补齐足够长度。";
        let r = fix_paragraphs(s);
        assert!(r.fixed.contains("# 标题"));
        assert!(r.fixed.contains("> 引用"));
    }
}