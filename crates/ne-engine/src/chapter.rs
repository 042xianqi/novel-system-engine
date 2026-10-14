//! 章节工程 —— `chapter_engine.py` 的 Rust 移植（提示词构建 + 确定性质检 + 落盘）。
//!
//! 事实源：`05_管线框架/engine/chapter_engine.py`（886 行）
//!
//! 分两类：
//!   * **确定性可移植**（照抄）：`deterministic_check` / `write_chapter` / `_strip_narrative` /
//!     `_neutralize_myth` / `scene_dims` 路由表 / 常量表
//!   * **需外部数据**（走 `ChapterCtx` trait）：`build_prompt` / `build_knowledge_injection`
//!
//! LLM 调用不在此处（由 `crate::llm` 负责，对齐 Python 的分层）。

use anyhow::{Context, Result};
use regex::Regex;
use serde_json::{json, Map, Value};
use std::path::Path;

/// AI 痕迹词（照抄 AI_TRACE_WORDS）。
pub const AI_TRACE_WORDS: [&str; 8] =
    ["不禁", "竟然", "不可思议", "值得一提", "事实上", "众所周知", "缓缓", "微微"];
/// 正典禁词（照抄 BANNED_WORDS）。
pub const BANNED_WORDS: [&str; 3] = ["归墟", "熵兽", "热寂前夜"];
/// 字数区间（照抄 MIN_WORDS / MAX_WORDS）。
pub const MIN_WORDS: usize = 2500;
pub const MAX_WORDS: usize = 4000;

/// 场景 → 手法库路由（照抄 scene_dims）。
pub fn scene_dims(scene: &str) -> &'static str {
    match scene {
        "日常" => "A(高)+B+C(1层)，辅 F",
        "修炼" => "F+C(2层)+A(低)",
        "战斗" => "E+A(中)+C(1层)",
        "大场面" => "D+A(高)+E(片段)+C(2层)",
        "社交" => "B+C(1-2层)+A(中)",
        "心理" => "C(3层)+A(扭曲)",
        "探索" => "A(高)+B+C(1层)",
        "过渡" => "A(中)+时间跳",
        _ => "A(中)+B+C(1层)",
    }
}

/// 来源神话 6 词 → 中性词（照抄 MYTH_NEUTRAL）。
pub const MYTH_NEUTRAL: [(&str, &str); 6] = [
    ("存在之基", "锚点已建立"),
    ("存在之核", "锚点已建立"),
    ("高维观测者文明", "未知来源"),
    ("高维观测者", "未知来源"),
    ("阿莱夫·零", "未知来源"),
    ("金色立方体", "未知来源"),
];

/// 中性化正文注入文本（照抄 _neutralize_myth）。
pub fn neutralize_myth(md: &str) -> String {
    let mut s = md.to_string();
    for (a, b) in MYTH_NEUTRAL {
        s = s.replace(a, b);
    }
    s
}

/// 剥离叙事体（照抄 _strip_narrative）：剔除「## 一、二、」叙事章 + 连续 >300 字叙事段。
pub fn strip_narrative(md: &str) -> String {
    let re_narr_head = Regex::new(r"^##\s*[一二三四五六七八九十]+、").unwrap();
    let lines: Vec<&str> = md.lines().collect();
    let n = lines.len();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < n {
        let l = lines[i];
        if re_narr_head.is_match(l) {
            // 跳到该叙事章结束（下一个 ## 开头，或 EOF）
            i += 1;
            while i < n && !lines[i].starts_with("##") {
                i += 1;
            }
            continue;
        }
        out.push(l.to_string());
        i += 1;
    }
    // 再剥连续 >300 字叙事段
    let mut out2: Vec<String> = Vec::new();
    let mut para: Vec<&String> = Vec::new();
    let flush = |para: &mut Vec<&String>, out2: &mut Vec<String>| {
        if para.is_empty() {
            return;
        }
        let joined: String = para.iter().map(|x| x.trim()).collect();
        if joined.chars().count() > 300 {
            out2.push("（叙事段已剥离·见事件卡字段）".to_string());
        } else {
            out2.extend(para.iter().map(|s| s.to_string()));
        }
        para.clear();
    };
    for l in &out {
        let is_keep = l.starts_with('#')
            || l.starts_with('|')
            || l.trim().is_empty()
            || l.starts_with("- 所属")
            || l.starts_with("> 【");
        if is_keep {
            flush(&mut para, &mut out2);
            out2.push(l.clone());
        } else {
            para.push(l);
        }
    }
    flush(&mut para, &mut out2);
    out2.join("\n")
}


