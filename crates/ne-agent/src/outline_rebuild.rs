//! 细纲「拍」差异化重建（`ne-agent outline rebuild`）。
//!
//! # 背景
//! 部分卷（卷01/卷03）的 L3 细纲用「5 章一拍」模板机械展开：同一拍的 5 章
//! 「目标/在场/阻碍/选择/决定/代价」**完全复制**，只有「钩子/变化」不同，且变化是套话。
//! 这导致正文重复。（根因证据：`05_管线框架/reports/vol01_tail_sim.md` 载「卷01 原仅 53 章实写」）
//!
//! 本模块逐「拍」（连续 5 章）调 LLM，展开为 5 个递进子拍：
//! 起(基础) → 阻碍具象 → 选择挣扎 → 代价体感 → 钩子。
//!
//! # 约束
//! - **只改细纲**，不碰正文/canon 卡。
//! - **不新增设定**：只用基础概要里出现过的人/地/物/卡号。
//! - **禁工程词**：推演/算账/漏斗/面板/因果流/影子层/分支。
//! - 保留拍首章「本章约束」行（强制回注）与原锚点引典。
//! - 校验：5 章四字段须互异，否则重试（≤3 次）；仍失败 → 保留原样。
//! - 原文件自动备份 `.bak_rebuild`；`--dry-run` 不落盘。
//!
//! 复用 `health::outline_chapter_sigs` / `dump` 解析口径，避免分叉。

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::outline_parse::{grab, split_chapters, Chunk};
use ne_engine::llm::{self, LlmParams, Message};

pub const BEAT_SIZE: usize = 5;

// 设计层词探测集与禁令行标记统一取自 SSOT（之前本地副本用「分支」而 health 用「分支演算」，已收敛）。
use ne_core::lexicon::{DESIGN_LEAK_WORDS as DESIGN_WORDS, NEG_MARKS};
/// 机械套话（「变化」字段）。
const BOILER: &[&str] = &["阻力压进具体场景", "走到临界", "过节点", "本拍目标完成"];

/// 拍首章的基础概要八字段。
fn beat_fields(beat: &[Chunk]) -> Vec<(String, String)> {
    let body = &beat[0].2;
    ["目标", "在场", "阻碍", "选择", "钩子", "决定", "代价", "变化"]
        .iter()
        .map(|k| {
            let bold = !matches!(*k, "决定" | "代价" | "变化");
            ((*k).to_string(), grab(body, k, bold))
        })
        .collect()
}

/// 拍首章的「本章约束」行（`**【…】**` 且含约束/断言/禁/优先）。
fn beat_constraints(beat: &[Chunk]) -> Vec<String> {
    beat[0]
        .2
        .lines()
        .map(|l| l.trim())
        .filter(|t| t.starts_with("**【") && t.ends_with("**"))
        .filter(|t| ["约束", "断言", "禁", "优先"].iter().any(|k| t.contains(k)))
        .map(|s| s.to_string())
        .collect()
}

/// 拍内出现过的「引典：」值（去重）。
fn beat_anchors(beat: &[Chunk]) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for (_, _, body) in beat {
        for line in body.lines() {
            let t = line.trim();
            if let Some(rest) = t.strip_prefix("- 引典：").or_else(|| t.strip_prefix("- 引典:")) {
                let v = rest.trim().to_string();
                if seen.insert(v.clone()) {
                    out.push(v);
                }
            }
        }
    }
    out
}

// ───────────────────────────── prompt

