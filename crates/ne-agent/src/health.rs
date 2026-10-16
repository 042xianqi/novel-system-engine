//! 系统健康自检（health）——两层：
//!
//! 1. **加载时模块自检** `check_modules`：库根结构、novel.db 业务表、配置一致性、
//!    细纲、prompt 模板、角色卡目录、模型 provider。只读，不写库、不调模型。
//! 2. **生成前注入就绪硬闸门** `preflight`：针对「要写某章」检查该章所有**待注入**项
//!    （细纲切分 / 资料卡 / 角色卡 / 伏笔 / 事件 / 前文 / 记忆四块 / 章节钩子）是否就绪。
//!
//! # 约束（与 `audit_material` 同口径）
//! - **只读**：`novel.db` 以 `SQLITE_OPEN_READ_ONLY` 打开，自身不执行任何写语句。
//! - **零外呼**：全程不调大模型。
//! - **不改业务**：只复用 `NovelCtx` / `budget::build_injection` / `config::load`。
//!
//! 判定三态：`Fatal`（阻塞生成）/ `Warn`（可继续但需人工复核）/ `Info`（正常记录）。
//! `Report.ready` = 无 Fatal。
use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use serde_json::Value;
use std::path::Path;

use ne_engine::chapter::ChapterCtx;

/// 单项检查级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// 阻塞：就绪性缺失，不应继续（生成前闸门以此判 ready=false）。
    Fatal,
    /// 告警：可继续，但建议人工复核（如库规模未达标、快照未到触发点）。
    Warn,
    /// 正常。
    Info,
}

/// 一项检查。
#[derive(Debug, Clone, Serialize)]
pub struct Check {
    /// 分组（结构/数据库/配置/细纲/模板/注入/记忆…）。
    pub group: String,
    /// 项名。
    pub name: String,
    /// 级别。
    pub level: Level,
    /// 是否通过。
    pub ok: bool,
    /// 证据（计数/路径/首条摘录）。
    pub detail: String,
    /// 修复建议（非 ok 时给出）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl Check {
    fn info(group: &str, name: impl Into<String>, detail: String) -> Self {
        Self { group: group.into(), name: name.into(), level: Level::Info, ok: true, detail, hint: None }
    }
    fn warn(group: &str, name: impl Into<String>, detail: String, hint: impl Into<String>) -> Self {
        Self { group: group.into(), name: name.into(), level: Level::Warn, ok: false, detail, hint: Some(hint.into()) }
    }
    fn fatal(group: &str, name: impl Into<String>, detail: String, hint: impl Into<String>) -> Self {
        Self { group: group.into(), name: name.into(), level: Level::Fatal, ok: false, detail, hint: Some(hint.into()) }
    }
}

/// 报告（两入口共用）。
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    /// `modules`（加载时）| `preflight`（生成前）。
    pub kind: String,
    pub lib_root: String,
    pub volume: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chapter: Option<u32>,
    pub total: usize,
    pub fatal: usize,
    pub warn: usize,
    /// 无 Fatal 即就绪（生成前闸门据此决定是否放行）。
    pub ready: bool,
    pub checks: Vec<Check>,
}

impl Report {
    fn build(kind: &str, lib_root: &Path, volume: &str, chapter: Option<u32>, checks: Vec<Check>) -> Self {
        let fatal = checks.iter().filter(|c| c.level == Level::Fatal).count();
        let warn = checks.iter().filter(|c| c.level == Level::Warn).count();
        Self {
            kind: kind.into(),
            lib_root: lib_root.display().to_string(),
            volume: volume.into(),
            chapter,
            total: checks.len(),
            fatal,
            warn,
            ready: fatal == 0,
            checks,
        }
    }
}

/// 关键业务表（缺一即生成链路易断）。
const KEY_TABLES: &[&str] = &[
    "materials", "events", "foreshadow", "char_state", "char_emotion",
    "short_term_memory", "mid_term_snapshot", "long_term_memory",
    "chapters", "chapter_meta", "scoring_dimensions", "kb_knowledge",
];

/// 只读打开 novel.db。
fn open_ro(db: &Path) -> Result<Connection> {
    Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("只读打开 novel.db 失败：{}", db.display()))
}

fn table_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [name],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
    .unwrap_or(false)
}
fn count(conn: &Connection, sql: &str, params: &[&dyn rusqlite::ToSql]) -> i64 {
    conn.query_row(sql, params, |r| r.get::<_, i64>(0)).unwrap_or(0)
}

// ── 细纲结构性检查：章概要雷同（模板机械展开 → 正文重复的根源）
//
// 解析口径统一走 `outline_parse`（唯一真值源），不再本地重复实现。

use crate::outline_parse::{chapter_sigs, dup_runs};

/// 兼容旧调用名（内部委托 `outline_parse::chapter_sigs`）。
fn outline_chapter_sigs(txt: &str) -> Vec<(u32, String, String)> {
    chapter_sigs(txt)
}