/// Python `round(x, n)`（half-to-even）。
fn py_round(x: f64, digits: u32) -> f64 {
    let m = 10f64.powi(digits as i32);
    let scaled = x * m;
    let floor = scaled.floor();
    let diff = scaled - floor;
    let r = if (diff - 0.5).abs() < 1e-9 {
        if (floor as i64) % 2 == 0 { floor } else { floor + 1.0 }
    } else {
        scaled.round()
    };
    r / m
}

/// Python dict repr：`{'a': 1, 'b': 2}`（对齐 f"{dict}" 的输出）。
fn py_dict_repr(hits: &[(&str, usize)]) -> String {
    let inner: Vec<String> = hits.iter().map(|(k, v)| format!("'{k}': {v}")).collect();
    format!("{{{}}}", inner.join(", "))
}

/// 确定性质检结果（照抄 deterministic_check 的返回结构）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct Findings {
    pub p0: Vec<Finding>,
    pub p1: Vec<Finding>,
    pub stats: Value,
    pub verdict: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Finding {
    pub dim: String,
    pub issue: String,
    pub action: String,
}

fn f(dim: &str, issue: String, action: &str) -> Finding {
    Finding { dim: dim.into(), issue, action: action.into() }
}

/// 确定性质检（照抄 `deterministic_check`，逐条对齐阈值与输出）。
///
/// 使用默认正典禁词（《残响之河》`BANNED_WORDS`）。新书请用
/// [`deterministic_check_with`] 传入 `book_profile.banned_words()`。
pub fn deterministic_check(text: &str, _chapter_no: u32) -> Findings {
    deterministic_check_with(text, _chapter_no, &BANNED_WORDS)
}