#[allow(clippy::too_many_arguments)]
fn build_prompt(beat: &[Chunk], idx: usize, total: usize, beat_name: &str, prev_tail: &str, next_head: &str) -> String {
    let f = beat_fields(beat);
    let get = |k: &str| f.iter().find(|(a, _)| a == k).map(|(_, b)| b.clone()).unwrap_or_default();
    let first = beat[0].0;
    let last = beat[beat.len() - 1].0;
    let a1 = first + 1;
    let a2 = first + 2;
    let a3 = first + 3;
    let n = BEAT_SIZE;

    let mut ctx = String::new();
    if !prev_tail.is_empty() {
        ctx.push_str(&format!("【上一拍结尾】{prev_tail}\n"));
    }
    if !next_head.is_empty() {
        ctx.push_str(&format!("【下一拍开头】{next_head}\n"));
    }
    let cons = beat_constraints(beat);
    let cons_block = if cons.is_empty() {
        String::new()
    } else {
        format!("【本章约束·必须原样保留在首章】\n{}\n", cons.join("\n"))
    };
    let anch = beat_anchors(beat);
    let anch_block = if anch.is_empty() {
        String::new()
    } else {
        format!("【本拍锚点·引典（照抄）】\n{}\n", anch.iter().map(|a| format!("- {a}")).collect::<Vec<_>>().join("\n"))
    };
    let anchor_example = anch.first().cloned().unwrap_or_else(|| "（无）".to_string());

    format!(
        "你是网文细纲编辑。下面是一个「故事拍」（连续 {n} 章）。该拍当前 {n} 章概要**完全复制**，这是缺陷，需要你把它们改成 {n} 个**递进的不同子阶段**。\n\n\
【拍名】{beat_name}\n\
【章节范围】第{first}~{last}章（第 {idx}/{total} 拍）\n\
【基础概要·本拍发生了什么】\n\
- 目标：{g0}\n- 在场：{g1}\n- 阻碍：{g2}\n- 选择：{g3}\n- 决定：{g4}\n- 代价：{g5}\n- 变化：{g6}\n- 钩子：{g7}\n\n\
{ctx}{cons_block}{anch_block}\n\
【任务】\n\
把本拍拆成 {n} 个递进的子阶段，各章分工：\n\
- 第{first}章：起——呈现本拍基础情境（目标初见、处境铺开）\n\
- 第{a1}章：阻碍具象——把抽象阻碍压成**当下具体可感的事件/场景**\n\
- 第{a2}章：选择挣扎——主角走到抉择临界，把权衡写成**具体的进退两难**\n\
- 第{a3}章：代价体感——让「决定」的代价在**身体/生活/关系**上落地\n\
- 第{last}章：钩子——用未解疑点收束整拍\n\n\
【硬要求（违反即废稿）】\n\
1. {n} 章的「目标」必须**各不相同且递进**，禁止复制。\n\
2. 「阻碍」须具体化、场景化，{n} 章各有侧重；禁止照抄同一句。\n\
3. 「选择」「决定」必须是该章**具体、不可逆的动作**（如「连夜下山去镇上」），不是抽象概括。\n\
4. 「代价」必须是**当章已发生的具体损失**（钱/伤/名/时间/关系），禁止「消耗次数」「负担未解」这类复用套话。\n\
5. 「变化」写该章结束时处境/关系/认知的**实际位移**；**严禁**出现「阻力压进具体场景」「走到临界」「过节点」「本拍目标完成」这类套话。\n\
6. 「钩子」每章不同，是本章末的悬念。\n\
7. **严禁工程词**：推演/算账/漏斗/面板/因果流/影子层/分支。金手指只能写「后脑那股说不清的异样体感」。\n\
8. **不新增设定**：只用上面出现过的人/地/物/卡号。保留合法卡号标注（如【EVNT-99-005·地点】）。\n\
9. 字数：每章概要总长 120~220 字。\n\n\
【输出格式】严格照抄，{n} 章，不要任何解释、不要代码围栏：\n\
### 第{first}章 标题（本章的短标题，2~5 字，不要用「·阻碍具象」式机械后缀）\n\
**概要**\n- **目标**：…\n- **在场**：…\n- **阻碍**：…\n- **选择**：…\n- **钩子**：…\n\
**决定→代价→变化**\n- 决定：…\n- 代价：…\n- 变化：…\n\
锚点：\n- 引典：{anchor_example}\n---\n（后续 {} 章同格式，章号依次递增）",
        n - 1,
        g0 = get("目标"), g1 = get("在场"), g2 = get("阻碍"), g3 = get("选择"),
        g4 = get("决定"), g5 = get("代价"), g6 = get("变化"), g7 = get("钩子"),
    )
}

// ───────────────────────────── 解析 + 校验

fn parse_reply(reply: &str, first_no: u32) -> Vec<Chunk> {
    use crate::outline_parse::{find_chapter_start, parse_head, split_at_chapter_heads};
    let txt = reply
        .lines()
        .filter(|l| !l.trim_start().starts_with("```"))
        .collect::<Vec<_>>()
        .join("\n");
    let Some(pos) = find_chapter_start(&txt) else { return Vec::new() };
    let mut out: Vec<Chunk> = Vec::new();
    for part in split_at_chapter_heads(&txt[pos..]) {
        if let Some((no, title)) = parse_head(&part) {
            out.push((no, title, part));
        }
    }
    out.retain(|(no, _, _)| *no >= first_no && *no < first_no + BEAT_SIZE as u32);
    out
}

