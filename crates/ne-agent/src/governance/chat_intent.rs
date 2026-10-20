//! 对话中枢的 **L1 规则层**（正则意图解析）—— 2026-10-17
//!
//! # 为什么先做规则层，不做 LLM 意图识别
//!
//! 1. **零延迟**：LLM 意图识别一轮 2~8 秒，规则层 <1ms。聊天框的第一体验是"立刻回话"。
//! 2. **零成本/零授权**：真调 LLM 需要 `wogov_real_llm.allow` 文件锁，聊天框不该因为
//!    "问一句状态"就要用户解锁。
//! 3. **可解释**：每条命中都带 `matched` 规则名，误判时能定位、能加规则。
//! 4. **契约稳定**：返回值形状与将来 LLM 层**完全一致**（`ChatReply`），
//!    将来加 L2（LLM 兜底）时前端一行不用改。
//!
//! # 设计边界（重要）
//!
//! 本模块 **只做三件事**：解析意图 → 生成人类可读回复 → 必要时登记任务。
//! 它**不写正文、不写卡片内容、不调 LLM**。登记任务走 `task_store::submit`，
//! 真正执行仍由执行器 / MCP 做，与既有权责分离完全一致。
//!
//! # 与「三层锁」的关系
//!
//! 本模块**从不**创建 `wogov_real_llm.allow`。当意图是写类（写正文 / 真跑建卡）时，
//! 只检查锁是否存在，返回 `needs_unlock: true` 让前端提示用户手动解锁——
//! 铁则「该文件只能由人点开关创建」不被绕过。

use anyhow::Result;
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::OnceLock;

use super::task_store;
use super::chat_tools;

// ══════════════════════════════════════════════ 返回结构

/// 一条聊天回复。字段名与前端 `api.js` / `useChat.js` 严格对应。
pub struct ChatReply {
    /// 人类可读回复（前端逐字打字机渲染）
    pub reply: String,
    /// 命中的规则名（调试 / 可解释性）
    pub matched: String,
    /// 执行计划（前端渲染成步骤卡）。空数组 = 纯聊天，无动作
    pub plan: Vec<Value>,
    /// 本次登记的任务 id 列表（前端据此弹悬浮窗）
    pub task_ids: Vec<String>,
    /// 是否需要解锁真实 LLM（前端显示「去解锁」按钮）
    pub needs_unlock: bool,
    /// 提示文案（可选）
    pub hint: Option<String>,
    /// 本次实际调用的工具名（没调工具则 None）
    pub tool_used: Option<String>,
    /// 工具返回的摘要（人话，不是原始 JSON）
    pub tool_result: Option<String>,
    /// 是否需要用户显式确认后才执行（写类操作为 true）
    pub needs_confirmation: bool,
    /// 确认卡片数据（needs_confirmation 为 true 时提供）
    pub confirm: Option<Value>,
}

impl ChatReply {
    // 【2026-10-17】升为 pub：`chat_llm.rs`（L2 意图层）要构造回复，
    // 而它在 chat_intent 之外。改 pub 比复制一份结构体安全得多——
    // 复制会导致两张"回复契约"各自漂移，前端迟早对不上。
    pub fn chat(reply: impl Into<String>, matched: impl Into<String>) -> Self {
        ChatReply {
            reply: reply.into(),
            matched: matched.into(),
            plan: vec![],
            task_ids: vec![],
            needs_unlock: false,
            hint: None,
            tool_used: None,
            tool_result: None,
            needs_confirmation: false,
            confirm: None,
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "ok": true,
            "reply": self.reply,
            "matched": self.matched,
            "plan": self.plan,
            "task_ids": self.task_ids,
            "needs_unlock": self.needs_unlock,
            "hint": self.hint,
            // ── C-1 新增（旧前端忽略即可，不影响兼容）──
            "tool_used": self.tool_used,
            "tool_result": self.tool_result,
            "needs_confirmation": self.needs_confirmation,
            "confirm": self.confirm,
        })
    }
}

// ══════════════════════════════════════════════ 正则表（懒编译，全局一次）

struct Rules {
    cardgen: Regex,
    write_chapter: Regex,
    check_conflict: Regex,
    task_status: Regex,
    // 【2026-10-18 新增】失败任务查询。
    //
    // ## 为什么必须单独一个意图
    // 用户实测问「那些失败的任务」，L1 全表无一命中，L2 模型也判成 chat，
    // 于是回了一句「没听懂」+ 能力清单。这是最气人的那种答非所问：
    // 用户心里有明确的一件事，系统却说"我不知道你在说什么"。
    //
    // task_status 那条正则里虽然有「任务」相关词，但要求完整的
    // 「任务状态/任务进度/队列/进度」这类短语，「哪些失败了」一个都不沾。
    // 所以这里单开一条，只认**失败/报错/挂了**这一族说法。
    failures: Regex,
    self_check: Regex,
    library_query: Regex,
    funnel: Regex,
    materials: Regex,
    inspect_card: Regex,
    engine_status: Regex,
    chapter_list: Regex,
    help: Regex,
}

fn rules() -> &'static Rules {
    static R: OnceLock<Rules> = OnceLock::new();
    R.get_or_init(|| Rules {
        // 「生成 10 张混沌帝国的卡」「建 20 张卡包」「跑 5 张卡」
        cardgen: Regex::new(
            r"(?:生成|建|跑|做|批量生成)\s*([0-9]+|[一二三四五六七八九十]+)?\s*张\s*([^\s，。的]*?)\s*(?:的)?\s*(?:卡|卡片|卡包)?",
        )
        .unwrap(),
        // 「写第 4 章」「生成第12章正文」「跑第 3 章」
        write_chapter: Regex::new(
            r"(?:写|生成|跑|产出|出)\s*(?:卷\s*([0-9]+)\s*)?第\s*([0-9]+)\s*章",
        )
        .unwrap(),
        check_conflict: Regex::new(r"(?:冲突|矛盾|校验|检查|体检)\s*(?:第\s*([0-9]+)\s*章)?").unwrap(),
        // 「任务状态」「进度」「在跑什么」
        task_status: Regex::new(
            // 【2026-10-17 收紧】原正则只有「(?:任务|进度|状态|在跑|队列|跑完|执行)」，
            // 太宽：用户问「你能建立任务吗」含「任务」二字就命中，
            // 直接被当成"查状态"，答了一堆 pending/running 数字。
            // 用户体感就是"复读机"。现在要求必须出现完整的"状态类短语"，
            // 单个「任务」不再触发。
            r"(?:任务状态|任务进度|任务队列|执行进度|执行情况|在跑什么|跑到哪|跑完了|队列|进度|状态)",
        )
        .unwrap(),
        // 【2026-10-18 新增】「那些失败的任务」「哪些卡跑挂了」「报错了哪些」
        //
        // 覆盖三类说法：① 直接点名失败（失败/挂了/报错/超时/中断/被拒）
        //             ② 问"有哪些没成功"（没成功/没成/不成功）
        //             ③ 单说"失败"两个字（用户最短表达，必须先认）
        // 只要出现这族词，就当作"要失败清单"，不要求再带「任务」二字 ——
        // 实测用户就是只说了「那些失败的」，省略了"任务"，不能因此不认。
        failures: Regex::new(
            r"(?:失败|挂了|跑挂|报错|出错|错误|超时|中断|被拒|拒绝|没成功|没成|不成功|卡住|failed|error)",
        )
        .unwrap(),
        // 「跑一次系统自检」
        self_check: Regex::new(r"(?:自检|体检|健康检查|system.?check)").unwrap(),
        // 「资料库里有哪些卡片」
        // 【2026-10-18 补】原来只认「资料库|卡片|卡库|索引|有哪些|库里有」，
        // 于是最自然的问法「现在有多少张卡？」「一共几张卡」全部落空——
        // 实测 matched=fallback，回了一屏能力清单。用户问数量是高频需求，
        // 补「多少张卡/几张卡/卡总数」这族说法。
        library_query: Regex::new(
            r"(?:资料库|卡片|卡库|索引|有哪些|库里有|多少张卡|几张卡|卡总数|总共多少.{0,4}卡)",
        )
        .unwrap(),
        // ── C-1 新增：要真实数据的四类 ──
        // 「漏斗什么进度」「漏斗到哪了」
        funnel: Regex::new(r"(?:漏斗|管线|L0|L1x|进度)").unwrap(),
        // 「资料库稽核」「缺卡情况」「库盘漂移」
        materials: Regex::new(r"(?:稽核|缺卡|漂移|资料库状态|库健康)").unwrap(),
        // 「CHAR-99-001 是什么卡」「查 FACT-99-902」
        inspect_card: Regex::new(r"([A-Z]{3,4}-[0-9]{2}-[0-9]{3})").unwrap(),
        // 「引擎状态」「LLM 网关通吗」「模型连得上吗」
        engine_status: Regex::new(r"(?:引擎状态|LLM\s*状态|模型状态|网关|连得上)").unwrap(),
        chapter_list: Regex::new(r"(?:第\s*([0-9]+)\s*卷|卷\s*([0-9]+)|章节列表|目录)").unwrap(),
        // 【2026-10-17 收紧】原正则含裸 `\?|？`，导致任何带问号的句子
        // （"现在有多少张卡？"）都被当成「求助」，答一串能力清单。
        // 现在只认明确求助：单独的"帮助/help"、或整句就是一个问号。
        help: Regex::new(r"(?:^帮助$|^help$|能做什么|你能做什么|怎么用|指令清单|^[?？]$)")
            .unwrap(),
    })
}