/// 确定性质检（禁词表可注入版）。
///
/// `banned_words` 由调用方给出（通常 = `cfg.book_profile.banned_words()`）；
/// 缺省传入 [`BANNED_WORDS`] 时与旧行为逐字一致。
pub fn deterministic_check_with(text: &str, _chapter_no: u32, banned_words: &[&str]) -> Findings {
    let mut p0 = Vec::new();
    let mut p1 = Vec::new();
    let no_ws: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let words = no_ws.chars().count();
    let excl = text.matches('!').count() + text.matches('！').count();
    let ai_hits: Vec<(&str, usize)> = AI_TRACE_WORDS
        .iter()
        .filter(|w| text.contains(**w))
        .map(|w| (*w, text.matches(*w).count()))
        .collect();
    let banned_hits: Vec<(&str, usize)> = banned_words
        .iter()
        .filter(|w| text.contains(**w))
        .map(|w| (*w, text.matches(*w).count()))
        .collect();
    // 排比：连续同结构（紧邻 ≥3）
    let repet = Regex::new(r"(不是[^。，\n]{1,8}[，,]){2,}不是").unwrap().is_match(text)
        || Regex::new(r"(在[^。]{1,6}。){3,}").unwrap().is_match(text);

    // 章末：最后一个句末标点之后的句子
    let seg_re = Regex::new(r"[。！？!?…\n]").unwrap();
    let trimmed = text.trim();
    let mut last_sentence = String::new();
    let segs: Vec<&str> = seg_re.split(trimmed).collect();
    for s in segs.iter().rev() {
        if !s.trim().is_empty() {
            last_sentence = s.trim().to_string();
            break;
        }
    }

    // 段落粒度
    let para_re = Regex::new(r"\n\s*\n").unwrap();
    let para_lens: Vec<usize> = para_re
        .split(trimmed)
        .filter(|p| !p.trim().is_empty())
        .map(|p| p.chars().filter(|c| !c.is_whitespace()).count())
        .collect();
    let max_para = para_lens.iter().copied().max().unwrap_or(0);
    let para_count = para_lens.len();
    if max_para > 200 {
        p0.push(f(
            "B.段落",
            format!("最长段 {max_para} 字 >200（单段硬上限）"),
            "长段按动作/转折/情绪拆成 2-4 段（§1 单段≤200字·全章18-25段）",
        ));
    } else if para_count > 25 {
        p1.push(f(
            "B.段落",
            format!("段数 {para_count} >25（拆过碎，全章目标18-25段）"),
            "相邻短段合并回 18-25 段，别拆太碎（句长目标25-40）",
        ));
    }

    // 断句节奏
    let comma_cnt = text.matches('，').count() + text.matches(',').count();
    let period_cnt = text.matches('。').count();
    let avg_sent = if period_cnt > 0 { words as f64 / period_cnt as f64 } else { 0.0 };
    let comma_ratio = if period_cnt > 0 { comma_cnt as f64 / period_cnt as f64 } else { 0.0 };
    if comma_ratio > 3.0 {
        p1.push(f(
            "B.断句节奏",
            format!("逗号:句号={comma_ratio:.2} >3.0（手稿≤2.8·逗号流不收句）"),
            "每2-3个逗号落一个句号，句号密度提到≥26/千字",
        ));
    }
    if avg_sent > 45.0 {
        p1.push(f(
            "B.断句节奏",
            format!("平均句长{avg_sent:.1}>45（参考值25-40偏长）"),
            "长句在逗号处断开，句号收束",
        ));
    } else if avg_sent < 22.0 && period_cnt > 0 {
        p1.push(f(
            "B.断句节奏",
            format!("平均句长{avg_sent:.1}<22（电报体回潮）"),
            "碎句合并，主句25-40字逗号串联",
        ));
    }

    if !banned_hits.is_empty() {
        p0.push(f("A.世界观", format!("禁词 {}", py_dict_repr(&banned_hits)), "改"));
    }
    if !ai_hits.is_empty() {
        p1.push(f("B.风格", format!("AI痕迹词 {}", py_dict_repr(&ai_hits)), "替换"));
    }
    if excl > 1 {
        p1.push(f("B.风格", format!("感叹号 {excl} 个 >1"), "删到≤1"));
    }
    if repet {
        p1.push(f("B.风格", "疑似排比堆叠".into(), "拆散句"));
    }
    if !(MIN_WORDS..=MAX_WORDS).contains(&words) {
        p1.push(f(
            "J.字数",
            format!("{words}字 超出{MIN_WORDS}-{MAX_WORDS}"),
            "补/砍",
        ));
    }
    if last_sentence.chars().filter(|c| *c != ' ').count() > 10 {
        p1.push(f(
            "B.章末",
            format!("末句超10字：{}", last_sentence.chars().take(40).collect::<String>()),
            "收尾",
        ));
    }

    let verdict = if !p0.is_empty() {
        "reject"
    } else if !p1.is_empty() {
        "fix"
    } else {
        "pass"
    }
    .to_string();

    let comma_density = if words > 0 { comma_cnt as f64 / words as f64 * 1000.0 } else { 0.0 };
    let period_density = if words > 0 { period_cnt as f64 / words as f64 * 1000.0 } else { 0.0 };
    let stats = json!({
        "words": words,
        "excl": excl,
        "ai_words": ai_hits.iter().map(|(_, n)| *n).sum::<usize>(),
        "banned": banned_hits.iter().map(|(_, n)| *n).sum::<usize>(),
        "max_para": max_para,
        "para_count": para_count,
        "avg_sent": py_round(avg_sent, 1),
        "comma_density": py_round(comma_density, 1),
        "period_density": py_round(period_density, 1),
        "comma_ratio": py_round(comma_ratio, 2),
    });

    Findings { p0, p1, stats, verdict }
}

/// 落盘第 N 章（照抄 write_chapter；路径由调用方给定，不硬编码）。
pub fn write_chapter(root: &Path, text: &str, chapter: u32, volume: &str) -> Result<String> {
    let dir = root.join("04_正文").join(volume);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("建目录失败：{}", dir.display()))?;
    let path = dir.join(format!("第{chapter}章.md"));
    std::fs::write(&path, format!("{}\n", text.trim()))
        .with_context(|| format!("写文件失败：{}", path.display()))?;
    Ok(path.to_string_lossy().to_string())
}

