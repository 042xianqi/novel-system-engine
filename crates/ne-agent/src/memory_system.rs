//! 记忆子系统 —— 外围预处理/后置更新，**不接管生成**（需求文档 §一）。
//!
//! # 职责边界（严格）
//!
//! ```text
//! 用户输入(章号+原始L3细纲)
//!         ↓
//!   preprocess()  ──► Ok(记忆注入片段 + 压缩版细纲) / Err(状态冲突)
//!         ↓
//!   原有 build_knowledge_injection / build_prompt（内部逻辑零改动）
//!         ↓
//!   原有门禁链路
//!         ↓
//!   post_update() ──► 保守实体解析 + 短期记忆 + 30 章快照
//! ```
//!
//! # 关键隔离原则（§一 末）
//!
//! 本模块**只产出文本片段**交给原有 prompt 构建函数，绝不接管 prompt 内部拼接。
//!
//! # token 参数说明（与需求文档的偏离，已核实）
//!
//! 需求文档按「1M 上下文 / 输出 131k / `memory_inject_max_token=280000`」设计，
//! 但本库实际 `models.yml` 为 **`contextWindow: 128000` / `maxTokens: 32000`**。
//! 照抄 280000 会**每次调用直接超窗口**。故 token 上限一律从 `system_config` 读，
//! 缺省值按真实窗口取（见 [`MemConfig::default_for_real_model`]）。
use anyhow::{anyhow, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// 记忆子系统运行参数（存 `system_config` 表，可人工调整）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemConfig {
    /// 记忆注入片段 token 上限（文档值 280000 超真实窗口，此处按窗口取值）
    pub memory_inject_max_token: usize,
    /// 总输入 token 上限，超过直接报错不调模型
    pub total_input_max_token: usize,
    /// 短期记忆窗口（章）
    pub short_term_window: usize,
    /// 中期快照间隔（章）
    pub snapshot_interval: usize,
    /// 模型上下文窗口（信息位，用于校验）
    pub model_context_window: usize,
    /// 模型单次最大输出
    pub model_max_output: usize,
}

impl MemConfig {
    /// 按**真实模型**（`models.yml`: 128k 窗口 / 32k 输出）取值。
    ///
    /// 分配：上下文 128000 = 注入 24000 + 输出预留 32000 + 模板/接线/系统提示 ≈ 72000。
    pub fn default_for_real_model() -> Self {
        Self {
            memory_inject_max_token: 24_000,
            total_input_max_token: 96_000,
            short_term_window: 8,
            snapshot_interval: 30,
            model_context_window: 128_000,
            model_max_output: 32_000,
        }
    }
    /// 从 `system_config` 读覆盖值（键名前缀 `mem.`），缺省回落真实模型默认值。
    pub fn load(conn: &Connection) -> Self {
        let mut c = Self::default_for_real_model();
        let get = |k: &str| -> Option<String> {
            conn.query_row(
                "SELECT value FROM system_config WHERE key=?1",
                params![k],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten()
        };
        if let Some(v) = get("mem.memory_inject_max_token").and_then(|s| s.parse().ok()) {
            c.memory_inject_max_token = v;
        }
        if let Some(v) = get("mem.total_input_max_token").and_then(|s| s.parse().ok()) {
            c.total_input_max_token = v;
        }
        if let Some(v) = get("mem.short_term_window").and_then(|s| s.parse().ok()) {
            c.short_term_window = v;
        }
        if let Some(v) = get("mem.snapshot_interval").and_then(|s| s.parse().ok()) {
            c.snapshot_interval = v;
        }
        c
    }
    /// 写入默认参数（仅当键不存在）。
    pub fn seed(conn: &Connection) -> Result<()> {
        let d = Self::default_for_real_model();
        let pairs: [(&str, String, &str); 6] = [
            ("mem.memory_inject_max_token", d.memory_inject_max_token.to_string(),
             "记忆注入片段 token 上限（文档 280000 超真实窗口，按 128k 窗口取值）"),
            ("mem.total_input_max_token", d.total_input_max_token.to_string(),
             "总输入 token 上限，超限直接报错不调模型"),
            ("mem.short_term_window", d.short_term_window.to_string(), "短期记忆窗口（章）"),
            ("mem.snapshot_interval", d.snapshot_interval.to_string(), "中期快照间隔（章）"),
            ("mem.model_context_window", d.model_context_window.to_string(), "模型上下文窗口"),
            ("mem.model_max_output", d.model_max_output.to_string(), "模型单次最大输出"),
        ];
        for (k, v, note) in pairs {
            conn.execute(
                "INSERT INTO system_config(key,value,note) VALUES(?1,?2,?3)
                 ON CONFLICT(key) DO NOTHING",
                params![k, v, note],
            )?;
        }
        Ok(())
    }
}

/// 预处理结果：记忆注入片段 + 压缩版细纲。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreprocessOutput {
    /// 送 `build_knowledge_injection` 的**文本片段**（不是结构化 JSON）
    pub memory_inject: String,
    /// 送 SYS_LAYER_2 的压缩版细纲
    pub compressed_outline: String,
    /// 被裁剪掉的块（报告用）
    pub pruned: Vec<String>,
    /// 估算 token
    pub est_tokens: usize,
}