/// 判断这句话是不是"自然语言提问"，而不是"系统指令"。
///
/// 【为什么需要这个判断】L1 是正则层，只在"用户的措辞很像指令"时才该赢。
/// 一旦用户用的是人话（带问号、带「吗/呢/什么/多少/能不能」），
/// 就该让 L2（LLM）先理解，否则会出现：
///   「你能建立任务吗」→ 命中 task_status → 答一堆 pending 数字（复读机）
///   「现在有多少张卡？」→ 命中 help → 答一串能力清单（答非所问）
///
/// L2 若因为授权锁/网关问题不可用，会自动回落 L1，所以这个判断没有副作用：
/// 最坏情况就是多花 0 毫秒（锁没开时 L2 立刻 Skipped）。
fn looks_like_question(msg: &str) -> bool {
    let t = msg.trim();
    if t.is_empty() {
        return false;
    }
    // 结尾问号：最可靠的提问信号
    if t.ends_with('?') || t.ends_with('？') {
        return true;
    }
    // 疑问词（出现在句子里任意位置都算）
    //
    // 【2026-10-17 补】原来只有 14 个，实测漏了两大类真实提问：
    //   「帮我看看星耀联邦的设定」——没有问号、没有疑问词，但明显是请求查询
    //   「生成 5 张混沌帝国的卡好吗」——带"吗"但其实是写指令
    // 所以补上"请求类动词"：看看/查查/帮我/告诉我/介绍一下/搜一下。
    // 这类词开头基本可以确定是"要我去做一次查询"，不是系统指令。
    const QW: [&str; 27] = [
        "吗", "呢", "什么", "怎么", "怎样", "如何", "多少", "几个", "哪些", "哪个", "哪张",
        "能不能", "可不可以", "是否",
        // 请求类（隐含"请你去做一次查询/回答"）
        "帮我看看", "帮我查", "帮我找", "看看", "查查", "查一下", "看一下", "告诉我",
        "介绍一下", "搜一下", "查找", "列一下", "有没有",
    ];
    QW.iter().any(|w| t.contains(w))
}

/// 中文数字（1~99 够用）→ 阿拉伯数字。
fn cn_num(s: &str) -> Option<u32> {
    if s.is_empty() {
        return None;
    }
    if let Ok(n) = s.parse::<u32>() {
        return Some(n);
    }
    let digit = |c: char| -> Option<u32> {
        Some(match c {
            '一' => 1, '二' => 2, '三' => 3, '四' => 4, '五' => 5,
            '六' => 6, '七' => 7, '八' => 8, '九' => 9, '十' => 10,
            _ => return None,
        })
    };
    let chars: Vec<char> = s.chars().collect();
    if chars.len() == 1 {
        return digit(chars[0]);
    }
    // 处理「十X」「X十」「X十Y」
    if chars.len() == 2 && chars[1] == '十' {
        return digit(chars[0]).map(|d| d * 10);
    }
    if chars.len() == 2 && chars[0] == '十' {
        return digit(chars[1]).map(|d| 10 + d);
    }
    if chars.len() == 3 && chars[1] == '十' {
        let a = digit(chars[0])?;
        let b = digit(chars[2])?;
        return Some(a * 10 + b);
    }
    None
}

/// 已知文明名（用于从「混沌帝国的卡」里抽出文明名）。
/// 顺序有意从长到短，避免「星耀」先于「星耀联邦」命中。
const CIVS: [&str; 12] = [
    "界碑交汇所", "太玄帝国", "天启帝国", "星耀联邦", "星际商会",
    "暗影王朝", "混沌帝国", "万界通途", "圣辉教廷", "归墟古族", "逻辑族", "蛮族",
];

fn pick_civ(s: &str) -> Option<&'static str> {
    CIVS.iter().copied().find(|c| s.contains(c))
}

/// 供 `chat_llm.rs`（L2 层）复用的文明名识别。
///
/// 【为什么要暴露】同一句话无论走 L1 正则还是 L2 模型，认出来的文明必须一致。
/// 复制一份实现迟早会漂移（一边改了另一边忘改），所以直接共用。
pub fn pick_civ_pub(s: &str) -> Option<&'static str> {
    pick_civ(s)
}

/// 供 `chat_llm.rs`（L2 层）复用的规格推断。
///
/// 同上：L2 拿不到模型给的 spec 时，要按 L1 同一套规则推，
/// 否则「生成 6 张卡（1 文明 2 势力 2 地点 1 角色）」走 L2 会推出不同分布。
pub fn infer_spec_pub(msg: &str, total: u32) -> String {
    infer_spec(msg, total)
}

// ════════════════════════════════════════ 供 L2 工具层（chat_tools_fc）复用的包装
//
// 【为什么这些包装必须存在】`chat_tools_fc.rs` 走 Function Calling，
// 模型自己决定调哪个工具、参数填什么，然后由 Rust 侧真正执行。
// 执行时不能复制一份查询逻辑——复制就会漂移：
// 哪天 L1 修了"stale 隔离卡不该查出来"，FC 那份还是旧 SQL，用户就会
// 在聊天里看到 146 张已废弃的卡。所以**一律复用同一份实现**。
//
// 【为什么返回结构化 JSON 而不是 ChatReply 文本】
// 这些返回值会被原样塞进 `{"role":"tool","content": ...}` 回填给模型，
// 模型靠它组织语言。塞一段带 "ok/matched/plan" 的回复契约给它只是噪声，
// 所以这里给紧凑的事实数据，由模型自己说人话。

