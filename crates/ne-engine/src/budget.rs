//! 上下文预算控制 —— `context_budget.py` 的 Rust 忠实移植。
//!
//! 事实源：`05_管线框架/engine/context_budget.py`（约 280 行）
//!
//! 分两类：
//!   * **纯函数**（照抄）：est_tokens / extract_chapter / infer_scene / extract_person / match_chars
//!   * **需外部数据**（走 `BudgetCtx` trait）：char_index / select_materials / get_char_cards /
//!     due_foreshadow / event_status / prev_text / read_outline
//!
//! 设计：DB 与文件访问通过 trait 注入，本模块零 rusqlite / 零盘 IO，便于纯函数单测。

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;

/// 场景关键词表（**照抄 SCENE_KEYWORDS，顺序即优先级**）。
pub const SCENE_KEYWORDS: [(&str, &[&str]); 7] = [
    ("战斗", &["战斗", "厮杀", "攻击", "斩", "刺", "兽虎", "凶兽", "清道队", "脉冲", "剥离", "高熵"]),
    ("修炼", &["修为", "境界", "功法", "修炼", "运转", "灵力", "熵流体悟", "逆流", "滞流", "锚点"]),
    ("社交", &["议会", "报告", "判定", "七柱", "联席", "审判", "证词", "表决", "申请", "提案"]),
    ("心理", &["恐惧", "焦虑", "被遗忘", "丧失", "透明", "消散", "忘完", "薄", "空壳"]),
    ("大场面", &["大撕裂", "九界域", "文明碰撞", "全面", "所有阵营"]),
    ("探索", &["发现", "锚定", "刻痕", "频率", "感知", "低语", "脉动"]),
    ("过渡", &["赶路", "离开", "回到", "走出", "进入", "一年之后"]),
];

/// 外部数据源（DB + 文件）。
pub trait BudgetCtx {
    /// 细纲全文（03_叙事规划/细纲/{volume}.md）
    fn outline_text(&self, volume: &str) -> Option<String>;
    /// 按 code 查材料行，返回 {code,name,summary,file_path,category} 或 None
    fn material(&self, code: &str) -> Option<Value>;
    /// 读资料库文件全文（相对 02_资料库 的路径），缺失返回 None
    fn read_lib_file(&self, rel_path: &str) -> Option<String>;
    /// 全库角色行 [{code,name}]（category='角色'）
    fn role_rows(&self) -> Vec<Value>;
    /// 角色卡的 personality/summary [{code,name,personality,summary}]
    fn char_card_rows(&self, codes: &[String]) -> Vec<Value>;
    /// 到期 / 可埋伏笔
    fn foreshadow_rows(&self, volume: &str, chapter: u32) -> (Vec<Value>, Vec<Value>);
    /// 事件状态行 [{id,name,status}]
    fn event_rows(&self, ids: &[String]) -> Vec<Value>;
    /// 上一章正文末尾 tail_chars 字
    fn prev_tail(&self, volume: &str, chapter: u32, tail_chars: usize) -> String;
}

// ───────────────────────────── 纯函数

/// token 估算（照抄：按字符数）。
pub fn est_tokens(text: &str) -> usize {
    text.chars().count()
}

/// 场景推断 —— **计分制**（照抄 infer_scene）：各场景按命中关键词个数打分，
/// 取最高；全 0 返回 "日常"；并列取 SCENE_KEYWORDS 声明序第一个。
pub fn infer_scene(outline_text: &str) -> String {
    let scores: Vec<(&str, usize)> = SCENE_KEYWORDS
        .iter()
        .map(|(s, kws)| (*s, kws.iter().filter(|k| outline_text.contains(**k)).count()))
        .collect();
    let max = scores.iter().map(|(_, n)| *n).max().unwrap_or(0);
    if max == 0 {
        return "日常".to_string();
    }
    scores
        .iter()
        .find(|(_, n)| *n == max)
        .map(|(s, _)| s.to_string())
        .unwrap_or_else(|| "日常".to_string())
}