// ───────────────────────────── ① 加载时模块自检

/// 加载时模块自检（只读、零外呼）。
pub fn check_modules(lib_root: &Path, volume: &str) -> Result<Report> {
    let mut checks = Vec::new();

    // ── 结构：库根关键目录/文件
    let dirs: [(&str, &str); 5] = [
        ("novel.db", "file"),
        ("02_资料库", "dir"),
        ("03_叙事规划", "dir"),
        ("04_正文", "dir"),
        ("05_管线框架", "dir"),
    ];
    for (rel, kind) in dirs {
        let p = lib_root.join(rel);
        let exists = if kind == "dir" { p.is_dir() } else { p.is_file() };
        if exists {
            checks.push(Check::info("结构", rel, format!("存在：{}", p.display())));
        } else {
            checks.push(Check::fatal(
                "结构",
                rel,
                format!("缺失：{}", p.display()),
                format!("库根不像完整资料库，请确认 --lib 指向正确（当前 {})", lib_root.display()),
            ));
        }
    }

    // ── 数据库：可开 + 关键表 + 关键数据
    match open_ro(&lib_root.join("novel.db")) {
        Ok(conn) => {
            let missing: Vec<&str> = KEY_TABLES.iter().copied().filter(|t| !table_exists(&conn, t)).collect();
            if missing.is_empty() {
                checks.push(Check::info("数据库", "关键业务表", format!("{} 张全在", KEY_TABLES.len())));
            } else {
                checks.push(Check::fatal(
                    "数据库",
                    "关键业务表",
                    format!("缺 {} 张：{missing:?}", missing.len()),
                    "novel.db 结构不完整，生成链路会取不到数据",
                ));
            }
            // materials 规模
            let mats = count(&conn, "SELECT COUNT(*) FROM materials", &[]);
            if mats > 0 {
                checks.push(Check::info("数据库", "materials 资料卡", format!("{mats} 条")));
            } else if table_exists(&conn, "materials") {
                checks.push(Check::fatal("数据库", "materials 资料卡", "0 条".into(), "资料库为空，注入包里将无资料卡"));
            }
            // 章节元数据
            let metas = count(&conn, "SELECT COUNT(*) FROM chapter_meta WHERE volume=?1", &[&volume]);
            checks.push(Check::info("数据库", "chapter_meta（本卷）", format!("{metas} 行")));
            // 零外呼证据：llm 调用日志表应可读（哪怕为空）
            if table_exists(&conn, "llm_call_log") {
                let calls = count(&conn, "SELECT COUNT(*) FROM llm_call_log", &[]);
                checks.push(Check::info("数据库", "llm_call_log", format!("历史 {calls} 条（可读）")));
            }
        }
        Err(e) => checks.push(Check::fatal("数据库", "novel.db 打开", format!("{e}"), "检查文件权限/是否被锁")),
    }

    // ── 配置：dynamics_config 可解析 + 字数 ramp 与段数区间自洽
    match ne_engine::config::load(lib_root) {
        Ok(cfg) => {
            checks.push(Check::info(
                "配置",
                "dynamics_config.json",
                format!("已加载（word_min 基准={} / word_max={}）", cfg.word_min(), cfg.word_max()),
            ));
            // ⚠ 一致性：卷下限字数与段数区间必须**双向**可同时满足：
            //   ① word_min ≤ MAX_PARA × 每段上限（段数上界能装下字数下限）
            //   ② word_min ≥ MIN_PARA × 每段下限（段数下界能撑起字数下限）
            let wmin_v = cfg.word_min_for_volume(volume) as usize;
            let para_max = ne_engine::chapter::MAX_PARA;
            let para_min = ne_engine::chapter::MIN_PARA;
            let span_max = 80usize; // 每段常规上限（模板 §1.2，硬上限 100）
            let span_min = 40usize; // 每段常规下限（模板 §1.2）
            let upper = para_max * span_max;
            let lower = para_min * span_min;
            if wmin_v > upper {
                checks.push(Check::fatal(
                    "配置",
                    "字数×段数自洽",
                    format!("{volume} 下限 {wmin_v} 字 > 段数上限 {para_max} × 每段 {span_max} 字 = {upper}"),
                    "段数上限与字数下限互斥：任何章节都无法同时达标，请调 MAX_PARA 或 word_min",
                ));
            } else if wmin_v < lower && wmin_v > 0 {
                checks.push(Check::warn(
                    "配置",
                    "字数×段数自洽",
                    format!("{volume} 下限 {wmin_v} 字 < 段数下限 {para_min} × 每段 {span_min} 字 = {lower}"),
                    "段数下限会撑出超过字数下限的篇幅（可能触发字数超上限）；建议同步调 MIN_PARA 或接受浮动",
                ));
            } else {
                checks.push(Check::info(
                    "配置",
                    "字数×段数自洽",
                    format!("{volume} 下限 {wmin_v} 字 ∈ [段数下界 {lower}, 段数上界 {upper}]"),
                ));
            }
            // ramp 一致性提示：micro 卷（细纲 180 章）应有多档下限
            let has_ramp = cfg.gates.chapter_word_min_ramp.as_ref().map(|r| !r.is_empty()).unwrap_or(false);
            checks.push(Check::info(
                "配置",
                "字数下限分卷 ramp",
                if has_ramp { "已配置分卷覆盖".into() } else { "无 ramp（全卷用基准下限）".into() },
            ));
        }
        Err(e) => checks.push(Check::fatal("配置", "dynamics_config.json", format!("{e}"), "JSON 解析或读取失败")),
    }

    // ── 细纲：文件存在 + 可切章
    let outline_path = lib_root.join("03_叙事规划/细纲").join(format!("{volume}.md"));
    match std::fs::read_to_string(&outline_path) {
        Ok(txt) => {
            let seg = ne_engine::budget::extract_chapter(&txt, 1);
            if seg.text.trim().is_empty() {
                checks.push(Check::warn("细纲", format!("{volume}.md 第1章"), "切出为空".into(), "检查细纲标题格式是否为 `### 第N章 …`"));
            } else {
                checks.push(Check::info("细纲", format!("{volume}.md"), format!("{} 字，第1章可切出 {} 字", txt.chars().count(), seg.text.chars().count())));
            }
            // ── 结构性：卷内「章概要雷同」统计（模板机械展开的信号）
            let sigs = outline_chapter_sigs(&txt);
            let dups = dup_runs(&sigs, 3);
            let involved: usize = dups.iter().map(|(_, _, n)| *n).sum();
            if dups.is_empty() {
                checks.push(Check::info("细纲", "章概要雷同", format!("{} 章，无连续雷同组", sigs.len())));
            } else {
                checks.push(Check::warn(
                    "细纲",
                    "章概要雷同",
                    format!(
                        "{} 章中 {involved} 章卷入 {} 个雷同组（例：{:?}）",
                        sigs.len(), dups.len(),
                        dups.iter().take(3).map(|(s, e, n)| format!("第{s}~{e}章×{n}")).collect::<Vec<_>>()
                    ),
                    "细纲用模板机械展开、章概要复制粘贴，正文会重复；须按章差异化后再生成",
                ));
            }
        }
        Err(e) => checks.push(Check::fatal("细纲", format!("{volume}.md"), format!("{e}"), "细纲文件缺失，无法切章")),
    }

    // ── prompt 模板：l5_正文.md 存在且含关键约束段
    let prompt_path = lib_root.join("05_管线框架/prompts/l5_正文.md");
    match std::fs::read_to_string(&prompt_path) {
        Ok(t) => {
            let need = ["幽默", "钩子"];
            let lack: Vec<&&str> = need.iter().filter(|k| !t.contains(**k)).collect();
            if lack.is_empty() {
                checks.push(Check::info("模板", "l5_正文.md", format!("{} 字，含关键约束段", t.chars().count())));
            } else {
                checks.push(Check::warn("模板", "l5_正文.md", format!("缺关键词：{lack:?}"), "模板被删改，风格约束可能失效"));
            }
        }
        Err(e) => checks.push(Check::fatal("模板", "l5_正文.md", format!("{e}"), "正文模板缺失，prompt 组装将为空")),
    }

    // ── 角色卡目录
    let role_dir = lib_root.join("02_资料库/角色");
    let n_roles = std::fs::read_dir(&role_dir).map(|rd| rd.filter_map(|e| e.ok()).count()).unwrap_or(0);
    if n_roles > 0 {
        checks.push(Check::info("资料库", "角色卡目录", format!("{n_roles} 个文件")));
    } else {
        checks.push(Check::warn("资料库", "角色卡目录", "0 个文件".into(), "角色卡缺失会影响人物一致性注入"));
    }

    Ok(Report::build("modules", lib_root, volume, None, checks))
}