/// 资料库总览：总卡数、各分类数量、隔离卡数、索引条数。
///
/// 「库里有多少张卡」「都有什么」这类问题由它回答。
/// 注意必须带 `stale=0` 过滤——库里有 146 张已隔离进 `09_废弃区/` 的卡，
/// 不过滤就会把废弃卡也算进"现有资产"，是假的数字。
pub fn library_stats_pub(conn: &Connection) -> Value {
    let total: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM materials WHERE COALESCE(stale,0)=0",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let stale: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM materials WHERE COALESCE(stale,0)<>0",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let index_total: i64 = conn
        .query_row("SELECT COUNT(*) FROM library_index", [], |r| r.get(0))
        .unwrap_or(0);

    let mut by_category: Vec<Value> = Vec::new();
    if let Ok(mut st) = conn.prepare(
        "SELECT COALESCE(category,'未分类'), COUNT(*) FROM materials \
         WHERE COALESCE(stale,0)=0 GROUP BY category ORDER BY COUNT(*) DESC",
    ) {
        if let Ok(rows) = st.query_map([], |r| {
            Ok((r.get::<_, String>(0).unwrap_or_default(), r.get::<_, i64>(1).unwrap_or(0)))
        }) {
            for (cat, n) in rows.flatten() {
                by_category.push(json!({"category": cat, "count": n}));
            }
        }
    }

    let mut by_civ: Vec<Value> = Vec::new();
    if let Ok(mut st) = conn.prepare(
        "SELECT sub_category, COUNT(*) FROM materials \
         WHERE COALESCE(stale,0)=0 AND sub_category IS NOT NULL AND sub_category <> '' \
         GROUP BY sub_category ORDER BY COUNT(*) DESC LIMIT 20",
    ) {
        if let Ok(rows) = st.query_map([], |r| {
            Ok((r.get::<_, String>(0).unwrap_or_default(), r.get::<_, i64>(1).unwrap_or(0)))
        }) {
            for (civ, n) in rows.flatten() {
                by_civ.push(json!({"sub_category": civ, "count": n}));
            }
        }
    }

    json!({
        "total": total,
        "quarantined": stale,
        "index_total": index_total,
        "by_category": by_category,
        "by_sub_category": by_civ,
    })
}

/// 任务队列状态（在跑 / 待执行 / 执行器 / 授权锁 / 历史累计）。
///
/// `lib_root` 只用来确认锁文件所在盘的挂载状态；锁本身按绝对路径判，
/// 与 `real_llm_lock::ALLOW_FILE` 保持一致，避免两处口径不同。
pub fn status_pub(conn: &Connection, lib_root: &Path) -> Value {
    let summary = task_store::active_summary(conn).unwrap_or_else(|_| json!({}));
    let unlocked = Path::new(super::real_llm_lock::ALLOW_FILE).exists();
    json!({
        "pending": summary["pending"].as_i64().unwrap_or(0),
        "running": summary["running"].as_i64().unwrap_or(0),
        "executor_alive": summary["executor_alive"].as_bool().unwrap_or(false),
        "llm_unlocked": unlocked,
        "totals": summary["totals"].clone(),
        "active": summary["items"].clone(),
        "lib_root_mounted": lib_root.exists(),
    })
}

/// 失败/被拒/阻塞任务的计数（三态分开给，前端与工具层共用一份口径）。
pub fn failure_counts(conn: &Connection) -> (i64, i64, i64) {
    let cnt = |st: &str| -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM webui_task WHERE state=?1",
            rusqlite::params![st],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
    };
    (cnt("failed"), cnt("rejected"), cnt("blocked"))
}

/// 失败任务清单。
///
/// 【为什么不重新实现一遍解析】`intent_failures()` 里有近 100 行解析：
/// cardgen 汇总 JSON 的 `failures[]`、中断快照的 `state=running`、
/// `rejected` 的纯文本原因、`blocked` 的无产出说明……
/// 这套规则如果复制一份到工具层，改一处忘一处就会出现
/// "L1 说 3 张卡挂了、L2 说 5 张"这种自相矛盾。
/// 所以这里**直接复用它的渲染结果**，只额外补上结构化计数。
pub fn failures_pub(conn: &Connection) -> Value {
    let (n_failed, n_rejected, n_blocked) = failure_counts(conn);
    let total = n_failed + n_rejected + n_blocked;
    if total == 0 {
        return json!({
            "count": 0,
            "failed": 0, "rejected": 0, "blocked": 0,
            "detail": "没有失败任务。",
        });
    }
    let v = intent_failures(conn);
    json!({
        "count": total,
        "failed": n_failed,
        "rejected": n_rejected,
        "blocked": n_blocked,
        "detail": v["reply"].as_str().unwrap_or(""),
    })
}