fn validate(chs: &[Chunk], first_no: u32) -> (bool, Vec<String>) {
    let mut err = Vec::new();
    if chs.len() != BEAT_SIZE {
        err.push(format!("章数 {} != {BEAT_SIZE}", chs.len()));
        return (false, err);
    }
    for (i, (no, _, body)) in chs.iter().enumerate() {
        if *no != first_no + i as u32 {
            err.push(format!("章号不连续：期望 {} 实得 {no}", first_no + i as u32));
        }
        for k in ["目标", "在场", "阻碍", "选择", "钩子"] {
            if grab(body, k, true).is_empty() {
                err.push(format!("第{no}章缺 {k}"));
            }
        }
        for k in ["决定", "代价", "变化"] {
            if grab(body, k, false).is_empty() {
                err.push(format!("第{no}章缺 {k}"));
            }
        }
    }
    for key in ["目标", "阻碍", "选择"] {
        let vals: Vec<String> = chs.iter().map(|(_, _, b)| grab(b, key, true)).filter(|v| !v.is_empty()).collect();
        let mut uniq = vals.clone();
        uniq.sort();
        uniq.dedup();
        if uniq.len() != vals.len() {
            err.push(format!("{key} 有重复"));
        }
    }
    for (no, _, body) in chs {
        let chg = grab(body, "变化", false);
        if BOILER.iter().any(|b| chg.contains(b)) {
            err.push(format!("第{no}章「变化」是套话：{}", &chg.chars().take(24).collect::<String>()));
        }
        for line in body.lines() {
            if NEG_MARKS.iter().any(|n| line.contains(n)) {
                continue;
            }
            if let Some(w) = DESIGN_WORDS.iter().find(|w| line.contains(**w)) {
                err.push(format!("第{no}章出现工程词「{w}」"));
                break;
            }
        }
    }
    (err.is_empty(), err)
}

// ───────────────────────────── 主流程

pub struct RebuildOpts {
    pub volume: String,
    pub only_beat: Option<usize>,
    pub dry_run: bool,
}