// ───────────────────────────── ② 生成前注入就绪硬闸门

/// 生成前闸门：检查第 `chapter` 章**所有待注入项**是否就绪。
/// 有 Fatal 时 `report.ready=false`，调用方应中止生成。
pub fn preflight(lib_root: &Path, volume: &str, chapter: u32) -> Result<Report> {
    let mut checks = Vec::new();
    let ctx = crate::novel_ctx::NovelCtx::open(lib_root)?;

    // ── 细纲切分
    let raw_outline = std::fs::read_to_string(
        lib_root.join("03_叙事规划/细纲").join(format!("{volume}.md")),
    )
    .unwrap_or_default();
    let seg = ne_engine::budget::extract_chapter(&raw_outline, chapter);
    if seg.text.trim().is_empty() {
        checks.push(Check::fatal("注入", "细纲切分", format!("第{chapter}章切出为空"), "细纲缺该章，或标题格式不符"));
    } else {
        checks.push(Check::info("注入", "细纲切分", format!("{} 字", seg.text.chars().count())));
        // 本章钩子字段（G02 依赖）
        let has_hook = seg.text.contains("钩子");
        if has_hook {
            checks.push(Check::info("注入", "章节钩子字段", "细纲含『钩子』".into()));
        } else {
            checks.push(Check::warn("注入", "章节钩子字段", "细纲无『钩子』行".into(), "章末钩子无源，G02 易挂"));
        }
        // ── 细纲结构性缺陷：本章是否卷入「概要雷同」组（模板机械展开 → 正文重复的根源）
        let sigs = outline_chapter_sigs(&raw_outline);
        let dups = dup_runs(&sigs, 3);
        if let Some((s, e, n)) = dups.iter().find(|(s, e, _)| chapter >= *s && chapter <= *e) {
            checks.push(Check::fatal(
                "细纲",
                "本章概要雷同",
                format!("本章属雷同组 第{s}~{e}章（{n} 章共享同一「目标/在场/阻碍/选择」）"),
                "细纲该组概要为模板复制，正文会与前章重复；须先差异化该组再生成此章",
            ));
        }
    }

    // ── 注入包（materials / chars / foreshadow / events / prev_tail）
    match ctx.injection(volume, chapter) {
        Ok(inj) => {
            let mats = inj.get("materials").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
            let chars = inj.get("chars").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
            let missing: Vec<String> = inj
                .get("missing_refs")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let refs = seg.card_refs.len();

            // 细纲引用了代号 → 必须能选入资料卡
            if refs > 0 && mats == 0 {
                checks.push(Check::fatal("注入", "资料卡选入", format!("细纲引用 {refs} 个代号，选入 0 张"), "materials 表或 select_materials 通路断了"));
            } else {
                checks.push(Check::info("注入", "资料卡选入", format!("引用 {refs} → 选入 {mats} 张")));
            }
            if !missing.is_empty() {
                checks.push(Check::warn(
                    "注入",
                    "缺失引用卡",
                    format!("{} 个：{:?}", missing.len(), missing.iter().take(5).collect::<Vec<_>>()),
                    "细纲引用了库中无卡的代号（待建卡），注入会缺项",
                ));
            }
            // 本卷需要出场角色时应能匹配到角色卡
            if chars == 0 {
                checks.push(Check::warn("注入", "角色卡匹配", "0 张".into(), "本章无角色卡注入，人物一致性可能漂移"));
            } else {
                checks.push(Check::info("注入", "角色卡匹配", format!("{chars} 张")));
            }
            let due = inj.get("foreshadow").and_then(|f| f.get("due")).and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
            checks.push(Check::info("注入", "伏笔（到期）", format!("{due} 条")));
            let events = inj.get("events").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
            checks.push(Check::info("注入", "事件", format!("{events} 条")));
            let prev = inj.get("prev_tail").and_then(|v| v.as_str()).unwrap_or("").chars().count();
            if prev == 0 {
                checks.push(Check::warn("注入", "前文末尾", "0 字".into(), "第1章或无前文属正常；否则检查 04_正文"));
            } else {
                checks.push(Check::info("注入", "前文末尾", format!("{prev} 字")));
            }
        }
        Err(e) => checks.push(Check::fatal("注入", "注入包组装", format!("{e}"), "build_injection 失败，无法组装 prompt")),
    }

    // ── 记忆四块（short_term / snapshot / char_state / anchors）
    match crate::memory_system::open(&lib_root.join("novel.db")) {
        Ok(conn) => {
            let short = count(&conn, "SELECT COUNT(*) FROM short_term_memory WHERE volume=?1 AND chapter<?2", &[&volume, &chapter]);
            let snap = count(&conn, "SELECT COUNT(*) FROM mid_term_snapshot WHERE upto_chapter<?1", &[&chapter]);
            let cstate = count(&conn, "SELECT COUNT(*) FROM char_state WHERE volume=?1 AND chapter<=?2", &[&volume, &chapter]);
            let anchors = count(&conn, "SELECT COUNT(*) FROM long_term_memory WHERE category='锚点元'", &[]);
            checks.push(Check::info("记忆", "短期记忆", format!("{short} 条")));
            checks.push(Check::info("记忆", "中期快照", format!("{snap} 个（未到触发点可为 0，属正常）")));
            checks.push(Check::info("记忆", "实体状态", format!("{cstate} 行")));
            checks.push(Check::info("记忆", "跨卷锚点", format!("{anchors} 条")));
        }
        Err(e) => checks.push(Check::warn("记忆", "记忆库打开", format!("{e}"), "记忆注入将降级为空")),
    }

    Ok(Report::build("preflight", lib_root, volume, Some(chapter), checks))
}