/// 外部数据源（DB + prompts 文件）。
pub trait ChapterCtx {
    /// 注入包（来自 `crate::budget::build_injection`）
    fn injection(&self, volume: &str, chapter: u32) -> Result<Value>;
    /// 读 prompts/{name}
    fn prompt_file(&self, name: &str) -> Option<String>;
    /// 查写法规则（kb_rules），scene 过滤；返回 [{code,name,rule_text,example}]
    fn kb_rules(&self, scene: &str, limit: usize) -> Vec<Value>;
    /// 查卡文本（用于注入，含 myth 中性化）
    fn material_content(&self, code: &str) -> Option<String>;
    /// 库根（用于接线层 wire 读取 data/*.json 与 novel.db）。
    /// 默认 None：不接线的实现（测试 mock）不受影响。
    fn lib_root(&self) -> Option<std::path::PathBuf> {
        None
    }
}

/// 构建知识注入块（照抄 build_knowledge_injection 的骨架上，返回 (block, counts)）。
pub fn build_knowledge_injection<C: ChapterCtx>(
    ctx: &C,
    chapter: u32,
    volume: &str,
    scene: &str,
) -> Result<(String, Value)> {
    let inj = ctx.injection(volume, chapter)?;
    let mut parts: Vec<String> = Vec::new();
    let mut counts = Map::new();

    // 资料卡注入（剥离叙事体 + myth 中性化）
    if let Some(mats) = inj.get("materials").and_then(|v| v.as_array()) {
        let mut mat_lines = Vec::new();
        let mut n = 0;
        for m in mats {
            let name = m.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let code = m.get("code").and_then(|v| v.as_str()).unwrap_or("");
            let content = m.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let cleaned = neutralize_myth(&strip_narrative(content));
            mat_lines.push(format!("### {name}（{code}）\n{cleaned}"));
            n += 1;
        }
        counts.insert("资料卡n".into(), json!(n));
        if !mat_lines.is_empty() {
            parts.push(format!("【资料卡注入】\n{}", mat_lines.join("\n\n")));
        }
    }

    // 角色性格卡
    if let Some(chars) = inj.get("chars").and_then(|v| v.as_array()) {
        let mut lines = Vec::new();
        let mut canon_lines: Vec<String> = Vec::new();
        for c in chars {
            let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let code = c.get("code").and_then(|v| v.as_str()).unwrap_or("");
            let style = c.get("speech_style").and_then(|v| v.as_str()).unwrap_or("");
            let catch = c.get("catchphrase").and_then(|v| v.as_str()).unwrap_or("");
            // 空值兜底：绝不把空字符串直接送进 prompt（会放任 LLM 自由发挥）
            let style_disp = if style.trim().is_empty() { "未标定（以角色卡整体为准）" } else { style };
            let catch_disp = if catch.trim().is_empty() { "无" } else { catch };
            lines.push(format!("  【{code} {name}】语言风格：{style_disp}；口头禅：{catch_disp}"));
            if let Some(cl) = c.get("canon_lines").and_then(|v| v.as_array()) {
                for s in cl.iter().filter_map(|x| x.as_str()) {
                    if !s.trim().is_empty() && !canon_lines.iter().any(|x| x == s) {
                        canon_lines.push(s.to_string());
                    }
                }
            }
        }
        counts.insert("角色卡n".into(), json!(lines.len()));
        counts.insert("定版台词n".into(), json!(canon_lines.len()));
        if !lines.is_empty() {
            parts.push(format!("【角色性格卡】\n{}", lines.join("\n")));
        }
        // 定版原文保护块：强制 LLM 原样输出，禁止拆词 / 扩写 / 加解说
        if !canon_lines.is_empty() {
            let body: Vec<String> = canon_lines
                .iter()
                .map(|s| format!("{s}"))
                .collect();
            parts.push(format!(
                "【定版原文（不可修改）】\n{}\n{}\n{}\n\
                 > 上述被 [[CANON_TEXT_START]]/[[CANON_TEXT_END]] 包裹的文本为**不可修改定版原文**：\n\
                 > 只能原封不动输出，禁止拆分、扩写、追加解释、新增字段（如「两个字」「推演次数」等）。",
                "[[CANON_TEXT_START]]",
                body.join("\n"),
                "[[CANON_TEXT_END]]",
            ));
        }
    }

    // 伏笔
    if let Some(fs) = inj.get("foreshadow") {
        if let Some(due) = fs.get("due").and_then(|v| v.as_array()) {
            if !due.is_empty() {
                let lines: Vec<String> = due
                    .iter()
                    .map(|d| {
                        format!(
                            "  【{}】{}",
                            d.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                            d.get("content").and_then(|v| v.as_str()).unwrap_or("")
                        )
                    })
                    .collect();
                counts.insert("到期伏笔n".into(), json!(lines.len()));
                parts.push(format!("【到期伏笔（本章须兑现/延期）】\n{}", lines.join("\n")));
            }
        }
    }

    // 前文尾巴
    if let Some(prev) = inj.get("prev_tail").and_then(|v| v.as_str()) {
        if !prev.is_empty() {
            parts.push(format!("【上一章末尾（衔接用）】\n{prev}"));
        }
    }

    // 写法规则（kb_rules）
    let rules = ctx.kb_rules(scene, 9);
    let mut rule_lines = Vec::new();
    for r in &rules {
        let code = r.get("code").and_then(|v| v.as_str()).unwrap_or("");
        let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let rt = r.get("rule_text").and_then(|v| v.as_str()).unwrap_or("");
        rule_lines.push(format!("  【{code} {name}】{rt}"));
    }
    counts.insert("规则n".into(), json!(rule_lines.len()));
    parts.push(format!(
        "【⑧写法规则（{scene}场景·参考，不覆盖 §1 用户风格）】\n{}\n【怎么写】规则供参照；语感/句长/口语腔以 §1 用户手稿风格为准，规则不覆盖。",
        if rule_lines.is_empty() { "  （无匹配规则，按 §1 铁律写）".to_string() } else { rule_lines.join("\n") }
    ));

    Ok((parts.join("\n\n"), Value::Object(counts)))
}