/// 重建一卷。返回 (重建拍数, 保留拍数)。
pub fn rebuild(lib_root: &Path, llm: &LlmParams, opts: &RebuildOpts) -> Result<(usize, usize)> {
    let path: PathBuf = lib_root.join("03_叙事规划/细纲").join(format!("{}.md", opts.volume));
    let txt = std::fs::read_to_string(&path)
        .with_context(|| format!("读取细纲失败：{}", path.display()))?;
    let (header, chunks) = split_chapters(&txt);
    if chunks.is_empty() {
        anyhow::bail!("{} 解析不到 `### 第N章` 章节", path.display());
    }
    let beats: Vec<&[Chunk]> = chunks.chunks(BEAT_SIZE).collect();
    let total = beats.len();

    let mut blocks: Vec<String> = Vec::new();
    let (mut n_ok, mut n_keep) = (0usize, 0usize);
    for (bi, beat) in beats.iter().enumerate() {
        let i = bi + 1;
        if let Some(only) = opts.only_beat {
            if i != only {
                blocks.push(beat.iter().map(|c| c.2.as_str()).collect::<String>());
                continue;
            }
        }
        let beat_name = beat[0].1.split('·').next().unwrap_or("").trim().to_string();
        if beat.len() != BEAT_SIZE {
            println!("  [{i}/{total}] {beat_name} — 非完整拍（{} 章），跳过", beat.len());
            blocks.push(beat.iter().map(|c| c.2.as_str()).collect::<String>());
            n_keep += 1;
            continue;
        }
        let prev_tail = if bi > 0 {
            let pf = beat_fields(beats[bi - 1]);
            let chg = pf.iter().find(|(k, _)| k == "变化").map(|(_, v)| v.clone()).unwrap_or_default();
            format!("「{}」拍以变化「{chg}」收尾", beats[bi - 1][0].1)
        } else {
            String::new()
        };
        let next_head = if bi + 1 < total {
            let nf = beat_fields(beats[bi + 1]);
            let goal = nf.iter().find(|(k, _)| k == "目标").map(|(_, v)| v.clone()).unwrap_or_default();
            format!("「{}」拍目标「{goal}」", beats[bi + 1][0].1)
        } else {
            String::new()
        };

        let mut prompt = build_prompt(beat, i, total, &beat_name, &prev_tail, &next_head);
        let mut block: Option<String> = None;
        let mut errs: Vec<String> = Vec::new();
        for _ in 0..3 {
            let reply = match llm::call(llm, &[Message::user(prompt.clone())]) {
                Ok(o) => o.content,
                Err(e) => {
                    errs = vec![format!("LLM: {e}")];
                    continue;
                }
            };
            let chs = parse_reply(&reply, beat[0].0);
            let (ok, e) = validate(&chs, beat[0].0);
            if ok {
                block = Some(chs.iter().map(|c| format!("{}\n", c.2.trim_end())).collect::<Vec<_>>().join("\n"));
                break;
            }
            errs = e;
            prompt.push_str(&format!(
                "\n\n【上次未通过，原因】{}\n请修正后重新输出完整 {BEAT_SIZE} 章。",
                errs.iter().take(6).cloned().collect::<Vec<_>>().join("；")
            ));
        }
        match block {
            Some(mut b) => {
                // 强制回注「本章约束」行到首章标题后（LLM 常漏抄）。
                let cons = beat_constraints(beat);
                if !cons.is_empty() {
                    let mut lines: Vec<String> = b.split('\n').map(|s| s.to_string()).collect();
                    if let Some(pos) = lines.iter().position(|l| l.starts_with("### ")) {
                        for (k, c) in cons.iter().enumerate() {
                            lines.insert(pos + 1 + k, c.clone());
                        }
                    }
                    b = lines.join("\n");
                }
                println!("  [{i}/{total}] {beat_name} ✓");
                blocks.push(b);
                n_ok += 1;
            }
            None => {
                println!("  [{i}/{total}] {beat_name} ✗ 保留原样：{:?}", errs.iter().take(3).collect::<Vec<_>>());
                blocks.push(beat.iter().map(|c| c.2.as_str()).collect::<String>());
                n_keep += 1;
            }
        }
    }

    let mut out = header;
    for b in &blocks {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(b);
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }

    if opts.dry_run {
        println!("\n===== DRY-RUN（前 4000 字）=====\n{}", out.chars().take(4000).collect::<String>());
    } else {
        let bak = path.with_extension("md.bak_rebuild");
        if !bak.exists() {
            std::fs::write(&bak, &txt).with_context(|| format!("备份失败：{}", bak.display()))?;
        }
        std::fs::write(&path, &out).with_context(|| format!("写回失败：{}", path.display()))?;
        println!("\n已写回 {}（备份 {}）", path.display(), bak.file_name().unwrap_or_default().to_string_lossy());
    }
    println!("重建 {n_ok} 拍 / 保留 {n_keep} 拍 / 共 {total} 拍");
    Ok((n_ok, n_keep))
}

// ───────────────────────────── 设计层词清洗（outline sanitize）

pub struct SanitizeOpts {
    pub volume: String,
    /// 只清洗第 N 章（1 起）；不传 = 全卷。
    pub only_chapter: Option<u32>,
    pub dry_run: bool,
}

/// 某行是否含设计层词（跳过显式禁令行）。
fn design_hit_line(l: &str) -> bool {
    if NEG_MARKS.iter().any(|n| l.contains(n)) {
        return false;
    }
    DESIGN_WORDS.iter().any(|w| l.contains(*w))
}

fn chapter_has_design(body: &str) -> bool {
    body.lines().any(design_hit_line)
}

/// 抽全文中的卡号（`[A-Z]{2,6}-\d+`），用于校验清洗后未丢引用。
fn card_codes(text: &str) -> std::collections::BTreeSet<String> {
    let re = regex::Regex::new(r"\b[A-Z]{2,6}-\d+(?:-\d+)*\b").unwrap();
    re.find_iter(text).map(|m| m.as_str().to_string()).collect()
}