// ───────────────────────────── ③ 启动自检（boot）：设计层词污染全量扫描

// 设计层词探测集与「显式禁令行」标记统一取自 SSOT，避免本地副本漂移。
use ne_core::lexicon::{DESIGN_LEAK_WORDS as BOOT_DESIGN_WORDS, NEG_MARKS as BOOT_NEG_MARKS};

/// 会被注入 prompt 的库内文件（相对库根）。原 Python `tools/boot_check.py` 已并入本模块。
const BOOT_INJECTED_FILES: &[&str] = &[
    "05_管线框架/prompts/l5_正文.md",
    "05_管线框架/prompts/l5_质检.md",
    "05_管线框架/prompts/l5_修复.md",
    "02_资料库/_规范/全局铁则/_全局世界观铁则.md",
    "02_资料库/_规范/全局铁则/_全局角色档案库.md",
    "02_资料库/_规范/全局铁则/_跨卷锚点追踪表.md",
    "03_叙事规划/00_整本概括.md",
    "03_叙事规划/00_故事脊梁.md",
    "03_叙事规划/00_叙事总纲.md",
    "03_叙事规划/00_宏大对抗总规划.md",
    "03_叙事规划/护栏·规划约束.md",
    "03_叙事规划/细纲/跨卷线锚点表.md",
];