/// 叙事与人物风格约束（追加在模板末尾；贴合原作者手写文风）。
/// 纯 prompt 层，不含任何代码门禁逻辑。
pub const STYLE_CONSTRAINTS: &str = "\
# 叙事与人物风格约束（贴合原著文风）
1. 主角内心活动不要全部写成事后完整复盘总结。模拟人脑真实思考：允许碎片化、跳跃式念头、一闪而过的自嘲脑洞，部分想法不必得出结论，想法可以中途搁置。避免通篇规整、逻辑顺滑的大段心理独白。
2. 配角不能只有功能性喜怒哀乐。给配角增加微小具体小怪癖、无意识小动作；允许配角说出不直接推动主线的细碎言行。
3. 不必追求本章所有线索全部闭环。可以抛出疑问、异象碎片谜团，不在本章解答，留给后续章节。
4. 叙事视角：绝大多数跟随主角，允许极短镜头短暂偏移到环境、配角细微动作，不要死死锁死主角第一感官。
5. 文笔不要过度打磨华丽顺滑；适度保留思考的“毛刺感”；少用成套网文比喻；心理优先靠动作细节表现，但保留少量直白脑内碎念。
6. 硬性：主角仅内心允许自嘲玩笑；客观叙事、受伤死亡悲剧场景禁止戏谑；念头吐槽完毕必须回归残酷现实，不消解苦难。

硬性保留旧规则：段落40-80汉字；[[CANON_TEXT_START]]定版文本原样输出；禁止系统面板游戏UI；允许碎片化念头、配角微小习惯、可留存未解谜团。

# 输出强制要求
1. 严格遵守全局铁则 + 本章细纲，禁止自行新增剧情、新增人物、修改既定伏笔时序。
2. 文风：整体黑暗压抑；陈寂只允许内心进行自嘲脑洞，客观叙事不能搞笑；可碎片化念头，不要通篇规整复盘心理。
3. **只输出纯粹小说正文**：禁止前置分析、禁止结尾总结、禁止输出溯源 ID、**禁止输出数据库 SQL 脚本 / 代码围栏 / 任何非正文的技术内容**。
4. 生成完成自行自查一遍：是否触犯本章专属禁令。";