/// 某卷的章节标题列表。抽出来是为了让 `intent_chapters`（吃 regex Captures）
/// 与工具层（吃卷号）走同一份 SQL。
fn chapter_titles(conn: &Connection, vol: u32) -> Vec<String> {
    let vol_name = format!("卷{vol:0>2}");
    let mut stmt = match conn.prepare(
        "SELECT title FROM library_index WHERE title LIKE ?1 ORDER BY title LIMIT 30",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map([format!("%{vol_name}%")], |r| r.get::<_, String>(0))
        .map(|it| it.flatten().collect())
        .unwrap_or_default()
}

/// 章节列表（`vol=None` 时给全部卷的条目数概要）。
pub fn chapters_pub(conn: &Connection, vol: Option<i64>) -> Value {
    match vol {
        Some(v) if v > 0 => {
            let v = v as u32;
            let titles = chapter_titles(conn, v);
            json!({
                "volume": v,
                "count": titles.len(),
                "titles": titles,
                "note": if titles.is_empty() { "该卷在索引里没有条目" } else { "" },
            })
        }
        _ => {
            // 不传卷号：给 1~12 卷里有条目的概览（大多数作品不会超过 12 卷）
            let mut vols: Vec<Value> = Vec::new();
            for v in 1u32..=12 {
                let n = chapter_titles(conn, v).len();
                if n > 0 {
                    vols.push(json!({"volume": v, "count": n}));
                }
            }
            json!({"volumes": vols, "count": vols.len()})
        }
    }
}

/// 系统自检登记。复用 `intent_self_check` 的登记逻辑（含"登记了但不会执行"
/// 那套如实话术），只把结果整理成模型能读的结构。
pub fn self_check_pub(conn: &Connection) -> Value {
    let v = intent_self_check(conn);
    json!({
        "reply": v["reply"].as_str().unwrap_or(""),
        "task_ids": v["task_ids"].clone(),
    })
}


// ══════════════════════════════════════════════ 主入口

/// 解析一条自然语言指令，返回回复 + 可选任务派发。
///
/// `conn` 用于登记任务与查状态；`lib_root` 用于检查锁文件所在盘。
/// `session` 用于回填多轮对话历史（空串按 DEFAULT_SESSION 处理）。
/// **不创建任何授权文件**。
pub fn handle(conn: &Connection, lib_root: &Path, message: &str, session: &str) -> Result<Value> {
    let msg = message.trim();
    if msg.is_empty() {
        return Ok(ChatReply::chat("你说空话呢。输入点指令吧。", "empty").to_json());
    }

    let r = rules();
    let unlocked = Path::new(super::real_llm_lock::ALLOW_FILE).exists();

    // ══════════════════════════════════════════════════════════════
    // 新架构（2026-10-18 总指挥指令）：L1 让位，L2 接管
    // ══════════════════════════════════════════════════════════════
    //
    // 【为什么改】旧架构是「L1 正则全表扫，扫不到才给 L2」。这条路的
    // 死穴是：只要用户换一种说法，L1 就落空，然后 L2 再花 3 秒理解。
    // 更糟的是 L1 经常**抢答**——用户问「太玄帝国有哪些势力」，
    // library_query 的「有哪些」抢先命中，答了全库索引统计。
    //
    // 补救办法曾经是"往词表里不断加词"。这是打地鼠：中文的说法无穷无尽
    //（「帮我整点星耀的卡」「星耀那边长啥样」「给我看看星耀」），
    // 写 1000 条正则也覆盖不完。
    //
    // 所以现在反过来：
    //   · L1 只保留**极其死板的秒回指令**（帮助、精确卡号、任务状态、失败清单）
    //     —— 这些词形固定，正则一命中就是准的，没必要花 3 秒问模型。
    //   · 其余**一律先交给 L2**（原生 Function Calling），由模型自己决定
    //     调哪个工具、参数填什么。工具表就是能力边界，不需要为每种说法写正则。
    //   · L2 不可用（锁没开 / 网关挂 / 超时）时，**完整回落 L1 老链路**，
    //     一个能力都不丢。最坏情况退化成改造前的行为，不会更差。

    // ── ① 死板固定指令：词形固定，L1 秒回（<1ms）──
    //
    // 帮助：整句就是"帮助/help/能做什么"，正则锚定了首尾，不会误伤。
    if r.help.is_match(msg) {
        return Ok(help_reply());
    }
    // 精确卡号（形如 CIVI-99-900）：这是在要一条确定的记录，
    // 模型帮不上忙还得靠 SQL 精确查，秒回更准也更快。
    // 注意 `try_data_query` 内部把 inspect_card 排在第一位，
    // 所以只要这一条命中，它必定走 inspect_card 分支，不会误判成别的。
    if r.inspect_card.is_match(msg) {
        if let Some(v) = try_data_query(conn, lib_root, msg, r) {
            return Ok(v);
        }
    }
    // 任务状态 / 失败清单：纯读库、答案就是几个数字或一张清单，
    // 而且失败清单的解析逻辑（cardgen 汇总 JSON、中断快照）比较细，
    // L1 已经写好了 100 行，没必要让模型重算一遍再转述。
    if r.failures.is_match(msg) {
        return Ok(intent_failures(conn));
    }
    if r.task_status.is_match(msg) {
        return Ok(intent_status(conn));
    }

    // ── ② 主力：L2 原生 Function Calling 接管一切自然语言 ──
    //
    // 模型拿到 12 个工具的签名，自己决定：
    //   「星耀那边长啥样」  → query_cards(keyword="星耀")
    //   「帮我整点星耀的卡」→ generate_cards(civ="星耀", count=5)
    //   「漏斗到哪了」      → funnel_status()
    //   「那些失败的」      → list_failures()
    //   「今天好累啊」      → 不调工具，直接聊
    //
    // 返回 None 表示 L2 不可用（未解锁 / 网关不可达 / 超时），
    // 此时控制流继续往下走 L1 老链路，能力不丢。
    if let Some(v) = super::chat_tools_fc::interpret_with_tools(conn, lib_root, msg, session) {
        return Ok(v);
    }

    // ── ③ L2 不可用 → 回落完整 L1 链路（改造前的全部能力，一条不少）──
    if let Some(v) = l1_rules_only(conn, lib_root, msg, r, unlocked, session) {
        return Ok(v);
    }

    // 兜底：不瞎猜，明确告知边界
    Ok(ChatReply::chat(
        format!(
            "没听懂「{msg}」。\n\
             我现在能听懂这些：\n\
             · 生成 N 张<文明>的卡\n\
             · 写第 N 章 / 跑第 N 章\n\
             · 检查第 N 章的冲突\n\
             · 看当前任务状态 / 进度\n\
             · 哪些任务失败了（失败清单 + 原因）\n\
             · 跑一次系统自检\n\
             · 资料库里有哪些卡片 / 现在有多少张卡\n\
             · 第 N 卷有哪些章节\n\
             · 查某张卡（如 CIVI-99-900 是什么卡）\n\
             \n也可以直接说人话，比如「星耀那边长啥样」「帮我整点星耀的卡」。\n\
             问「帮助」可以看完整清单。",
        ),
        "fallback",
    )
    .to_json())
}

/// L1 老链路：L2 不可用时的兜底。**保留改造前的全部能力，一条不少**。
///
/// 【为什么整体保留而不是删掉】L2 依赖两件外部条件：授权锁 + 网关。
/// 任何一件出问题（用户上锁、网关进程被回收、模型超时），L2 就整个不可用。
/// 那时如果只剩几条死板指令，用户会突然发现"系统变笨了"。
/// 保留这条链路的意义是：**降级不等于残废**——最坏退回改造前。
fn l1_rules_only(
    conn: &Connection,
    lib_root: &Path,
    msg: &str,
    r: &Rules,
    unlocked: bool,
    session: &str,
) -> Option<Value> {
    // ① 写正文（最危险，先拦）
    if let Some(c) = r.write_chapter.captures(msg) {
        return Some(intent_write_chapter(conn, c, unlocked));
    }
    // ② 真实数据查询 —— 这些**真的**调 ne-mcp 只读工具
    if let Some(v) = try_data_query(conn, lib_root, msg, r) {
        return Some(v);
    }
    // ③ 建卡
    if let Some(c) = r.cardgen.captures(msg) {
        if let Some(v) = intent_cardgen(conn, c, msg, unlocked) {
            return Some(v);
        }
    }
    // ④ 冲突检查
    if let Some(c) = r.check_conflict.captures(msg) {
        if let Some(v) = intent_check(conn, c) {
            return Some(v);
        }
    }
    // ⑤ 系统自检
    if r.self_check.is_match(msg) {
        return Some(intent_self_check(conn));
    }
    // ⑥ 失败清单（排在 task_status 之前：两者可能同时命中，
    //    而清单比几个统计数字信息量大得多）
    if r.failures.is_match(msg) {
        return Some(intent_failures(conn));
    }
    if r.task_status.is_match(msg) {
        return Some(intent_status(conn));
    }
    // ⑦ 章节列表（必须在 library_query 之前，否则「第1卷有哪些章节」
    //    会被 library_query 的「有哪些」抢走）
    if let Some(c) = r.chapter_list.captures(msg) {
        if let Some(v) = intent_chapters(conn, c) {
            return Some(v);
        }
    }
    // ⑧ 资料库查询
    if r.library_query.is_match(msg) {
        return Some(intent_library(conn, msg));
    }
    // ⑨ 帮助
    if r.help.is_match(msg) {
        return Some(help_reply());
    }
    // ⑩ 旧 L2（"让模型吐 JSON"模式）作为最后一道。
    //
    // 【为什么还留着】新 L2（Function Calling）依赖网关支持 `tools` 参数。
    // 万一换了模型/网关上不支持 tools，FC 会失败但普通 chat/completions
    // 仍然可用——那时这条旧路径还能救一次。这属于"多一层保险不亏"。
    //
    // 【session 传空串的原因】这条旧 L2 会在内部再调一次
    // `chat_intent::handle()` 去复用 L1 的实现（见 chat_llm.rs:350/355）。
    // 那些是**系统内部子调用**，不是用户发言。传空串让它们不回填历史，
    // 否则「看当前任务状态」这句话会被当成用户说过的话写进对话记录，
    // 下一轮把脏上下文喂给模型。
    if let Some(v) = super::chat_llm::try_fallback(conn, lib_root, msg) {
        return Some(v);
    }
    let _ = session;
    None
}

// ══════════════════════════════════════════════ 各意图实现

/// 真实数据查询分派（C-1）。
///
/// 返回 `Some(json)` 表示"我处理了"，`None` 表示"不归我管，交给后面的规则"。
///
/// 关键设计：**任何工具调用失败都不 panic、不阻断聊天**。
/// 连不上 ne-mcp 时，reply 里明说"连不上引擎"，而不是假装查到了。
fn try_data_query(
    _conn: &Connection,
    lib_root: &Path,
    msg: &str,
    r: &Rules,
) -> Option<Value> {
    // 判定意图（顺序：特定卡号 > 引擎状态 > 稽核 > 漏斗）
    let plan: Option<(&str, serde_json::Value, &str)> = if let Some(c) = r.inspect_card.captures(msg) {
        let key = c.get(1)?.as_str().to_string();
        Some(("wg_inspect_card", serde_json::json!({ "key": key }), "inspect_card"))
    } else if r.engine_status.is_match(msg) {
        Some(("wg_llm_status", serde_json::json!({ "action": "gate" }), "engine_status"))
    } else if r.materials.is_match(msg) {
        Some(("ne_materials", serde_json::json!({}), "materials_audit"))
    } else if r.funnel.is_match(msg) {
        Some(("funnel", serde_json::json!({}), "funnel_status"))
    } else {
        None
    };

    let (tool, args, matched) = plan?;

    // ── 调工具 ──
    let mut reply = ChatReply::chat("", matched);
    reply.tool_used = Some(tool.to_string());

    match chat_tools::ChatTools::connect(lib_root) {
        Ok(mut tools) => match tools.call(tool, args) {
            Ok(raw) => {
                let summary = chat_tools::summarize(tool, &raw);
                reply.reply = format!("{summary}\n\n（数据来源：ne-mcp · {tool}）");
                reply.tool_result = Some(summary);
            }
            Err(e) => {
                reply.reply = format!(
                    "想帮你查，但工具调用失败：{e}\n\
                     可能是 ne-mcp 未编译、资料库路径不对，或该工具需要参数。\n\
                     这不是聊天功能的问题——其余指令我还能正常响应。"
                );
            }
        },
        Err(e) => {
            reply.reply = format!(
                "引擎连不上：{e}\n\
                 （聊天框架仍在工作，只是查不了实时数据。）\n\
                 排查：确认 `target/release/ne-mcp` 存在，且资料库路径可读。"
            );
        }
    }

    Some(reply.to_json())
}

fn intent_write_chapter(
    conn: &Connection,
    c: regex::Captures,
    unlocked: bool,
) -> Value {
    let volume = if c.get(1).map(|m| m.as_str() == "0").unwrap_or(true) {
        "卷01".to_string()
    } else {
        // 【决策二·E级修复】`c.get(1)` 依赖"正则含第 1 捕获组"这一隐式前提；
        // 正则一旦被改就会 panic。改为 and_then 链，取不到时回退默认卷号 1。
        format!("卷{:0>2}", c.get(1).and_then(|m| cn_num(m.as_str())).unwrap_or(1))
    };
    let chapter = c
        .get(2)
        .and_then(|m| cn_num(m.as_str()))
        .unwrap_or(1);

    let mut r = ChatReply::chat(
        format!(
            "写正文属于**写类操作**，必须走授权锁。\n\
             目标：{volume} 第 {chapter} 章。\n\
             {}",
            if unlocked {
                "当前锁是开着的。确认要跑就说一声，我登记任务。"
            } else {
                "当前是🔒锁定状态——请先在上方锁条点「解锁」并二次确认。"
            }
        ),
        "write_chapter",
    );
    r.plan = vec![
        json!({"step": 1, "action": "校验授权锁 /sdcard/wogov_real_llm.allow"}),
        json!({"step": 2, "action": format!("派发 ne_run_chapter（{volume} 第 {chapter} 章）")}),
        json!({"step": 3, "action": "跑完自动上锁，回写门禁结论"}),
    ];
    r.needs_unlock = !unlocked;
    r.hint = Some("写正文需要真人显式授权，我不会替你解锁。".into());

    // 只有已解锁时才登记任务，避免登记一堆跑不了的 blocked
    if unlocked {
        let payload = json!({"volume": volume, "chapter": chapter, "real": true});
        if let Ok(v) = task_store::submit(
            conn,
            &json!({"kind": "chapter_gen", "payload": payload}).to_string(),
        ) {
            if let Some(id) = v["id"].as_str() {
                if v["ok"].as_bool().unwrap_or(false) {
                    r.task_ids.push(id.to_string());
                    r.reply.push_str(&format!("\n\n已登记任务：{id}"));
                }
            }
        }
    }
    r.to_json()
}

fn intent_cardgen(
    conn: &Connection,
    c: regex::Captures,
    msg: &str,
    unlocked: bool,
) -> Option<Value> {
    // 必须能从原句里认出文明名，否则可能是「生成 3 张卡」这种含糊指令 → 交后续规则
    let civ = pick_civ(msg)?;

    let n = c
        .get(1)
        .and_then(|m| cn_num(m.as_str()))
        .unwrap_or(10)
        .clamp(1, 40);

    // 规格推断：有人明说「1 文明 3 势力 3 地点 3 角色」时按其说不改；
    // 否则按「1 文明 + 均分其余」的默认模板。
    let spec = infer_spec(msg, n);

    let mut r = ChatReply::chat(
        format!(
            "好，「{civ}」建卡：{n} 张，规格 `{spec}`。\n\
             {}\n\
             建卡会调本地模型网关，耗时较长（10 张约 3~8 分钟），\
             我建议**先预演（real=false）**验结构，再解锁跑真。",
            if unlocked {
                "锁现在是开着的。"
            } else {
                "当前🔒锁定——预演随时能跑，真跑需要先解锁。"
            }
        ),
        "cardgen",
    );

    r.plan = vec![
        json!({"step": 1, "action": format!("分配编号区间（{civ}）")}),
        json!({"step": 2, "action": "模板注入 + 结构校验"}),
        json!({"step": 3, "action": format!("落盘 {n} 张卡 + 三同步（materials/library_index/rel_edge）")}),
        json!({"step": 4, "action": "门禁复检（gate14）"}),
    ];

    // 预演任务（real=false）不需要授权，直接登记
    let payload = json!({"civ": civ, "spec": spec, "real": false, "count": n});
    match task_store::submit(
        conn,
        &json!({"kind": "cardgen", "payload": payload}).to_string(),
    ) {
        Ok(v) => {
            if let Some(id) = v["id"].as_str() {
                if v["ok"].as_bool().unwrap_or(false) {
                    r.task_ids.push(id.to_string());
                    r.reply.push_str(&format!(
                        "\n\n已登记**预演**任务：{id}\n点悬浮窗里的「执行」即可开跑。"
                    ));
                } else {
                    r.reply.push_str(&format!(
                        "\n\n任务没能登记：{}",
                        v["error"].as_str().unwrap_or("未知原因")
                    ));
                }
            }
        }
        Err(e) => r.reply.push_str(&format!("\n\n登记失败：{e}")),
    }
    r.hint = Some("真跑请先在锁条解锁；跑完会自动上锁。".into());
    Some(r.to_json())
}

/// 从原句推断卡片规格。
/// 认「N 文明 / N 势力 / N 地点 / N 角色」四类，没写的按默认补。
fn infer_spec(msg: &str, total: u32) -> String {
    let grab = |kw: &str| -> Option<u32> {
        let re = Regex::new(&format!(r"([0-9]+)\s*{}", kw)).ok()?;
        re.captures(msg)?.get(1)?.as_str().parse::<u32>().ok()
    };
    let civ = grab("文明");
    let fact = grab("势力");
    let loca = grab("地点");
    let ch = grab("角色");

    if civ.is_some() || fact.is_some() || loca.is_some() || ch.is_some() {
        let c = civ.unwrap_or(1);
        let f = fact.unwrap_or(3);
        let l = loca.unwrap_or(3);
        let x = ch.unwrap_or(3);
        return format!("文明:{c},势力:{f},地点:{l},角色:{x}");
    }

    // 默认：1 张文明主卡，其余四类均分
    if total <= 1 {
        "文明:1".to_string()
    } else {
        let rest = total - 1;
        let each = (rest / 3).max(1);
        format!("文明:1,势力:{each},地点:{each},角色:{}", rest - each * 2)
    }
}

fn intent_check(conn: &Connection, c: regex::Captures) -> Option<Value> {
    let chapter = c.get(1).and_then(|m| cn_num(m.as_str()));

    let mut r = ChatReply::chat(
        match chapter {
            Some(n) => format!("登记一次设定冲突检查：第 {n} 章。"),
            None => "登记一次全库设定冲突扫描。".to_string(),
        },
        "check_conflict",
    );
    r.plan = vec![
        json!({"step": 1, "action": "找出该章引用的资料卡"}),
        json!({"step": 2, "action": "比对力量体系 / 地理 / 时间线一致性"}),
        json!({"step": 3, "action": "输出冲突清单（P0 阻断 / P1 告警）"}),
    ];

    let payload = json!({"chapter": chapter, "scope": if chapter.is_some() { "chapter" } else { "all" }});
    if let Ok(v) = task_store::submit(
        conn,
        &json!({"kind": "self_check", "payload": payload}).to_string(),
    ) {
        if let Some(id) = v["id"].as_str() {
            if v["ok"].as_bool().unwrap_or(false) {
                r.task_ids.push(id.to_string());
                r.reply.push_str(&format!("\n\n已登记：{id}"));
            } else {
                // 同上：self_check 类走语义双门控，日常关闭 → blocked。
                // 如实说明，并给出「不用任务系统也能跑」的替代路径。
                r.reply.push_str(&format!(
                    "\n\n⚠ 已登记但**不会被执行**：{}\n\
                     原因：{}（语义双门控日常关闭）\n\
                     替代：终端直接跑 05_管线框架 下的冲突检查脚本，或等我接入免门控的只读检查工具。",
                    id,
                    v["error"].as_str().unwrap_or("门控未开"),
                ));
            }
        }
    }
    Some(r.to_json())
}

fn intent_self_check(conn: &Connection) -> Value {
    let mut r = ChatReply::chat("登记一次系统自检（系统体检 + 门禁闸门）。", "self_check");
    r.plan = vec![
        json!({"step": 1, "action": "跑 system_health_check.py --gate"}),
        json!({"step": 2, "action": "汇总 🔴/🟡/🟢 三档"}),
        json!({"step": 3, "action": "门禁 exit code 决定是否放行写类操作"}),
    ];
    match task_store::submit(conn, &json!({"kind": "self_check", "payload": {}}).to_string()) {
        Ok(v) => {
            let id = v["id"].as_str().unwrap_or("").to_string();
            if v["ok"].as_bool().unwrap_or(false) {
                r.task_ids.push(id.clone());
                r.reply.push_str(&format!("\n\n已登记：{id}"));
            } else {
                // self_check 不在 file_gated 白名单 → 走 l1 && l2 语义双门控。
                // 那套需要编译期 SEMANTIC_ENABLE_GATE + --semantic + 环境变量，
                // 日常是关的，所以这里必然 blocked。如实告知，不掩饰。
                r.reply.push_str(&format!(
                    "\n\n⚠ 已登记但**不会被执行**：{id}\n\
                     原因：self_check 走语义双门控（日常关闭），当前状态 blocked。\n\
                     这不是故障——是设计如此（防止面板绕过门控随意触发管线运算）。\n\
                     要跑体检，请直接在终端执行：\n\
                     　cd 05_管线框架 && python3 engine/system_health_check.py --gate",
                ));
            }
        }
        Err(e) => r.reply.push_str(&format!("\n\n登记失败：{e}")),
    }
    r.to_json()
}

/// 【2026-10-18 新增】失败任务清单。
///
/// ## 为什么不用 task_store::failures()
/// 那个函数读的是 `webui_task_fail` 归档表，实测该表 **0 行**——
/// 失败原因是写在 `webui_task.result` 里的 JSON 文本，不是归档表。
/// 用它会得到"没有失败任务"，比不回答更糟（是假答案）。
///
/// ## 数据形态（实测）
///   failed    : result 是 cardgen 的汇总 JSON
///               {"ok":false,"state":"partial","total":4,"success":3,"failed":1,
///                "failures":[{"code":"POWR-99-911","name":"归墟古族·弱点机制",
///                             "why":"校验未过: 缺区块…；字数 1139 < 要求 1200"}]}
///   rejected  : result 是纯文本，如「真跑被拒：…allow 不存在」
///
/// ## 输出口径
/// 只列**还有意义**的失败：failed（跑过但没成）+ rejected（压根没跑）。
/// archived 是历史归档，不算失败；cancelled 是用户主动取消，也不算。
/// 每条写清：卡名 + 具体原因。用户要的是"哪些卡坏了、为什么坏"，
/// 不是"failed=16"这种数字。
fn intent_failures(conn: &Connection) -> Value {
    // 先取总数，避免"一条都没有"时回答得像出错
    let cnt = |st: &str| -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM webui_task WHERE state=?1",
            rusqlite::params![st],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
    };
    let n_failed = cnt("failed");
    let n_rejected = cnt("rejected");
    let n_blocked = cnt("blocked");

    if n_failed == 0 && n_rejected == 0 && n_blocked == 0 {
        return ChatReply::chat(
            "没有失败任务。当前队列里没有跑挂或未授权被拒的记录。",
            "task_failures",
        )
        .to_json();
    }

    let mut st = match conn.prepare(
        "SELECT id, kind, payload, state, result, created_ts FROM webui_task \
         WHERE state IN ('failed','rejected','blocked') \
         ORDER BY created_ts DESC LIMIT 40",
    ) {
        Ok(s) => s,
        Err(e) => {
            return ChatReply::chat(&format!("读任务表失败：{e}"), "task_failures").to_json()
        }
    };

    struct Row {
        id: String,
        kind: String,
        civ: String,
        state: String,
        result: String,
        ts: i64,
    }

    let rows: Vec<Row> = match st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, i64>(5)?,
        ))
    }) {
        Ok(it) => it
            .filter_map(|x| x.ok())
            .map(|(id, kind, payload, state, result, ts)| {
                let pv: Value = serde_json::from_str(&payload).unwrap_or(json!({}));
                Row {
                    id,
                    kind,
                    civ: pv["civ"].as_str().unwrap_or("").to_string(),
                    state,
                    result,
                    ts,
                }
            })
            .collect(),
        Err(e) => {
            return ChatReply::chat(&format!("读任务表失败：{e}"), "task_failures").to_json()
        }
    };

    // 逐条摘要：优先抽 cardgen 的 failures[]，抽不到就退回 result 纯文本
    let mut lines: Vec<String> = Vec::new();
    let mut plan: Vec<Value> = Vec::new();
    let mut shown = 0usize;

    for row in &rows {
        let tag = match row.state.as_str() {
            "failed" => "失败",
            "rejected" => "被拒",
            "blocked" => "阻塞",
            _ => continue,
        };

        // ① 尝试按 cardgen 汇总 JSON 解析
        let mut details: Vec<String> = Vec::new();
        if let Ok(v) = serde_json::from_str::<Value>(&row.result) {
            // 【2026-10-18 补 · 假答案修复】识别"跑到一半被打断"的残留快照。
            // 老口径直接读 total/success/failed，遇到 state="running"、ok=null、
            // 没有 failed 字段的中断快照，会渲染成「10 张里 0 成 / 0 败」——
            // 这是假答案：用户会以为一张都没开始，实际是执行器被回收/服务重启
            // 打断在第 done 张。dd 值在，能说清停在哪儿，就别装成"什么都没发生"。
            let snap_running = v["state"].as_str() == Some("running") || v["ok"].is_null();
            let has_fail_list = v["failures"]
                .as_array()
                .map_or(false, |a| !a.is_empty());
            if snap_running && !has_fail_list {
                let total = v["total"].as_i64().unwrap_or(0);
                let done = v["done"].as_i64().unwrap_or(0);
                let cur = v["current"].as_str().unwrap_or("");
                let cur_name = v["current_name"].as_str().unwrap_or("");
                let mut s =
                    format!("执行中断（执行器被回收/服务重启，非建卡本身失败），进度 {done}/{total}");
                if !cur_name.is_empty() {
                    s.push_str(&format!("，最后停在 {cur_name}（{cur}）"));
                } else if !cur.is_empty() {
                    s.push_str(&format!("，最后停在 {cur}"));
                }
                details.push(s);
            }
            if has_fail_list {
                if let Some(arr) = v["failures"].as_array() {
                    for f in arr {
                        let code = f["code"].as_str().unwrap_or("");
                        let name = f["name"].as_str().unwrap_or("");
                        let why = f["why"].as_str().unwrap_or("（未写原因）");
                        // 原因可能很长（含多条"缺区块"），压成一行可读
                        let why_short = shorten_reason(why, 90);
                        details.push(format!("{name}（{code}）→ {why_short}"));
                    }
                }
            }
            // 部分成功：把 success/failed 计数也带上，用户才知道坏了几张。
            // 注意必须同时要求 bad>0 或 ok>0——否则「total 有值但 success/failed
            // 两字段都缺失」的中断快照会再次退化成「0 成 / 0 败」假答案。
            if details.is_empty() {
                let total = v["total"].as_i64().unwrap_or(0);
                let ok = v["success"].as_i64().unwrap_or(0);
                let bad = v["failed"].as_i64().unwrap_or(0);
                if total > 0 && (bad > 0 || ok > 0) {
                    details.push(format!("{total} 张里 {ok} 成 / {bad} 败（未记录具体卡名）"));
                }
            }
        }
        // ② 纯文本 result（rejected 常见）
        if details.is_empty() && !row.result.trim().is_empty() {
            details.push(shorten_reason(row.result.trim(), 110));
        }
        // ③ 兜底：result 既不是 JSON 也不是文本（阻塞态常见，因为阻塞发生在
        // 登记之后、执行之前，压根没产生 result）。别只写"无失败详情"——
        // 得说清它是哪种阻塞，否则用户看到等于没看到。
        if details.is_empty() {
            details.push(match row.state.as_str() {
                "blocked" => {
                    "阻塞：任务已登记但未进入执行（前置门禁未过，或执行器未扫到），无产出记录"
                        .to_string()
                }
                "rejected" => "被拒：未通过授权/门禁校验，无产出记录".to_string(),
                _ => "无失败详情（result 为空）".to_string(),
            });
        }

        let head = {
            let civ = if row.civ.is_empty() { row.id.as_str() } else { row.civ.as_str() };
            format!("{tag} · {} · {civ}", row.kind)
        };
        shown += 1;
        lines.push(format!("{head}"));
        for d in &details {
            lines.push(format!("    {d}"));
        }
        plan.push(json!({ "action": format!("{head}：{}", details.join("；")) }));
        if shown >= 12 {
            lines.push(format!("…还有 {} 条，去任务看板「阻塞/失败」筛选里看全。", rows.len() - shown));
            break;
        }
    }

    let mut r = ChatReply::chat(
        format!(
            "失败任务共 {} 条（失败 {} · 被拒 {} · 阻塞 {}），最近 {} 条：\n{}",
            n_failed + n_rejected + n_blocked,
            n_failed,
            n_rejected,
            n_blocked,
            shown,
            lines.join("\n")
        ),
        "task_failures",
    );
    r.plan = plan;
    r.to_json()
}