fn build_sanitize_prompt(body: &str) -> String {
    format!(
        "你是网文细纲编辑。下面一章细纲用了「设计层工程词」描述本章行为——这些词（推演/面板/漏斗/因果流/算账/算路径/算因果/影子层/分支演算）是给设计者看的，不属于故事世界，若随细纲流入正文会变成「系统行」。\n\n\
【任务】把这一章改写为**叙事层**表达：\n\
1. 结构保持（字段与顺序不变）：`### 第N章 标题`——**标题里的设计层词也必须改写为叙事表达**（如「算筹婆·市井推演」→「算筹婆·市井卜算」；「推演面板·体系外变量定位」→「那股东西·体系外变量定位」）；`**概要**`、`- **目标**：`/`- **在场**：`/`- **阻碍**：`/`- **选择**：`/`- **钩子**：`、`**决定→代价→变化**`/`- 决定：`/`- 代价：`/`- 变化：`、`锚点：`/`- 引典：` 全部照抄，字段名与顺序不变。\n\
2. 所有卡号（如 ITEM-99-005、EVNT-99-002、CHAR-50-012）**原样保留**，不得删改；但 `- 引典：` 行里卡号**之外**的描述性文字（如「推演分析」）里的设计层词也要改写。\n\
3. 把设计层词改写为角色/故事视角的说法：金手指写「他脑子里那股说不清的东西」「后脑那阵说不清的东西」，不命名、不解释、不类比；「推演结果」写「他心里过的那个结论」；保留剧情实质（谁做什么、结果如何）。\n\
3b. **叙事句内不得再嵌金手指卡号**（如 `ITEM-99-005`）：卡号只保留在 `- 引典：` 行与【】标注中；正文描述用「那股说不清的东西」指代即可。\n\
4. 具体能力等级（如 U1/U5）可保留为中性说法（如「那股东西又醒了一分」），不写成面板字段。\n\
5. 字数与原章相当（±20%），不得增删剧情节点。\n\
6. **只输出这一章文本**，不要解释、不要代码围栏。\n\n\
【原文】\n{body}"
    )
}