/// 构建完整正文 prompt（照抄 build_prompt 的骨架；模板来自 prompts/l5_正文.md）。
pub fn build_prompt<C: ChapterCtx>(ctx: &C, chapter: u32, volume: &str) -> Result<String> {
    let inj = ctx.injection(volume, chapter)?;
    let ch = inj.get("chapter").cloned().unwrap_or_else(|| json!({}));
    let scene = ch.get("scene_type").and_then(|v| v.as_str()).unwrap_or("日常");
    let dim = scene_dims(scene);
    let (kb_block, _counts) = build_knowledge_injection(ctx, chapter, volume, scene)?;
    let base = ctx.prompt_file("l5_正文.md").unwrap_or_default();
    // 在模板末尾追加「叙事与人物风格约束」（原有模板内容一字不动，仅追加）。
    let base = format!("{base}\n\n{}", STYLE_CONSTRAINTS);

    let outline = ch.get("outline").and_then(|v| v.as_str()).unwrap_or("");
    let title = ch.get("title").and_then(|v| v.as_str()).unwrap_or("");
    let anchors = ch.get("anchors").cloned().unwrap_or_else(|| json!({}));
    let wire_block = build_wire_injection(ctx, &inj, volume, chapter, scene);

    Ok(format!(
        "── 第{chapter}章 {title} ──\n\
         【场景】{scene}  手法路由：{dim}\n\
         【字数】{MIN_WORDS}-{MAX_WORDS} 字；单段≤200；全章18-25段\n\
         【锚点】{}\n\
         【细纲】\n{outline}\n\n\
         {base}\n\n{kb_block}\n\n{wire_block}\n",
        anchors
    ))
}

/// 组装接线注入块（库根缺失时返回空串，不阻断）。
fn build_wire_injection<C: ChapterCtx>(
    ctx: &C,
    inj: &Value,
    volume: &str,
    chapter: u32,
    scene: &str,
) -> String {
    let Some(root) = ctx.lib_root() else { return String::new() };
    let codes: Vec<String> = inj
        .get("chars")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|c| c.get("code").and_then(|x| x.as_str()).map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let data = crate::wire::gather(&root, volume, chapter, scene, &codes);
    crate::wire::render_blocks(&data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn myth_neutralized() {
        let s = neutralize_myth("这里提到存在之基与高维观测者文明");
        assert!(s.contains("锚点已建立"));
        assert!(s.contains("未知来源"));
        assert!(!s.contains("存在之基"));
    }

    #[test]
    fn strip_narrative_removes_sections() {
        let md = "# 卡\n| 字段 | 值 |\n## 一、叙事\n这是叙事内容\n更多\n## 二、叙事2\nxxx\n## 字段区\n| a | b |";
        let out = strip_narrative(md);
        assert!(!out.contains("这是叙事内容"), "{out}");
        assert!(out.contains("| a | b |"));
    }

    #[test]
    fn deterministic_rejects_banned_words() {
        let text = format!("{}归墟{}", "字".repeat(2600), "。");
        let r = deterministic_check(&text, 1);
        assert_eq!(r.verdict, "reject");
        assert!(r.p0.iter().any(|x| x.dim == "A.世界观"));
    }

    #[test]
    fn deterministic_passes_clean_text() {
        // 造一段干净文本：2500-4000 字，多段，句号密度够
        let para = "他走进屋子，看着桌上的灯。那灯很旧。他没有说话。窗外风很大。";
        let mut text = String::new();
        while text.chars().filter(|c| !c.is_whitespace()).count() < 2600 {
            text.push_str(para);
            text.push_str("\n\n");
        }
        let r = deterministic_check(&text, 1);
        assert!(!r.p0.iter().any(|x| x.dim == "A.世界观"), "{:?}", r.p0);
        assert!(r.stats["words"].as_u64().unwrap() >= 2500);
    }

    #[test]
    fn deterministic_flags_ai_words_and_long_para() {
        let long = "甲".repeat(250);
        let text = format!("不禁{}{}", long, "。".repeat(5));
        let r = deterministic_check(&text, 1);
        assert!(r.p0.iter().any(|x| x.dim == "B.段落"), "长段应 P0: {:?}", r.p0);
        assert!(r.p1.iter().any(|x| x.dim == "B.风格"), "AI词应 P1: {:?}", r.p1);
    }

    #[test]
    fn scene_dims_default_is_fallback() {
        assert_eq!(scene_dims("未知场景"), "A(中)+B+C(1层)");
        assert_eq!(scene_dims("战斗"), "E+A(中)+C(1层)");
    }
}