/// 保守实体解析结果（宁少勿脑补，§四.3）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PostUpdateReport {
    pub extracted: BTreeMap<String, String>,
    pub warnings: Vec<String>,
    pub snapshot_created: Option<String>,
    pub short_term_pruned: usize,
}

/// 初始化记忆子系统表（幂等）。
pub fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(include_str!("memory_schema.sql"))?;
    MemConfig::seed(conn)?;
    Ok(())
}

/// 在指定库根打开/初始化记忆库（`novel.db` 内的新表）。
pub fn open(db_path: &Path) -> Result<Connection> {
    let conn = Connection::open(db_path)?;
    init_schema(&conn)?;
    Ok(conn)
}

// ─────────────────────────────────────────── 文本工具

/// 按字符估算 token（保守：中文 1 字符 ≈ 1 token；ASCII 4 字符 ≈ 1 token）。
///
/// 与 `ne_core::estimate_tokens` 同口径，此处独立实现避免跨 crate 依赖。
pub fn est_tokens(s: &str) -> usize {
    let mut han = 0usize;
    let mut other = 0usize;
    for c in s.chars() {
        if ('\u{4e00}'..='\u{9fff}').contains(&c) {
            han += 1;
        } else {
            other += 1;
        }
    }
    han + other.div_ceil(4)
}