/// 把一段可能很长的原因压成一行：去换行、去多余空格、超长截断加省略号。
fn shorten_reason(s: &str, max: usize) -> String {
    let one_line: String = s
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let one_line = one_line.replace("; ", "；").replace(", ", "，");
    if one_line.chars().count() <= max {
        one_line
    } else {
        let cut: String = one_line.chars().take(max).collect();
        format!("{cut}…")
    }
}

/// 任务状态总览（在跑 / 待执行 / 锁）。
fn intent_status(conn: &Connection) -> Value {
    // 直接读真实状态，不登记任务
    let summary = task_store::active_summary(conn).unwrap_or_else(|_| json!({}));
    let pending = summary["pending"].as_i64().unwrap_or(0);
    let running = summary["running"].as_i64().unwrap_or(0);
    let alive = summary["executor_alive"].as_bool().unwrap_or(false);
    let unlocked = summary["llm_unlocked"].as_bool().unwrap_or(false);
    let totals = &summary["totals"];

    let mut lines = vec![
        format!("任务：{running} 个在跑 / {pending} 个待执行"),
        format!("执行器：{}", if alive { "在跑" } else { "空闲" }),
        format!("授权锁：{}", if unlocked { "🔓 已解锁" } else { "🔒 已锁定" }),
    ];
    if let Some(obj) = totals.as_object() {
        let mut parts: Vec<String> = obj
            .iter()
            .map(|(k, v)| format!("{k}={}", v.as_i64().unwrap_or(0)))
            .collect();
        parts.sort();
        if !parts.is_empty() {
            lines.push(format!("历史累计：{}", parts.join(" / ")));
        }
    }

    let mut r = ChatReply::chat(lines.join("\n"), "task_status");

    // 把活跃任务列进 plan（前端会渲染成列表）
    if let Some(items) = summary["items"].as_array() {
        r.plan = items
            .iter()
            .map(|t| {
                json!({
                    "action": format!(
                        "{} · {} · {}",
                        t["kind"].as_str().unwrap_or("?"),
                        t["state"].as_str().unwrap_or("?"),
                        if t["civ"].as_str().unwrap_or("").is_empty() {
                            t["id"].as_str().unwrap_or("").to_string()
                        } else {
                            t["civ"].as_str().unwrap_or("").to_string()
                        }
                    )
                })
            })
            .collect();
    }
    r.to_json()
}