/// 从角色卡 name 提取纯人名（照抄 extract_person）。
/// '主角-陈寂（CHAR-99-001）' → '陈寂'
pub fn extract_person(name: &str) -> String {
    // 去括号（中英文）
    let re_paren = regex::Regex::new(r"[（(][^）)]*[）)]").unwrap();
    let s = re_paren.replace_all(name, "").trim().to_string();
    // 按 - · / 取最后一段
    let mut s = s;
    for sep in ["-", "·", "/"] {
        if s.contains(sep) {
            s = s.rsplit(sep).next().unwrap_or("").to_string();
        }
    }
    // 末尾 2-4 个汉字
    let re_tail = regex::Regex::new(r"([\u{4e00}-\u{9fff}]{2,4})$").unwrap();
    if let Some(m) = re_tail.captures(&s) {
        m.get(1).unwrap().as_str().to_string()
    } else {
        s
    }
}

/// 细纲第 N 章段落。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChapterSeg {
    pub title: String,
    pub text: String,
    pub card_refs: Vec<String>,
    pub anchors: Value,
}

/// 从细纲提取第 N 章（照抄 extract_chapter，含锚点块解析）。
/// 从细纲提取第 N 章（对齐 `extract_chapter` 语义）。
///
/// ⚠ Rust `regex` crate **不支持 look-ahead**（`(?=...)`），无法照抄 Python 原正则：
///   `###\s*第{N}章\s*(.+?)(?=\n###\s*第|\n=== |\Z)` + DOTALL
/// 改用等价实现：定位本章起点 → 找下一个 `### 第N章` / `=== ` 起点 → 手工切片。
/// **title 仍是整段**（与 Python 一致，Python 的 group(1) 也是整段）。
pub fn extract_chapter(outline: &str, chapter_no: u32) -> ChapterSeg {
    let start_re = match regex::Regex::new(&format!(r"###\s*第{chapter_no}章")) {
        Ok(r) => r,
        Err(_) => return ChapterSeg::default(),
    };
    let Some(sm) = start_re.find(outline) else {
        return ChapterSeg::default();
    };
    let start = sm.start();
    let after_head = sm.end();

    // 找下一个边界：`### 第` 或 `\n=== `
    let rest = &outline[after_head..];
    let next_re = regex::Regex::new(r"\n###\s*第").unwrap();
    let sep_re = regex::Regex::new(r"\n=== ").unwrap();
    let n1 = next_re.find(rest).map(|m| m.start());
    let n2 = sep_re.find(rest).map(|m| m.start());
    let cut = match (n1, n2) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => rest.len(),
    };
    let end = after_head + cut;

    let whole = outline[start..end].trim().to_string();
    // title = 本章头之后到章末（对齐 Python group(1) 的整段语义）
    let title = outline[after_head..end].trim().to_string();

    // card_refs: [A-Z]{2,5}(-?\d+)+
    let re_ref = regex::Regex::new(r"\b([A-Z]{2,5}(?:-?\d+)+)\b").unwrap();
    let mut seen = BTreeSet::new();
    let mut refs = Vec::new();
    for c in re_ref.captures_iter(&whole) {
        let r = c.get(1).unwrap().as_str().to_string();
        if r.chars().all(|ch| ch.is_ascii_digit()) {
            continue;
        }
        if seen.insert(r.clone()) {
            refs.push(r);
        }
    }

    // 锚点块
    let mut anchors = serde_json::Map::new();
    let re_anchor = regex::Regex::new(r"锚点：\n((?:\s*-\s*.+\n?)+)").unwrap();
    if let Some(am) = re_anchor.captures(&whole) {
        let body = am.get(1).map(|x| x.as_str()).unwrap_or("");
        let re_line = regex::Regex::new(r"\s*-\s*([^：:]+)[：:]\s*(.+)").unwrap();
        for line in body.lines() {
            if let Some(lm) = re_line.captures(line.trim()) {
                let key = lm.get(1).unwrap().as_str().trim().to_string();
                let val = lm.get(2).unwrap().as_str().trim().to_string();
                if key == "引典" || key == "情绪曲线" {
                    let tags: Vec<String> = regex::Regex::new(r"[,，/]")
                        .unwrap()
                        .split(&val)
                        .map(|t| t.trim().trim_matches(|c| c == '[' || c == ']' || c == '【' || c == '】').to_string())
                        .filter(|t| !t.is_empty())
                        .collect();
                    anchors.insert(key, json!(tags));
                } else {
                    anchors.insert(key, json!(val));
                }
            }
        }
    }

    ChapterSeg {
        title,
        text: whole,
        card_refs: refs,
        anchors: Value::Object(anchors),
    }
}