/// 会被注入 prompt 的 DB 表（表名, 文本列）——这些表内容**会进 prompt**。
const BOOT_INJECTED_DB: &[(&str, &[&str])] = &[
    ("long_term_memory", &["content"]),
    ("kb_knowledge", &["canon", "basis"]),
    ("event_template", &["trigger", "trigger_basis"]),
    ("materials", &["content"]),
    ("char_state", &["notes"]),
    ("short_term_memory", &["summary"]),
    ("mid_term_snapshot", &["payload"]),
    ("foreshadow", &["note"]),
];

/// 仅归档、**不参与注入**的 DB 表（报告信息，不阻塞）。
/// 依据：`prev_tail` 读磁盘 `04_正文/卷NN/第N章.md`，不读 `chapter_meta.content`。
const BOOT_ARCHIVE_DB: &[(&str, &[&str])] = &[("chapter_meta", &["content"])];

/// 主角相关卡号标记（命中含这些串的文件名 → 主角卡）。
const BOOT_PROTAGONIST_MARK: &[&str] = &["CHAR-99-001", "CHAR-90-038", "EVNT-99-002", "EVNT-99-005"];

fn boot_find_hits(text: &str) -> Vec<(&'static str, usize)> {
    let kept: String = text
        .lines()
        .filter(|l| !BOOT_NEG_MARKS.iter().any(|n| l.contains(n)))
        .collect::<Vec<_>>()
        .join("\n");
    let mut remaining = kept;
    let mut out = Vec::new();
    let mut ws: Vec<&'static str> = BOOT_DESIGN_WORDS.to_vec();
    ws.sort_by_key(|w| std::cmp::Reverse(w.chars().count()));
    for w in ws {
        let n = remaining.matches(w).count();
        if n > 0 {
            out.push((w, n));
            remaining = remaining.replace(w, &"\u{0}".repeat(w.len()));
        }
    }
    out
}

/// 该文本是否含设计层词（跳过禁令行）；供 materials/正文精筛复用。
pub fn boot_has_design_words(text: &str) -> bool {
    !boot_find_hits(text).is_empty()
}


/// 扫描**本卷细纲章文本**（`### 第N章` 之后，**排除头部引用卡清单**）中的设计层词。
///
/// 依据：`build_injection` 把细纲**章文本**原样注入 L5 prompt（`outline` 字段），
/// 即使注入期 sanitize 会中性化，细纲本身用设计层视角写（如「面板弹出一行」）
/// 仍会让模型产出「系统行」。故细纲源数据必须叙事化——此处作**出厂门禁**判据。
/// 跳过显式禁令行（合规提及）。
fn outline_body_design_hits(txt: &str) -> Vec<(&'static str, usize)> {
    let start = txt.find("### 第").unwrap_or(0);
    boot_find_hits(&txt[start..])
}

/// 从细纲抽出本卷**章正文**引用的卡号（`[A-Z]{2,6}-\d+`），用于收窄 materials/卡扫描面。
///
/// ⚠ 只扫 `### 第N章` 之后的正文，**排除文件头部**「引用卡清单」——
/// 那只是卷级声明，非逐章注入面。`build_injection` 实际用 `extract_chapter`
/// 从**章文本**抽的 `card_refs`，口径须与此一致（否则误报：终卷正典卡等
/// 仅出现在头部清单的卡会被误判为「本卷引用」）。
fn outline_ref_codes(lib_root: &Path, volume: &str) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    for rel in [format!("03_叙事规划/细纲/{volume}.md"), "03_叙事规划/细纲/跨卷线锚点表.md".to_string()] {
        let Ok(txt) = std::fs::read_to_string(lib_root.join(&rel)) else { continue };
        // 起始：第一个 `### 第` 标题；找不到则全文（跨卷表无章标题，全文扫）
        let start = txt.find("### 第").unwrap_or(0);
        let body = &txt[start..];
        let mut cur = String::new();
        for ch in body.chars() {
            if ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '-' {
                cur.push(ch);
            } else {
                if cur.len() >= 4 && cur.contains('-') {
                    set.insert(cur.clone());
                }
                cur.clear();
            }
        }
        if cur.len() >= 4 && cur.contains('-') {
            set.insert(cur);
        }
    }
    set
}