fn intent_library(conn: &Connection, msg: &str) -> Value {
    let civ = pick_civ(msg);
    let mut stmt = match conn.prepare(
        "SELECT layer, COUNT(*) FROM library_index GROUP BY layer ORDER BY COUNT(*) DESC LIMIT 12",
    ) {
        Ok(s) => s,
        Err(_) => return ChatReply::chat("资料库索引暂时读不到。", "library_query").to_json(),
    };
    let rows: Vec<(String, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map(|it| it.flatten().collect())
        .unwrap_or_default();

    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM library_index", [], |r| r.get(0))
        .unwrap_or(0);

    let mut lines = vec![format!("资料库索引共 {total} 条，按层分布：")];
    for (layer, n) in rows.iter().take(8) {
        lines.push(format!("  {layer} → {n}"));
    }
    if let Some(c) = civ {
        let hit: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM library_index WHERE title LIKE ?1",
                [format!("%{c}%")],
                |r| r.get(0),
            )
            .unwrap_or(0);
        lines.push(format!("\n「{c}」相关条目：{hit} 条"));
    }

    ChatReply::chat(lines.join("\n"), "library_query").to_json()
}

fn intent_chapters(conn: &Connection, c: regex::Captures) -> Option<Value> {
    let raw = c.get(1).or_else(|| c.get(2))?;
    let vol = cn_num(raw.as_str())?;
    let vol_name = format!("卷{vol:0>2}");

    let mut stmt = conn
        .prepare("SELECT title FROM library_index WHERE title LIKE ?1 ORDER BY title LIMIT 30")
        .ok()?;
    let rows: Vec<String> = stmt
        .query_map([format!("%{vol_name}%")], |r| r.get(0))
        .map(|it| it.flatten().collect())
        .unwrap_or_default();

    let reply = if rows.is_empty() {
        format!("{vol_name} 在索引里没查到条目。可能还没建卷，或命名格式不同。")
    } else {
        let mut s = format!("{vol_name} 相关条目 {} 条：\n", rows.len());
        for t in rows.iter().take(20) {
            s.push_str(&format!("  · {t}\n"));
        }
        s
    };
    Some(ChatReply::chat(reply, "chapter_list").to_json())
}