/// 细纲文本里实际出现的角色（照抄 match_chars：alias 长度≥2 且子串命中）。
pub fn match_chars(text: &str, roles: &[Value]) -> Vec<Value> {
    let mut hit = Vec::new();
    let mut seen = BTreeSet::new();
    for r in roles {
        let alias = r.get("alias").and_then(|v| v.as_str()).unwrap_or("");
        let code = r.get("code").and_then(|v| v.as_str()).unwrap_or("");
        if alias.chars().count() >= 2 && text.contains(alias) && !seen.contains(code) {
            seen.insert(code.to_string());
            hit.push(r.clone());
        }
    }
    hit
}

/// 全库角色 → [{code,name,alias}]。
pub fn char_index<C: BudgetCtx>(ctx: &C) -> Vec<Value> {
    let mut out = Vec::new();
    for r in ctx.role_rows() {
        let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let alias = extract_person(name);
        if !alias.is_empty() {
            out.push(json!({
                "code": r.get("code").cloned().unwrap_or(json!("")),
                "name": name,
                "alias": alias,
            }));
        }
    }
    out
}

/// 选卡（照抄 select_materials）：core 全文 / 超预算转 summary。
pub fn select_materials<C: BudgetCtx>(
    ctx: &C,
    card_refs: &[String],
    budget_tokens: usize,
) -> (Vec<Value>, Vec<String>) {
    let mut results = Vec::new();
    let mut missing = Vec::new();
    let mut used = 0usize;
    for code in card_refs {
        let Some(row) = ctx.material(code) else {
            missing.push(code.clone());
            continue;
        };
        let name = row.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let summary = row.get("summary").and_then(|v| v.as_str()).unwrap_or("");
        let file_path = row.get("file_path").and_then(|v| v.as_str()).unwrap_or("");
        let content = ctx
            .read_lib_file(file_path)
            .unwrap_or_else(|| summary.to_string());
        let tok = est_tokens(&content);
        if used + tok > budget_tokens {
            let s = summary.to_string();
            let stok = est_tokens(&s);
            results.push(json!({
                "code": code, "name": name, "content": s, "is_full": false, "tokens": stok,
            }));
            used += stok;
        } else {
            results.push(json!({
                "code": code, "name": name, "content": content, "is_full": true, "tokens": tok,
            }));
            used += tok;
        }
    }
    (results, missing)
}

/// 从角色卡正文提取「语言风格」兜底（仅当结构化字段为空时用）。
///
/// 本库 `personality` 列当前全空，「语言风格」永远为空，导致 LLM 无约束。
/// 兜底策略：从卡片正文的「性格」段摘一句风格类描述，原文照录、不改写、不臆造；
/// 摘不到则返回空串（下游会填「未标定」而非空白）。
fn extract_style_from_content(content: &str) -> String {
    const KEYS: [&str; 8] = [
        "不善言辞", "沉默寡言", "惜字如金", "健谈", "滔滔不绝",
        "口语", "说话", "口吻",
    ];
    // 优先匹配「性格：…」「性格」段内首个含风格词的短句
    let mut seg_start = None;
    for (i, line) in content.lines().enumerate() {
        if line.contains("**性格**") || line.trim_start().starts_with("性格") || line.contains("### 性格") {
            seg_start = Some(i);
            break;
        }
    }
    let scan = match seg_start {
        Some(i) => content.lines().skip(i).take(6).collect::<Vec<_>>(),
        None => content.lines().collect::<Vec<_>>(),
    };
    for line in &scan {
        // 剥掉 markdown 标记后找风格词
        let clean = line.replace("**", "").replace("*", "").replace("- ", "");
        if let Some(k) = KEYS.iter().find(|k| clean.contains(**k)) {
            // 取该行含关键词的短句（按 ｜ ； 。 切）
            for piece in clean.split(|c| ['。', '；', '|', '｜'].contains(&c)) {
                if piece.contains(*k) {
                    let s = piece.trim().trim_start_matches(|c: char| c == '：' || c == ':' || c == ' ');
                    if s.chars().count() >= 4 && s.chars().count() <= 40 {
                        return s.to_string();
                    }
                }
            }
            let s: String = clean.trim().chars().take(40).collect();
            if s.chars().count() >= 4 {
                return s;
            }
        }
    }
    String::new()
}