/// 启动自检：设计层词污染全量扫描（只读、零外呼）。
///
/// 覆盖四类**会被注入**的来源 + 一类回流入口：
/// 1. 注入用文件（模板/铁则/总纲/跨卷表）
/// 2. 注入用 DB 表（`materials` 收窄到本卷引用卡；`chapter_meta` 降为归档信息）
/// 3. 资料卡目录（本卷引用 / 主角自身 → 阻塞）
/// 4. 已生成正文（`prev_tail` 回流入口）
/// 5. 细纲章概要雷同（结构性缺陷）
pub fn boot(lib_root: &Path, volume: &str) -> Result<Report> {
    let mut checks = Vec::new();

    // 1. 注入用文件
    for rel in BOOT_INJECTED_FILES {
        let p = lib_root.join(rel);
        if !p.is_file() {
            continue;
        }
        if let Ok(txt) = std::fs::read_to_string(&p) {
            let hits = boot_find_hits(&txt);
            if !hits.is_empty() {
                let det = hits.iter().map(|(w, n)| format!("{w}×{n}")).collect::<Vec<_>>().join("、");
                checks.push(Check::warn(
                    "启动",
                    *rel,
                    det,
                    "注入用文件含设计层词；注入期 sanitize_injection 会中性化，此处列清单供复核",
                ));
            }
        }
    }

    // 2. 注入用 DB 表
    if let Ok(conn) = open_ro(&lib_root.join("novel.db")) {
        let ref_codes = outline_ref_codes(lib_root, volume);
        for (table, cols) in BOOT_INJECTED_DB {
            if !table_exists(&conn, table) {
                continue;
            }
            for col in *cols {
                let sql = format!(
                    "SELECT COUNT(*) FROM {table} WHERE {col} LIKE '%推演%' OR {col} LIKE '%面板%' \
                     OR {col} LIKE '%漏斗%' OR {col} LIKE '%因果流%'"
                );
                let n = count(&conn, &sql, &[]);
                if n == 0 {
                    continue;
                }
                let name = format!("{table}.{col}");
                // materials 二次精筛：只对本卷引用卡判阻塞
                if *table == "materials" {
                    let mut ref_hit: Vec<String> = Vec::new();
                    for code in &ref_codes {
                        let ok: bool = conn
                            .query_row(
                                "SELECT content FROM materials WHERE code=?1 OR new_code=?1 OR old_code=?1 LIMIT 1",
                                [code],
                                |r| Ok(r.get::<_, Option<String>>(0)?.unwrap_or_default()),
                            )
                            .map(|c| boot_has_design_words(c.trim_matches('\u{0}')))
                            .unwrap_or(false);
                        if ok {
                            ref_hit.push(code.clone());
                        }
                    }
                    if ref_hit.is_empty() {
                        checks.push(Check::info(
                            "启动",
                            name,
                            format!("{n} 行含设计层词（均非本卷引用卡，不进注入面）"),
                        ));
                    } else {
                        checks.push(Check::warn(
                            "启动",
                            name,
                            format!("本卷引用卡命中：{ref_hit:?}"),
                            "本卷注入面内的卡含设计层词；注入期 sanitize_injection 会中性化，此处列清单供复核",
                        ));
                    }
                    continue;
                }
                checks.push(Check::warn(
                    "启动",
                    name,
                    format!("{n} 行含设计层词"),
                    "注入表含设计层词；注入期 sanitize_injection 会中性化，此处列清单供复核",
                ));
            }
        }
        // 归档表：仅信息
        for (table, cols) in BOOT_ARCHIVE_DB {
            if !table_exists(&conn, table) {
                continue;
            }
            for col in *cols {
                let sql = format!(
                    "SELECT COUNT(*) FROM {table} WHERE {col} LIKE '%推演%' OR {col} LIKE '%面板%' \
                     OR {col} LIKE '%漏斗%' OR {col} LIKE '%因果流%'"
                );
                let n = count(&conn, &sql, &[]);
                if n > 0 {
                    checks.push(Check::info(
                        "启动",
                        format!("{table}.{col}"),
                        format!("{n} 行含设计层词（归档表·不注入·不阻塞）"),
                    ));
                }
            }
        }
    }

    // 3. 资料卡目录（角色/事件）
    let ref_codes = outline_ref_codes(lib_root, volume);
    let mut other_hits = 0;
    for dir in ["02_资料库/角色", "02_资料库/事件"] {
        let d = lib_root.join(dir);
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        let mut files: Vec<std::path::PathBuf> = rd.flatten().map(|e| e.path()).collect();
        files.sort();
        for p in files {
            if p.extension().map(|e| e != "md").unwrap_or(true) {
                continue;
            }
            let fname = p.file_name().unwrap_or_default().to_string_lossy().to_string();
            let Ok(txt) = std::fs::read_to_string(&p) else { continue };
            let hits = boot_find_hits(&txt);
            if hits.is_empty() {
                continue;
            }
            let stem = fname.trim_end_matches(".md").to_string();
            let is_ref = ref_codes.contains(&stem);
            let is_prot = BOOT_PROTAGONIST_MARK.iter().any(|m| fname.contains(m));
            let det = hits.iter().map(|(w, n)| format!("{w}×{n}")).collect::<Vec<_>>().join("、");
            if is_ref {
                checks.push(Check::warn(
                    "启动",
                    format!("{dir}/{fname}"),
                    det,
                    "本卷注入面内的卡含设计层词；注入期 sanitize_injection 会中性化，此处列清单供复核",
                ));
            } else if is_prot {
                checks.push(Check::warn(
                    "启动",
                    format!("{dir}/{fname}"),
                    det,
                    "主角其它期/终态卡（如终卷正典），不进本卷注入面；转卷前需复核",
                ));
            } else {
                other_hits += 1;
            }
        }
    }
    if other_hits > 0 {
        checks.push(Check::info(
            "启动",
            "非本卷引用卡",
            format!("{other_hits} 张含设计层词（不进本卷注入面；转卷前需复核）"),
        ));
    }

    // 4. 已生成正文（prev_tail 回流入口）
    let text_dir = lib_root.join("04_正文").join(volume);
    if let Ok(rd) = std::fs::read_dir(&text_dir) {
        let mut files: Vec<std::path::PathBuf> = rd.flatten().map(|e| e.path()).collect();
        files.sort();
        let mut bad = 0;
        for p in files {
            if p.extension().map(|e| e != "md").unwrap_or(true) {
                continue;
            }
            let fname = p.file_name().unwrap_or_default().to_string_lossy().to_string();
            let Ok(txt) = std::fs::read_to_string(&p) else { continue };
            let hits = boot_find_hits(&txt);
            if hits.is_empty() {
                continue;
            }
            bad += 1;
            let det = hits.iter().map(|(w, n)| format!("{w}×{n}")).collect::<Vec<_>>().join("、");
            checks.push(Check::warn(
                "启动",
                format!("04_正文/{volume}/{fname}"),
                det,
                "已生成正文含设计层词；会经 prev_tail 回流（注入期 sanitize_injection 兜底，但正文应干净）",
            ));
        }
        if bad == 0 {
            checks.push(Check::info("启动", "本卷正文", "无设计层词".into()));
        }
    }

    // 5. 细纲：① 章概要雷同；② 章文本设计层词（细纲是 L5 注入源，必须叙事化）
    if let Ok(txt) = std::fs::read_to_string(lib_root.join("03_叙事规划/细纲").join(format!("{volume}.md"))) {
        let sigs = outline_chapter_sigs(&txt);
        let dups = dup_runs(&sigs, 3);
        if dups.is_empty() {
            checks.push(Check::info("启动", "细纲章概要雷同", format!("{} 章，无连续雷同组", sigs.len())));
        } else {
            let involved: usize = dups.iter().map(|(_, _, n)| *n).sum();
            checks.push(Check::fatal(
                "启动",
                "细纲章概要雷同",
                format!("{} 章中 {involved} 章卷入 {} 个雷同组", sigs.len(), dups.len()),
                "细纲模板机械展开，正文会重复；用 `ne-agent outline rebuild` 差异化",
            ));
        }
        // 细纲章文本设计层词：源数据混层=「系统行」根因，判 Fatal（生成前必须清）。
        let oh = outline_body_design_hits(&txt);
        if oh.is_empty() {
            checks.push(Check::info("启动", "细纲章文本设计层词", "无".into()));
        } else {
            let total: usize = oh.iter().map(|(_, n)| *n).sum();
            let det = oh.iter().map(|(w, n)| format!("{w}×{n}")).collect::<Vec<_>>().join("、");
            checks.push(Check::fatal(
                "启动",
                "细纲章文本设计层词",
                format!("{total} 处：{det}"),
                "细纲章文本是 L5 注入源；含设计层词会经 prompt 渗成正文「系统行」。须叙事化后用 `ne-agent outline rebuild` 处理",
            ));
        }
    }

    Ok(Report::build("boot", lib_root, volume, None, checks))
}