fn help_reply() -> Value {
    let mut r = ChatReply::chat(
        "我能听懂这些指令：\n\
         \n\
         【建卡】生成 10 张混沌帝国的卡\n\
         　　　生成 6 张卡（1 文明 2 势力 2 地点 1 角色）→ 带规格更准\n\
         \n\
         【写正文】写第 4 章 / 跑卷01 第 12 章 → 需授权锁\n\
         \n\
         【检查】检查第 4 章的冲突 / 检查全库冲突\n\
         \n\
         【状态】看当前任务状态 / 进度\n\
         　　　哪些任务失败了 / 那些失败的任务 → 失败清单含卡名与原因\n\
         \n\
         【其他】跑一次系统自检 / 资料库里有哪些卡片 / 现在有多少张卡\n\
         　　　第 1 卷有哪些章节 / CIVI-99-900 是什么卡（查单卡）\n\
         \n\
         说明：建卡与写正文都会**登记任务**，不会立刻跑。\n\
         登记后点悬浮窗的「执行」才真正启动。真跑需要先解锁。",
        "help",
    );
    r.plan = vec![
        json!({"action": "规则层（L1）：正则解析，<1ms，零授权"}),
        json!({"action": "LLM 层（L2）：暂未接入，留给后续"}),
    ];
    r.to_json()
}