/// 从角色卡正文提取「定版台词」行（`### 标志性台词` 段内 `- 「…」`）。
/// 返回去引号的纯台词，供 prompt 层加不可修改标记。
fn extract_canon_lines(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_seg = false;
    for line in content.lines() {
        let t = line.trim();
        if t.contains("标志性台词") {
            in_seg = true;
            continue;
        }
        if in_seg && t.starts_with('#') {
            break; // 下一个标题段，结束
        }
        if in_seg {
            if let Some(s) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
                // 显式禁令行（如「（无定版台词。正文严禁出现"…"）」）不是台词，跳过。
                const NEG: [&str; 10] = [
                    "严禁", "禁止", "不许", "禁出现", "绝不", "永不", "不能出现", "不得出现",
                    "一概不", "不要",
                ];
                if NEG.iter().any(|n| t.contains(n)) {
                    continue;
                }
                // 形如「锚点已建立。」（面板定版台词）
                for (open, close) in [('「', '」'), ('“', '”'), ('"', '"')] {
                    if let Some(a) = s.find(open) {
                        // 闭合引号必须从开引号**之后**开始找，否则对称引号（"…"）
                        // 会匹配到开引号自身（b=0 → a+1..a 反向区间 panic）。
                        let from = a + open.len_utf8();
                        if let Some(b) = s[from..].find(close) {
                            let inner: String = s[from..from + b].trim().to_string();
                            if !inner.is_empty() && inner.chars().count() <= 40 {
                                out.push(inner);
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

/// 角色性格卡（照抄 get_char_cards 的字段投影）。
pub fn get_char_cards<C: BudgetCtx>(ctx: &C, codes: &[String]) -> Vec<Value> {
    if codes.is_empty() {
        return vec![];
    }
    let mut out = Vec::new();
    for r in ctx.char_card_rows(codes) {
        let p: Value = r
            .get("personality")
            .and_then(|v| v.as_str())
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_else(|| json!({}));
        let content = r.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let g = |k: &str| p.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        // 语言风格：结构化字段优先；为空则从卡片正文兜底提取（不臆造）
        let mut style = g("语言风格");
        if style.trim().is_empty() {
            style = extract_style_from_content(content);
        }
        // 定版台词：从卡片正文提取，供 prompt 加不可修改标记
        let canon_lines = extract_canon_lines(content);
        out.push(json!({
            "code": r.get("code").cloned().unwrap_or(json!("")),
            "name": r.get("name").cloned().unwrap_or(json!("")),
            "speech_style": style,
            "talk_amount": g("话量"),
            "inner_mode": g("内心模式"),
            "react_mode": g("反应模式"),
            "signature_action": g("特征动作"),
            "catchphrase": g("口头禅"),
            "canon_lines": canon_lines,
            "realm": g("境界"),
            "location": g("位置"),
            "status": p.get("状态").and_then(|v| v.as_str()).unwrap_or("存活"),
            "summary": r.get("summary").and_then(|v| v.as_str()).unwrap_or(""),
        }));
    }
    out
}

/// 主入口：构建注入包（照抄 build_injection 的返回结构，键名一致）。
pub fn build_injection<C: BudgetCtx>(ctx: &C, volume: &str, chapter: u32) -> Result<Value> {
    let outline = ctx.outline_text(volume).unwrap_or_default();
    let ch = extract_chapter(&outline, chapter);
    let scene = infer_scene(&ch.text);

    let roles = char_index(ctx);
    let matched = match_chars(&ch.text, &roles);
    let matched_codes: Vec<String> = matched
        .iter()
        .filter_map(|m| m.get("code").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .collect();
    let chars = get_char_cards(ctx, &matched_codes);

    let (materials, missing) = select_materials(ctx, &ch.card_refs, 20000);
    let (due, can_plant) = ctx.foreshadow_rows(volume, chapter);
    let foreshadow = json!({ "due": due, "can_plant": can_plant });
    let events = ctx.event_rows(&ch.card_refs);
    let prev = ctx.prev_tail(volume, chapter, 500);

    Ok(json!({
        "chapter": {
            "no": chapter,
            "title": ch.title,
            "outline": ch.text,
            "card_refs": ch.card_refs,
            "scene_type": scene,
            "matched_chars": matched.iter().filter_map(|m| m.get("alias").and_then(|v| v.as_str())).collect::<Vec<_>>(),
            "anchors": ch.anchors,
        },
        "materials": materials,
        "missing_refs": missing,
        "chars": chars,
        "foreshadow": foreshadow,
        "events": events,
        "prev_tail": prev,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn infer_scene_scoring_not_priority() {
        // 含 2 个"战斗"词 + 1 个"修炼"词 → 战斗
        let t = "厮杀 战斗 修为";
        assert_eq!(infer_scene(t), "战斗");
        // 全 0 → 日常
        assert_eq!(infer_scene("张三在走路"), "日常");
    }

    #[test]
    fn extract_person_strips_paren_and_prefix() {
        assert_eq!(extract_person("主角-陈寂（CHAR-99-001）"), "陈寂");
        assert_eq!(extract_person("陈寂"), "陈寂");
    }

    #[test]
    fn extract_canon_lines_handles_ascii_quotes_without_panic() {
        // 回归：ASCII 自配对引号曾致 a+1..a 反向区间 panic（CHAR-99-001 第69行触发）
        let md = "### 标志性台词\n- （无定版台词。正文严禁出现\"锚点已建立\"等体系术语。）\n- 「滑倒了，摔了一跤。」（对祖父口径）\n";
        let out = extract_canon_lines(md);
        assert!(!out.iter().any(|s| s.contains("锚点已建立")), "禁令行不得当台词：{out:?}");
        assert!(out.iter().any(|s| s.contains("滑倒了")), "正常台词应提取：{out:?}");
    }

    #[test]
    fn extract_chapter_refs_and_anchors() {
        // ⚠ 对齐 Python 实测行为：title 取的是「第N章 之后到章末」的**整段**，
        // 不是单行标题（Python 正则 `(.+?)` 配 DOTALL 捕获整段；实测已确认）。
        // 这里断言的是**与 Python 一致**，而非「更合理」的语义。
        let outline = "### 第1章 开局\n正文 CHAR-99-001 与 EVT-99-005。\n锚点：\n- 引典：[希望, 哲理]\n- 文明状态：太玄=开局\n\n### 第2章 续\nx";
        let c = extract_chapter(outline, 1);
        assert!(c.title.starts_with("开局"), "title 应以标题词开头，实际 {:?}", c.title);
        assert!(c.text.contains("CHAR-99-001"));
        assert!(c.card_refs.contains(&"CHAR-99-001".to_string()));
        assert!(c.card_refs.contains(&"EVT-99-005".to_string()));
        assert_eq!(c.anchors["引典"], json!(["希望", "哲理"]));
        assert_eq!(c.anchors["文明状态"], json!("太玄=开局"));
        // 第2章切分正确（不串到第1章）
        let c2 = extract_chapter(outline, 2);
        assert!(c2.title.starts_with("续"));
        assert!(!c2.text.contains("EVT-99-005"));
    }

    #[test]
    fn match_chars_uses_alias_substring() {
        let roles = vec![
            json!({"code":"CHAR-1","name":"主角-陈寂（CHAR-1）","alias":"陈寂"}),
            json!({"code":"CHAR-2","name":"配角-白鸦","alias":"白鸦"}),
        ];
        let hit = match_chars("陈寂走进屋子", &roles);
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0]["alias"], "陈寂");
    }

    #[test]
    fn select_materials_adds_missing() {
        struct C;
        impl BudgetCtx for C {
            fn outline_text(&self, _: &str) -> Option<String> { None }
            fn material(&self, code: &str) -> Option<Value> {
                if code == "CHAR-1" {
                    Some(json!({"code":"CHAR-1","name":"陈寂","summary":"摘要","file_path":"","category":"角色"}))
                } else { None }
            }
            fn read_lib_file(&self, _: &str) -> Option<String> { None }
            fn role_rows(&self) -> Vec<Value> { vec![] }
            fn char_card_rows(&self, _: &[String]) -> Vec<Value> { vec![] }
            fn foreshadow_rows(&self, _: &str, _: u32) -> (Vec<Value>, Vec<Value>) { (vec![], vec![]) }
            fn event_rows(&self, _: &[String]) -> Vec<Value> { vec![] }
            fn prev_tail(&self, _: &str, _: u32, _: usize) -> String { String::new() }
        }
        let (res, missing) = select_materials(&C, &["CHAR-1".into(), "LAW-99-001".into()], 20000);
        assert_eq!(res.len(), 1);
        assert_eq!(missing, vec!["LAW-99-001".to_string()]);
    }
}