/// 清洗一卷细纲。返回 (清洗章数, 保留章数)。
pub fn sanitize(lib_root: &Path, llm: &LlmParams, opts: &SanitizeOpts) -> Result<(usize, usize)> {
    let path: PathBuf = lib_root.join("03_叙事规划/细纲").join(format!("{}.md", opts.volume));
    let txt = std::fs::read_to_string(&path)
        .with_context(|| format!("读取细纲失败：{}", path.display()))?;
    let (header, chunks) = split_chapters(&txt);
    if chunks.is_empty() {
        anyhow::bail!("{} 解析不到 `### 第N章` 章节", path.display());
    }

    let mut blocks: Vec<String> = Vec::new();
    let (mut n_ok, mut n_keep) = (0usize, 0usize);
    let mut chapters_cleaned: Vec<u32> = Vec::new();
    for c in &chunks {
        let (no, _, body) = c;
        if let Some(only) = opts.only_chapter {
            if *no != only {
                blocks.push(body.clone());
                continue;
            }
        }
        if !chapter_has_design(body) {
            blocks.push(body.clone());
            n_keep += 1;
            continue;
        }
        // 只校验「引典」行的卡号不丢（叙事句内的卡号本就要被移除，不作为保留要求）。
        let ref_lines: String = body.lines().filter(|l| l.trim().starts_with("- 引典")).collect::<Vec<_>>().join("
");
        let want_cards = card_codes(&ref_lines);
        let mut prompt = build_sanitize_prompt(body);
        let mut new_body: Option<String> = None;
        let mut errs: Vec<String> = Vec::new();
        for _ in 0..3 {
            let reply = match llm::call(llm, &[Message::user(prompt.clone())]) {
                Ok(o) => o.content,
                Err(e) => { errs = vec![format!("LLM: {e}")]; continue; }
            };
            let txt2 = reply.lines().filter(|l| !l.trim_start().starts_with("```")).collect::<Vec<_>>().join("\n");
            let chs = parse_reply(&txt2, *no);
            if chs.len() != 1 {
                errs = vec![format!("回包章数 {} != 1", chs.len())];
                prompt.push_str(&format!("\n\n【上次失败】应只输出第{no}章一章。"));
                continue;
            }
            let nb = chs[0].2.trim_end().to_string();
            let narrative_card = nb.lines().any(|l| {
                let t = l.trim();
                !t.starts_with("- 引典") && t.contains("ITEM-99-005")
            });
            let latin_word = nb.lines().any(|l| {
                // 先剔卡号（含未带号的卡类前缀，如孤立 CHAR）；残余裸英文词才算污染。
                let card_re = regex::Regex::new(r"\b[A-Z]{2,6}(-\d+)?\b").unwrap();
                let stripped = card_re.replace_all(l, "");
                regex::Regex::new(r"[A-Za-z]{4,}").unwrap().is_match(&stripped)
            });
            if chapter_has_design(&nb) {
                errs = vec!["仍含设计层词".into()];
            } else if narrative_card {
                errs = vec!["叙事句内残留金手指卡号 ITEM-99-005（应只在引典行）".into()];
            } else if latin_word {
                errs = vec!["混入裸英文单词（如 readings）".into()];
            } else {
                let got_cards = card_codes(&nb);
                let lost: Vec<_> = want_cards.difference(&got_cards).cloned().collect();
                if !lost.is_empty() {
                    errs = vec![format!("丢卡号 {lost:?}")];
                } else {
                    new_body = Some(nb);
                    break;
                }
            }
            prompt.push_str(&format!("\n\n【上次未通过】{}\n请修正后重新输出完整一章。", errs.join("；")));
        }
        match new_body {
            Some(nb) => {
                println!("  [第{no}章] ok");
                blocks.push(format!("{nb}\n"));
                chapters_cleaned.push(*no);
                n_ok += 1;
            }
            None => {
                println!("  [第{no}章] keep：{:?}", errs.iter().take(2).collect::<Vec<_>>());
                blocks.push(body.clone());
                n_keep += 1;
            }
        }
    }

    let mut out = header;
    for b in &blocks {
        if !out.ends_with('\n') { out.push('\n'); }
        out.push_str(b);
    }
    if !out.ends_with('\n') { out.push('\n'); }

    if opts.dry_run {
        println!("\n===== DRY-RUN：清洗后的章节内容 =====");
        if n_ok == 0 {
            println!("（本卷/本范围无需清洗或全部保留）");
        }
        for no in &chapters_cleaned {
            if let Some((_, _, b)) = chunks.iter().find(|(n, _, _)| n == no) {
                // 打印清洗后版本（blocks 与 chunks 同序）
                let idx = chunks.iter().position(|(n, _, _)| n == no).unwrap();
                println!("{}", blocks[idx].trim_end());
                println!("---");
                let _ = b;
            }
        }
    } else {
        let bak = path.with_extension("md.bak_sanitize");
        if !bak.exists() {
            std::fs::write(&bak, &txt).with_context(|| format!("备份失败：{}", bak.display()))?;
        }
        std::fs::write(&path, &out).with_context(|| format!("写回失败：{}", path.display()))?;
        println!("\n已写回 {}（备份 {}）", path.display(), bak.file_name().unwrap_or_default().to_string_lossy());
    }
    println!("清洗 {n_ok} 章 / 保留 {n_keep} 章 / 共 {} 章", chunks.len());
    Ok((n_ok, n_keep))
}


#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# 卷头
> 说明
### 第1章 涧底醒
**概要**
- **目标**：确认自己还活着
- **在场**：陈寂
- **阻碍**：涧水齐腰
- **选择**：上岸
- **钩子**：那声音还会再响吗
**决定→代价→变化**
- 决定：只说滑倒了
- 代价：隐瞒的孤独
- 变化：祖父开始疑心
锚点：
- 引典：EVNT-99-005 §四
### 第2章 涧底醒·阻碍具象
- **目标**：确认自己还活着
- **在场**：陈寂
- **阻碍**：涧水齐腰
- **选择**：上岸
";

    #[test]
    fn split_keeps_header_and_chunks() {
        let (h, chs) = split_chapters(SAMPLE);
        assert!(h.contains("卷头"));
        assert_eq!(chs.len(), 2);
        assert_eq!(chs[0].0, 1);
        assert_eq!(chs[0].1, "涧底醒");
        assert_eq!(chs[1].0, 2);
        assert!(chs[1].1.contains("阻碍具象"));
    }

    #[test]
    fn grab_and_beat_fields() {
        let (_, chs) = split_chapters(SAMPLE);
        let f = beat_fields(&chs[..1]);
        let g = |k: &str| f.iter().find(|(a, _)| a == k).map(|(_, b)| b.clone()).unwrap();
        assert_eq!(g("目标"), "确认自己还活着");
        assert_eq!(g("决定"), "只说滑倒了");
        assert_eq!(g("钩子"), "那声音还会再响吗");
        let a = beat_anchors(&chs[..1]);
        assert_eq!(a, vec!["EVNT-99-005 §四".to_string()]);
    }

    #[test]
    fn validate_rejects_duplicate_and_boiler() {
        let reply = "\
### 第1章 A
- **目标**：甲
- **在场**：陈寂
- **阻碍**：甲阻
- **选择**：甲选
- **钩子**：甲钩
- 决定：甲决
- 代价：甲价
- 变化：阻力压进具体场景：可感的阻碍在当下时刻落地，处境收紧
### 第2章 B
- **目标**：甲
- **在场**：陈寂
- **阻碍**：甲阻
- **选择**：甲选
- **钩子**：乙钩
- 决定：乙决
- 代价：乙价
- 变化：乙变
### 第3章 C
- **目标**：丙
- **在场**：陈寂
- **阻碍**：丙阻
- **选择**：丙选
- **钩子**：丙钩
- 决定：丙决
- 代价：丙价
- 变化：丙变
### 第4章 D
- **目标**：丁
- **在场**：陈寂
- **阻碍**：丁阻
- **选择**：丁选
- **钩子**：丁钩
- 决定：丁决
- 代价：丁价
- 变化：丁变
### 第5章 E
- **目标**：戊
- **在场**：陈寂
- **阻碍**：戊阻
- **选择**：戊选
- **钩子**：戊钩
- 决定：戊决
- 代价：戊价
- 变化：戊变
";
        let chs = parse_reply(reply, 1);
        assert_eq!(chs.len(), 5);
        let (ok, errs) = validate(&chs, 1);
        assert!(!ok, "应检出问题");
        assert!(errs.iter().any(|e| e.contains("目标") && e.contains("重复")), "{errs:?}");
        assert!(errs.iter().any(|e| e.contains("套话")), "{errs:?}");
    }

    #[test]
    fn sanitize_helpers_detect_and_preserve() {
        // design_hit_line：含设计层词判 true；禁令行豁免
        assert!(design_hit_line("- **阻碍**：推演面板读数漂离"));
        assert!(!design_hit_line("严禁出现「面板」"));
        assert!(!design_hit_line("- **在场**：陈寂、林晚秋"));
        // chapter_has_design
        assert!(chapter_has_design("- **目标**：借推演摸到关系"));
        assert!(!chapter_has_design("- **目标**：弄清灵气为何物"));
    }

    #[test]
    fn card_codes_multisegment_and_ref_only() {
        // 多段数字卡号须整体抽取（历史 bug：[A-Z]{2,6}-\d+ 会截成 ITEM-99）
        let c = card_codes("- 引典：ITEM-99-005；FORE-99-020 §5.1；CHAR-50-012");
        assert!(c.contains("ITEM-99-005"), "{c:?}");
        assert!(c.contains("FORE-99-020"), "{c:?}");
        assert!(c.contains("CHAR-50-012"), "{c:?}");
        assert!(!c.contains("ITEM-99"), "不得截断：{c:?}");
    }

    #[test]
    fn validate_rejects_design_words() {
        let reply = "\
### 第1章 A
- **目标**：他借面板推演未来
- **在场**：陈寂
- **阻碍**：甲阻
- **选择**：甲选
- **钩子**：甲钩
- 决定：甲决
- 代价：甲价
- 变化：甲变
### 第2章 B
- **目标**：乙
- **在场**：陈寂
- **阻碍**：乙阻
- **选择**：乙选
- **钩子**：乙钩
- 决定：乙决
- 代价：乙价
- 变化：乙变
### 第3章 C
- **目标**：丙
- **在场**：陈寂
- **阻碍**：丙阻
- **选择**：丙选
- **钩子**：丙钩
- 决定：丙决
- 代价：丙价
- 变化：丙变
### 第4章 D
- **目标**：丁
- **在场**：陈寂
- **阻碍**：丁阻
- **选择**：丁选
- **钩子**：丁钩
- 决定：丁决
- 代价：丁价
- 变化：丁变
### 第5章 E
- **目标**：戊
- **在场**：陈寂
- **阻碍**：戊阻
- **选择**：戊选
- **钩子**：戊钩
- 决定：戊决
- 代价：戊价
- 变化：戊变
";
        let chs = parse_reply(reply, 1);
        let (ok, errs) = validate(&chs, 1);
        assert!(!ok);
        assert!(errs.iter().any(|e| e.contains("工程词")), "{errs:?}");
    }
}