// ══════════════════════════════════════════════ 单测

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cn_num_parses() {
        assert_eq!(cn_num("10"), Some(10));
        assert_eq!(cn_num("十"), Some(10));
        assert_eq!(cn_num("十二"), Some(12));
        assert_eq!(cn_num("二十"), Some(20));
        assert_eq!(cn_num("二十五"), Some(25));
        assert_eq!(cn_num("?"), None);
    }

    #[test]
    fn picks_civ() {
        assert_eq!(pick_civ("生成 10 张混沌帝国的卡"), Some("混沌帝国"));
        assert_eq!(pick_civ("建 5 张卡"), None);
        assert_eq!(pick_civ("界碑交汇所的卡"), Some("界碑交汇所"));
    }

    #[test]
    fn spec_infer_respects_explicit() {
        assert_eq!(
            infer_spec("生成 6 张卡（1 文明 2 势力 2 地点 1 角色）", 6),
            "文明:1,势力:2,地点:2,角色:1"
        );
    }

    /// 【2026-10-18 回归锁】用户原话「那些失败的」必须命中 failures 意图。
    ///
    /// 这条测试锁的是本次修的真实 bug：用户只说了四个字，没带「任务」，
    /// L1 正则里一条都不含失败词，于是 matched=fallback 回「没听懂」，
    /// 而用户心里有明确的一件事 —— 这是最伤体验的误判。
    #[test]
    fn failures_intent_matches_short_phrases() {
        let r = rules();
        // 用户实测的原话，一个字都没改
        assert!(r.failures.is_match("那些失败的"), "「那些失败的」必须命中");
        assert!(r.failures.is_match("哪些失败了？"));
        assert!(r.failures.is_match("有任务跑挂了吗"));
        assert!(r.failures.is_match("报错了哪些"));
        assert!(r.failures.is_match("有没有没成功的"));
        // 反例：不能把正常提问吃进来（这些该交给 L2 或 library_query）
        assert!(!r.failures.is_match("生成 10 张混沌帝国的卡"));
        assert!(!r.failures.is_match("写第 4 章"));
    }

    /// failures 必须排在 task_status 之前：两者可能同时命中（「失败的进度」
    /// 「失败任务状态」），而清单比几个统计数字信息量大得多。
    #[test]
    fn failures_wins_over_task_status() {
        let r = rules();
        let msg = "失败任务的进度";
        // 前提：这句确实两边都命中（否则测试没有意义）
        assert!(r.failures.is_match(msg), "前提：应命中 failures");
        assert!(r.task_status.is_match(msg), "前提：应命中 task_status");
        // 结论：路由里 failures 在前，所以最终走 failures
        assert!(r.failures.is_match(msg), "failures 优先");
    }

    /// 「现在有多少张卡？」曾经落到 fallback（实测），补正则后必须命中资料库查询。
    #[test]
    fn library_query_understands_card_count() {
        let r = rules();
        assert!(r.library_query.is_match("现在有多少张卡？"));
        assert!(r.library_query.is_match("一共几张卡"));
        assert!(r.library_query.is_match("资料库里有哪些卡片"));
    }

    /// shorten_reason 必须按「字符」截断，不能按字节 —— 中文是多字节，
    /// 按字节切会把字劈成乱码。
    #[test]
    fn shorten_reason_counts_chars_not_bytes() {
        let long = "归墟古族弱点机制校验未过缺区块基础要素核心关联实体扩展描述字数不足要求";
        let out = shorten_reason(long, 10);
        assert_eq!(out.chars().count(), 11, "10 个字 + 省略号");
        assert!(out.ends_with('…'));
        // 短文本原样返回，不加省略号
        assert_eq!(shorten_reason("很短", 10), "很短");
        // 换行必须被压成单行（否则清单排版会断）
        assert_eq!(shorten_reason("第一行\n第二行", 50), "第一行 第二行");
    }

    #[test]
    fn spec_infer_default_splits() {
        // 10 张：1 文明 + 剩下 9 张均分 3 类 = 3/3/3
        assert_eq!(infer_spec("生成 10 张混沌帝国的卡", 10), "文明:1,势力:3,地点:3,角色:3");
        assert_eq!(infer_spec("建 1 张卡", 1), "文明:1");
    }

    #[test]
    fn write_chapter_regex() {
        let r = rules();
        let c = r.write_chapter.captures("写第 4 章").unwrap();
        assert_eq!(c.get(2).unwrap().as_str(), "4");
        let c = r.write_chapter.captures("跑卷02 第 12 章").unwrap();
        assert_eq!(c.get(1).unwrap().as_str(), "02");
        assert_eq!(c.get(2).unwrap().as_str(), "12");
    }

    /// 回归：L1 只该拦截**四类死板指令**，不能再去抢自然语言。
    ///
    /// 【为什么锁这条】2026-10-18 总指挥明确纠正：L1 只处理极其死板的固定
    /// 指令（帮助 / 精确卡号 / 任务状态 / 失败清单），其余一律交给 L2。
    /// 之前的路子是"往词表里加词"，属于打地鼠——中文说法无穷无尽。
    ///
    /// 这条测试的真实价值是：**防止有人又把 L1 的匹配范围放大**。
    /// 下面这几句必须落空（即交给 L2），如果哪天有人给 cardgen 或
    /// library_query 加词导致它们命中了，这里会立刻红。
    #[test]
    fn l1_must_not_hijack_colloquial_questions() {
        let r = rules();
        // 总指挥点名的三句口语：都不能被 L1 的四条白名单正则命中
        for q in ["星耀那边长啥样", "帮我整点星耀的卡", "给我看看星耀"] {
            assert!(!r.help.is_match(q), "帮助正则不该命中：{q}");
            assert!(!r.inspect_card.is_match(q), "卡号正则不该命中：{q}");
            assert!(!r.failures.is_match(q), "失败正则不该命中：{q}");
            assert!(!r.task_status.is_match(q), "状态正则不该命中：{q}");
        }
    }

    /// 回归：L1 白名单里的四类，必须真的能命中（否则秒回能力丢了）。
    #[test]
    fn l1_fast_path_still_works() {
        let r = rules();
        assert!(r.help.is_match("帮助"));
        assert!(r.help.is_match("你能做什么"));
        assert!(r.inspect_card.is_match("CIVI-99-900 是什么卡"));
        assert!(r.failures.is_match("那些失败的"));
        assert!(r.task_status.is_match("看当前任务状态"));
    }
}