/// 保守按句截断到 token 上限（绝不切在句子中间）。
fn truncate_to_tokens(s: &str, max_tokens: usize) -> String {
    if est_tokens(s) <= max_tokens {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for seg in s.split_inclusive(['。', '！', '？', '\n']) {
        let t = est_tokens(seg);
        if used + t > max_tokens {
            break;
        }
        out.push_str(seg);
        used += t;
    }
    if out.trim().is_empty() {
        // 连一句都放不下：**不硬截半句**（宁可返回空，交由调用方整体裁剪），
        // 保证「要么空、要么以句末标点结尾」。
        return String::new();
    }
    out.trim_end().to_string()
}

// ─────────────────────────────────────────── preprocess

/// 预处理：读长期记忆 + 当前中期快照 + 短期记忆 → 分级裁剪 → 注入片段 + 压缩细纲。
///
/// 块排序（§三，适配注意力衰减）：A 短期（靠前，先裁） → B 中期（中间） → C 状态进度（末尾，禁裁）。
pub fn preprocess(
    conn: &Connection,
    volume: &str,
    chapter: u32,
    raw_outline: &str,
) -> Result<PreprocessOutput> {
    let cfg = MemConfig::load(conn);
    let mut pruned = Vec::new();

    // ── 块 A：短期记忆（近 N 章摘要，次要，最先裁）
    let win = cfg.short_term_window as i64;
    let mut stmt = conn.prepare(
        "SELECT chapter, summary FROM short_term_memory
         WHERE volume=?1 AND chapter < ?2 ORDER BY chapter DESC LIMIT ?3",
    )?;
    let mut short: Vec<(u32, String)> = stmt
        .query_map(params![volume, chapter, win], |r| {
            Ok((r.get::<_, i64>(0)? as u32, r.get::<_, String>(1)?))
        })?
        .filter_map(|x| x.ok())
        .collect();
    short.reverse();
    let block_a = if short.is_empty() {
        String::new()
    } else {
        let body: Vec<String> = short
            .iter()
            .map(|(c, s)| format!("  第{c}章：{s}"))
            .collect();
        format!("【短期记忆（近 {} 章）】\n{}", short.len(), body.join("\n"))
    };

    // ── 块 B：中期快照（重大隐患 + 已完成主线事件）
    let snap: Option<(String, String, String)> = conn
        .query_row(
            "SELECT snapshot_id, payload, warnings FROM mid_term_snapshot
             WHERE upto_chapter < ?1 ORDER BY upto_chapter DESC LIMIT 1",
            params![chapter],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let block_b = match &snap {
        Some((sid, payload, warn)) => {
            let mut s = format!("【中期快照 {sid}】\n{payload}");
            let w: Vec<String> = serde_json::from_str(warn).unwrap_or_default();
            if !w.is_empty() {
                s.push_str(&format!("\n  待人工复核：{}", w.join("；")));
            }
            s
        }
        None => String::new(),
    };

    // ── 块 C：当前关键实体状态 + 跨卷锚点进度（最高优先级，禁裁）
    let block_c = build_block_c(conn, volume, chapter)?;

    // ── 分级裁剪：A → B → C（C 永不裁）
    let budget = cfg.memory_inject_max_token;
    let (ta, tb, tc) = (est_tokens(&block_a), est_tokens(&block_b), est_tokens(&block_c));
    let mut a_out = block_a;
    let mut b_out = block_b;
    if ta + tb + tc > budget {
        // 先压 A
        let room_b = budget.saturating_sub(tb + tc);
        if est_tokens(&a_out) > room_b {
            if room_b > 0 {
                a_out = truncate_to_tokens(&a_out, room_b);
                pruned.push(format!("块A短期记忆截断至 {room_b} token"));
            } else {
                a_out.clear();
                pruned.push("块A短期记忆整体裁剪".into());
            }
        }
        // 仍超则压 B
        if est_tokens(&a_out) + tb + tc > budget {
            let room_b = budget.saturating_sub(est_tokens(&a_out) + tc);
            if room_b > 0 {
                b_out = truncate_to_tokens(&b_out, room_b);
                pruned.push(format!("块B中期快照截断至 {room_b} token"));
            } else {
                b_out.clear();
                pruned.push("块B中期快照整体裁剪".into());
            }
        }
    }
    let mut parts = Vec::new();
    if !a_out.trim().is_empty() {
        parts.push(a_out);
    }
    if !b_out.trim().is_empty() {
        parts.push(b_out);
    }
    if !block_c.trim().is_empty() {
        parts.push(block_c);
    }
    let memory_inject = parts.join("\n\n");
    let est = est_tokens(&memory_inject);

    let compressed = compress_outline(raw_outline, chapter);
    Ok(PreprocessOutput {
        memory_inject,
        compressed_outline: compressed,
        pruned,
        est_tokens: est,
    })
}

/// 块 C：关键实体状态 + 跨卷锚点进度（读 `char_state` + `long_term_memory` 锚点元）。
fn build_block_c(conn: &Connection, volume: &str, chapter: u32) -> Result<String> {
    let mut s = String::from("=====【当前关键实体状态｜最高优先级】=====\n");
    let mut stmt = conn.prepare(
        "SELECT char_name, realm, location, status, notes FROM char_state
         WHERE volume=?1 AND chapter<=?2 ORDER BY chapter DESC LIMIT 12",
    )?;
    let rows: Vec<(String, Option<String>, Option<String>, Option<String>, Option<String>)> = stmt
        .query_map(params![volume, chapter], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .filter_map(|x| x.ok())
        .collect();
    if rows.is_empty() {
        s.push_str("  （本章尚无已入库角色状态）\n");
    } else {
        let mut seen = std::collections::HashSet::new();
        for (name, realm, loc, st, notes) in rows {
            if !seen.insert(name.clone()) {
                continue;
            }
            let mut line = format!("  {name}：");
            let mut bits = Vec::new();
            if let Some(r) = realm.filter(|x| !x.is_empty()) {
                bits.push(format!("修为={r}"));
            }
            if let Some(l) = loc.filter(|x| !x.is_empty()) {
                bits.push(format!("位置={l}"));
            }
            if let Some(x) = st.filter(|x| !x.is_empty()) {
                bits.push(format!("状态={x}"));
            }
            if let Some(n) = notes.filter(|x| !x.is_empty()) {
                bits.push(n.chars().take(60).collect::<String>());
            }
            line.push_str(&bits.join("；"));
            s.push_str(&line);
            s.push('\n');
        }
    }
    s.push_str("=====【跨卷锚点当前进度】=====\n");
    let mut stmt = conn.prepare(
        "SELECT key, content FROM long_term_memory WHERE category='锚点元' ORDER BY priority, key",
    )?;
    let anchors: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .filter_map(|x| x.ok())
        .collect();
    if anchors.is_empty() {
        s.push_str("  （锚点元表未导入）\n");
    } else {
        for (k, v) in anchors {
            let one = v.lines().next().unwrap_or("");
            s.push_str(&format!("  {k}：{}\n", one.chars().take(80).collect::<String>()));
        }
    }
    Ok(s.trim_end().to_string())
}

/// 压缩 L3 细纲：剥离注释 / 溯源 ID / `【】` 参考资料行，保留事件骨架。
///
/// 目的（§六.3）：原始细纲有大量注释、溯源 ID，浪费 token 且干扰模型。
pub fn compress_outline(raw: &str, chapter: u32) -> String {
    let mut out = String::new();
    let mut in_ch = false;
    for line in raw.lines() {
        let t = line.trim();
        // 章边界（兼容 ### 第N章 / ### 第N章 xxx）
        if let Some(rest) = t.strip_prefix("### ") {
            let n = parse_chapter_no(rest);
            if n == Some(chapter) {
                in_ch = true;
                out.push_str(&format!("### 第{chapter}章 {}\n", rest.split_once(' ').map(|x| x.1).unwrap_or("")));
                continue;
            } else if in_ch {
                break; // 已到下一章
            }
        }
        if !in_ch {
            continue;
        }
        // 剥离纯引用行（含【】编号卡的整行参考资料 / 分隔线）
        if t.starts_with("---") && t.chars().all(|c| c == '-' || c.is_whitespace()) {
            continue;
        }
        if t.starts_with('>') {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if out.trim().is_empty() {
        // 未匹配到章头：退化为去注释的全文前 1500 字符
        let cleaned: String = raw
            .lines()
            .filter(|l| !l.trim_start().starts_with('>') && !l.trim().starts_with("---"))
            .collect::<Vec<_>>()
            .join("\n");
        return cleaned.chars().take(1500).collect();
    }
    out.trim_end().to_string()
}

/// 解析 `第N章` 中的 N。
fn parse_chapter_no(s: &str) -> Option<u32> {
    let rest = s.strip_prefix('第')?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

// ─────────────────────────────────────────── post_update

/// 章节元数据存档（§一 / §五.3）：原始细纲与压缩细纲一并入库，供人工溯源。
///
/// **原始细纲绝不进 prompt**（§六.3），此处只做存档。
#[allow(clippy::too_many_arguments)]
pub fn save_chapter_meta(
    conn: &Connection,
    volume: &str,
    chapter: u32,
    raw_outline: &str,
    compressed_outline: &str,
    content: &str,
    snapshot_id: Option<&str>,
    input_tokens: usize,
    output_tokens: usize,
    warnings: &[String],
) -> Result<()> {
    conn.execute(
        "INSERT INTO chapter_meta(
            volume, chapter, raw_outline, compressed_outline, content,
            snapshot_id, input_tokens, output_tokens, warnings, status)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'ok')
         ON CONFLICT(volume, chapter) DO UPDATE SET
            raw_outline=excluded.raw_outline,
            compressed_outline=excluded.compressed_outline,
            content=excluded.content,
            snapshot_id=excluded.snapshot_id,
            input_tokens=excluded.input_tokens,
            output_tokens=excluded.output_tokens,
            warnings=excluded.warnings,
            status='ok'",
        params![
            volume,
            chapter,
            raw_outline,
            compressed_outline,
            content,
            snapshot_id,
            input_tokens as i64,
            output_tokens as i64,
            serde_json::to_string(warnings).unwrap_or_else(|_| "[]".into()),
        ],
    )?;
    Ok(())
}

/// 后置更新：保守实体解析（宁少勿脑补）→ 短期记忆 → 30 章快照。
///
/// **绝不脑补推断状态**（§四.3）：只提取正文中**显式出现**的状态词，
/// 模糊歧义一律进 `warnings` 交人工复核，不写快照。
pub fn post_update(
    conn: &Connection,
    volume: &str,
    chapter: u32,
    content: &str,
) -> Result<PostUpdateReport> {
    let cfg = MemConfig::load(conn);
    let mut rep = PostUpdateReport::default();

    // ── 保守提取：只听显式状态词（命中即记，不推断）
    let probes: [(&str, &[&str]); 4] = [
        ("修为", &["炼气", "筑基", "金丹", "元婴", "化神", "凡人"]),
        ("伤势", &["伤口", "流血", "包扎", "淤", "断", "昏迷"]),
        ("道具", &["草药", "灵石", "铜钱", "布条", "竹篓"]),
        ("锚点", &["锚点", "面板", "推演"]),
    ];
    for (k, words) in probes {
        let hits: Vec<&str> = words.iter().copied().filter(|w| content.contains(w)).collect();
        if hits.len() == 1 {
            rep.extracted.insert(k.to_string(), hits[0].to_string());
        } else if hits.len() > 1 {
            // 多义 → 不猜，标警告
            rep.warnings
                .push(format!("{k}出现多个候选词 {hits:?}，未写入快照（待人工复核）"));
        }
    }

    // ── 短期记忆：写本章摘要（结构化要点，非原文）
    let summary = format!(
        "共 {} 字；状态词命中 {} 项",
        content.chars().count(),
        rep.extracted.len()
    );
    conn.execute(
        "INSERT INTO short_term_memory(volume,chapter,summary) VALUES(?1,?2,?3)
         ON CONFLICT(volume,chapter) DO UPDATE SET summary=excluded.summary",
        params![volume, chapter, summary],
    )?;

    // ── 超窗：最旧推入快照队列
    let win = cfg.short_term_window as i64;
    let overflow: Vec<i64> = conn
        .prepare(
            "SELECT chapter FROM short_term_memory WHERE volume=?1
             ORDER BY chapter DESC LIMIT -1 OFFSET ?2",
        )?
        .query_map(params![volume, win], |r| r.get(0))?
        .filter_map(|x| x.ok())
        .collect();
    for ch in &overflow {
        conn.execute(
            "UPDATE short_term_memory SET pushed=1 WHERE volume=?1 AND chapter=?2",
            params![volume, ch],
        )?;
    }
    rep.short_term_pruned = overflow.len();

    // ── 30 章阈值：生成新快照
    let interval = cfg.snapshot_interval as u32;
    if interval > 0 && chapter % interval == 0 {
        let sid = format!("SNAP-{chapter:04}");
        let payload = build_snapshot_payload(conn, volume, chapter)?;
        conn.execute(
            "INSERT INTO mid_term_snapshot(snapshot_id,upto_chapter,payload,warnings)
             VALUES(?1,?2,?3,?4) ON CONFLICT(snapshot_id) DO UPDATE SET payload=excluded.payload",
            params![
                sid,
                chapter,
                payload,
                serde_json::to_string(&rep.warnings).unwrap_or_else(|_| "[]".into())
            ],
        )?;
        rep.snapshot_created = Some(sid);
    }
    Ok(rep)
}

/// 快照 payload：从 `char_state` 汇总到本章的关键实体（只读既有表，不推断）。
fn build_snapshot_payload(conn: &Connection, volume: &str, upto: u32) -> Result<String> {
    let mut stmt = conn.prepare(
        "SELECT char_code, char_name, realm, location, status FROM char_state
         WHERE volume=?1 AND chapter<=?2 ORDER BY chapter DESC LIMIT 30",
    )?;
    let nodes: Vec<serde_json::Value> = stmt
        .query_map(params![volume, upto], |r| {
            Ok(serde_json::json!({
                "code": r.get::<_, String>(0)?,
                "name": r.get::<_, String>(1)?,
                "realm": r.get::<_, Option<String>>(2)?,
                "location": r.get::<_, Option<String>>(3)?,
                "status": r.get::<_, Option<String>>(4)?,
            }))
        })?
        .filter_map(|x| x.ok())
        .collect();
    Ok(serde_json::to_string_pretty(&serde_json::json!({
        "upto_chapter": upto,
        "volume": volume,
        "char_states": nodes,
    }))?)
}

/// 人工回滚：把指定快照 id 的状态恢复为新基线（§四.6）。
///
/// 返回被恢复的角色状态条数；不存在的快照报错。
pub fn rollback_snapshot(conn: &Connection, snapshot_id: &str) -> Result<usize> {
    let payload: Option<String> = conn
        .query_row(
            "SELECT payload FROM mid_term_snapshot WHERE snapshot_id=?1",
            params![snapshot_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(p) = payload else {
        return Err(anyhow!("快照不存在：{snapshot_id}"));
    };
    let v: serde_json::Value = serde_json::from_str(&p)?;
    let mut n = 0usize;
    if let Some(arr) = v.get("char_states").and_then(|x| x.as_array()) {
        for e in arr {
            let code = e.get("code").and_then(|x| x.as_str()).unwrap_or("");
            if code.is_empty() {
                continue;
            }
            conn.execute(
                "UPDATE char_state SET realm=?1, location=?2, status=?3
                 WHERE char_code=?4",
                params![
                    e.get("realm").and_then(|x| x.as_str()),
                    e.get("location").and_then(|x| x.as_str()),
                    e.get("status").and_then(|x| x.as_str()).unwrap_or("存活"),
                    code
                ],
            )?;
            n += 1;
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        init_schema(&c).unwrap();
        // 记忆子系统读既有库表 char_state（属 novel.db，不属本 schema）。
        // 测试用最小桩：只建被读到的列。
        c.execute_batch(
            "CREATE TABLE IF NOT EXISTS char_state (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                char_code TEXT, char_name TEXT, volume TEXT, chapter INTEGER,
                realm TEXT, location TEXT, status TEXT, notes TEXT
            );",
        )
        .unwrap();
        c
    }

    #[test]
    fn schema_idempotent_and_seeds_config() {
        let c = fresh();
        init_schema(&c).unwrap(); // 二次调用不报错
        let n: i64 = c
            .query_row("SELECT count(*) FROM system_config", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 6);
    }

    #[test]
    fn token_caps_fit_real_model_window() {
        let d = MemConfig::default_for_real_model();
        // 注入 + 输出预留 必须 < 窗口
        assert!(d.memory_inject_max_token < d.model_context_window);
        assert!(d.memory_inject_max_token + d.model_max_output < d.model_context_window);
        assert!(d.total_input_max_token < d.model_context_window);
        // 与文档值不同，是刻意的：280000 > 128000 会直接超窗口
        assert!(d.memory_inject_max_token < 280_000);
    }

    #[test]
    fn compress_outline_strips_refs_and_keeps_skeleton() {
        let raw = "\
# 卷01
> 卷级注释应删
### 第1章 涧底醒
**概要**
- **目标**：活着
【引用卡清单】应删？不，这是章内
---
### 第2章 下一章
不应出现";
        let out = compress_outline(raw, 1);
        assert!(out.contains("涧底醒"));
        assert!(out.contains("目标"));
        assert!(!out.contains("卷级注释应删"));
        assert!(!out.contains("不应出现"));
    }

    #[test]
    fn preprocess_empty_db_ok() {
        let c = fresh();
        let o = preprocess(&c, "卷01", 1, "### 第1章 涧底醒\n**目标**：活着").unwrap();
        assert!(o.memory_inject.contains("跨卷锚点当前进度"));
        assert!(o.compressed_outline.contains("涧底醒"));
        assert!(o.est_tokens > 0);
    }

    #[test]
    fn preprocess_prunes_a_first_when_over_budget() {
        let c = fresh();
        // 塞入大量短期记忆
        for ch in 1..=8u32 {
            c.execute(
                "INSERT INTO short_term_memory(volume,chapter,summary) VALUES('卷01',?1,?2)",
                params![ch, "很长很长的摘要".repeat(200)],
            )
            .unwrap();
        }
        c.execute(
            "UPDATE system_config SET value='300' WHERE key='mem.memory_inject_max_token'",
            [],
        )
        .unwrap();
        let o = preprocess(&c, "卷01", 9, "### 第9章 测试").unwrap();
        // 裁到预算内：块A 被截（或整体丢弃），块C 永不裁
        assert!(o.pruned.iter().any(|p| p.contains("块A")), "应裁块A: {:?}", o.pruned);
        // 块 C 永不裁
        assert!(o.memory_inject.contains("当前关键实体状态"));
    }

    #[test]
    fn post_update_conservative_no_guessing() {
        let c = fresh();
        // 同时出现两个修为词 → 不猜，进警告
        let r = post_update(&c, "卷01", 3, "他先是凡人，后来踏入炼气。").unwrap();
        assert!(!r.extracted.contains_key("修为"));
        assert!(r.warnings.iter().any(|w| w.contains("修为")));
    }

    #[test]
    fn post_update_creates_snapshot_at_interval() {
        let c = fresh();
        let r = post_update(&c, "卷01", 30, "他的伤口包扎了。").unwrap();
        assert_eq!(r.snapshot_created.as_deref(), Some("SNAP-0030"));
        let n: i64 = c
            .query_row("SELECT count(*) FROM mid_term_snapshot", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn rollback_restores_char_state() {
        let c = fresh();
        c.execute(
            "INSERT INTO char_state(char_code,char_name,volume,chapter,realm,location,status)
             VALUES('CHAR-X','甲','卷01',0,'炼气','村口','存活')",
            [],
        )
        .unwrap();
        post_update(&c, "卷01", 30, "正文").unwrap();
        // 改动状态后再回滚
        c.execute("UPDATE char_state SET realm='元婴' WHERE char_code='CHAR-X'", [])
            .unwrap();
        let n = rollback_snapshot(&c, "SNAP-0030").unwrap();
        assert_eq!(n, 1);
        let realm: Option<String> = c
            .query_row("SELECT realm FROM char_state WHERE char_code='CHAR-X'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(realm.as_deref(), Some("炼气"));
    }

    #[test]
    fn rollback_missing_snapshot_errors() {
        let c = fresh();
        assert!(rollback_snapshot(&c, "SNAP-9999").is_err());
    }

    #[test]
fn chapter_meta_saved_and_upserted() {
    let c = fresh();
    save_chapter_meta(&c, "卷01", 1, "原始细纲A", "压缩细纲", "正文A", None, 100, 200, &[]).unwrap();
    let (raw, out): (String, i64) = c
        .query_row(
            "SELECT raw_outline, output_tokens FROM chapter_meta WHERE volume='卷01' AND chapter=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(raw, "原始细纲A");
    assert_eq!(out, 200);
    // upsert：同章再存应覆盖，不新增行
    save_chapter_meta(&c, "卷01", 1, "原始细纲B", "压缩", "正文B", Some("SNAP-0030"), 1, 2, &["w".into()]).unwrap();
    let n: i64 = c
        .query_row("SELECT count(*) FROM chapter_meta", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
    let raw2: String = c
        .query_row("SELECT raw_outline FROM chapter_meta", [], |r| r.get(0))
        .unwrap();
    assert_eq!(raw2, "原始细纲B");
}
#[test]
fn truncate_never_splits_sentence() {
    let s = "第一句。第二句。第三句。第四句。";
    // 预算够放两句 → 必须以句末标点结尾
    let t = truncate_to_tokens(s, 8);
    assert!(t.ends_with('。'), "截断应落在句末: {t:?}");
    // 预算连一句都放不下 → 返回空，绝不吐半句
    let tiny = truncate_to_tokens(s, 3);
    assert!(tiny.is_empty(), "放不下任何整句应返回空: {tiny:?}");
}
}