// ───────────────────────────── 打印

/// 人类可读打印。
pub fn print_report(r: &Report) {
    let title = if r.kind == "preflight" {
        format!("生成前自检 {}（第{}章）", r.volume, r.chapter.unwrap_or(0))
    } else if r.kind == "boot" {
        format!("启动自检 {}", r.volume)
    } else {
        format!("模块自检 {}", r.volume)
    };
    println!("== {title}：{} 项，Fatal {}，Warn {}，{} ==",
        r.total, r.fatal, r.warn,
        if r.ready { "READY ✅" } else { "NOT READY ❌" });
    for c in &r.checks {
        let mark = match c.level {
            Level::Fatal => "❌",
            Level::Warn if c.ok => "⚠",
            Level::Warn => "⚠",
            Level::Info => "✅",
        };
        println!("  {mark} [{}] {} : {}", c.group, c.name, c.detail);
        if let Some(h) = &c.hint {
            println!("      ↳ 建议：{h}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_ready_follows_fatal() {
        let ok = Check::info("g", "n", "d".into());
        let warn = Check::warn("g", "w", "d".into(), "h");
        let fat = Check::fatal("g", "f", "d".into(), "h");
        let r1 = Report::build("modules", Path::new("/x"), "卷01", None, vec![ok.clone(), warn.clone()]);
        assert!(r1.ready, "仅 Warn 应 ready");
        let r2 = Report::build("preflight", Path::new("/x"), "卷01", Some(1), vec![ok, fat]);
        assert!(!r2.ready, "有 Fatal 不应 ready");
        assert_eq!(r2.fatal, 1);
        assert_eq!(r2.warn, 0);
    }

    #[test]
    fn missing_lib_root_is_fatal_and_not_ready() {
        let r = check_modules(Path::new("/definitely/not/a/lib/root"), "卷01").unwrap();
        assert!(!r.ready);
        assert!(r.fatal >= 5, "结构项应全报 Fatal：{r:?}");
    }

    #[test]
    fn boot_find_hits_skips_prohibition_lines() {
        // 显式禁令行合法提及设计词 → 不计
        let s = "严禁出现「面板」。\n他借面板推演未来。";
        let hits = boot_find_hits(s);
        let names: Vec<&str> = hits.iter().map(|(w, _)| *w).collect();
        assert!(names.contains(&"面板"), "{hits:?}");
        // 非禁令行里的面板/推演各算 1（禁令行的面板已被跳过）
        let panel = hits.iter().find(|(w, _)| *w == "面板").unwrap().1;
        assert_eq!(panel, 1, "禁令行的『面板』不应计入：{hits:?}");
        assert!(!boot_has_design_words("严禁出现「推演」「面板」"), "纯禁令行不应有命中");
    }

    #[test]
    fn boot_missing_lib_no_panic_not_ready() {
        // 空库根：不得 panic；无可注入文件/表→不产生 Fatal（结构缺失由 check_modules 负责）
        let d = std::env::temp_dir().join(format!("ne-agent-boot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let r = boot(&d, "卷01").unwrap();
        assert_eq!(r.kind, "boot");
        assert!(r.ready, "空库根 boot 不应有 Fatal：{r:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn outline_dup_runs_detects_copied_summaries() {
        let txt = "\
### 第1章 涧底醒
**概要**
- **目标**：确认自己还活着
- **障碍**：x
- **在场**：陈寂
- **阻碍**：涧水齐腰
- **选择**：上岸
### 第2章 涧底醒·阻碍具象
- **目标**：确认自己还活着
- **在场**：陈寂
- **阻碍**：涧水齐腰
- **选择**：上岸
### 第3章 涧底醒·选择挣扎
- **目标**：确认自己还活着
- **在场**：陈寂
- **阻碍**：涧水齐腰
- **选择**：上岸
### 第4章 老屋夜
- **目标**：弄清灵气为何物
- **在场**：陈寂、陈老头
- **阻碍**：记忆混乱
- **选择**：试通脉
";
        let sigs = outline_chapter_sigs(txt);
        assert_eq!(sigs.len(), 4, "{sigs:?}");
        let dups = dup_runs(&sigs, 3);
        assert_eq!(dups, vec![(1, 3, 3)], "第1~3章应判为雷同组：{dups:?}");
    }

    #[test]
    fn outline_distinct_summaries_no_dup() {
        let txt = "\
### 第1章 A
- **目标**：甲
- **在场**：A
- **阻碍**：甲阻
- **选择**：甲选
### 第2章 B
- **目标**：乙
- **在场**：B
- **阻碍**：乙阻
- **选择**：乙选
### 第3章 C
- **目标**：丙
- **在场**：C
- **阻碍**：丙阻
- **选择**：丙选
";
        let sigs = outline_chapter_sigs(txt);
        assert!(dup_runs(&sigs, 3).is_empty(), "无雷同不应报");
    }
}
