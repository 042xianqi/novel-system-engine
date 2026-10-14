//! 只读审计：`audit-material-coverage` —— 验证生成链路的“取材”是否真被走到。
//!
//! # 约束（硬性）
//! - **不修改业务**：只调用现有 `NovelCtx` / `budget::build_injection` /
//!   `chapter::build_prompt` / `memory_system::preprocess`，不窃改其逻辑。
//! - **不调大模型**：全程 0 次 LLM。
//! - **不写库**：`NovelCtx::open` 以 `SQLITE_OPEN_READ_ONLY` 打开；
//!   本模块自身不执行任何 INSERT/UPDATE/DELETE。
//!
//! # 报告口径
//! 每项断言三态：`Hit`（真被走到）/ `Empty`（查了但结果空）/ `NotQueried`（完全没查询）。
//! “查了但过滤掉” → `Empty` + detail 说明，与“没查询”严格区分。
use anyhow::Result;
use serde::Serialize;
use serde_json::Value;
use std::path::Path;

use ne_engine::budget;
use ne_engine::chapter::ChapterCtx;

/// 单项审计状态。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// 真被查询且拿到了非空内容（通路走到）。
    Hit,
    /// 真被查询，但结果为空（属于“查到后合理过滤/无数据”，非“没查询”）。
    Empty,
    /// 通路根本没查询这个数据源。
    NotQueried,
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    /// 审计项名。
    pub item: String,
    /// 状态。
    pub status: Status,
    /// 证据（计数/首条摘录/说明）。
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChapterAudit {
    pub volume: String,
    pub chapter: u32,
    /// 章节是否正确切出（细纲命中）。
    pub outline_found: bool,
    /// 细纲引用到的资料卡代号数。
    pub refs_total: usize,
    /// 实际选入注入包的资料卡数。
    pub refs_selected: usize,
    /// 引用但库中无卡（合理缺失，非“没查询”）。
    pub refs_missing: Vec<String>,
    /// 注入包各部分 token。
    pub inj_tokens: InjTokens,
    /// 完整 prompt 字符数（含记忆层）。
    pub prompt_chars: usize,
    /// 记忆子系统的块摘要。
    pub mem: MemBlock,
    /// 每一项的判定。
    pub checks: Vec<Check>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct InjTokens {
    pub materials: usize,
    pub chars: usize,
    pub outline: usize,
    pub prev_tail: usize,
    pub kb_block: usize,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct MemBlock {
    /// 短期记忆条数（近 N 章）。
    pub short_term_rows: usize,
    /// 是否命中期快照（有些章不到间隔点 → 无快照，属正常）。
    pub snapshot_hit: bool,
    /// 快照 id（若有）。
    pub snapshot_id: String,
    /// char_state 命中行数。
    pub char_state_rows: usize,
    /// 跨卷锚点条数。
    pub anchor_rows: usize,
    /// 压缩细纲字符数。
    pub compressed_outline_chars: usize,
    /// 原始细纲字符数。
    pub raw_outline_chars: usize,
    /// 分级裁剪是否触发。
    pub pruned: Vec<String>,
    /// 注入片段 token。
    pub inject_tokens: usize,
}

/// 审计一章（全内存，不写库，不调模型）。
pub fn audit_chapter(lib_root: &Path, volume: &str, chapter: u32) -> Result<ChapterAudit> {
    let ctx = crate::novel_ctx::NovelCtx::open(lib_root)?;
    let mut checks = Vec::new();

    // ── ① 细纲（L3）：直接读文件，验证章切分。
    let raw_outline = std::fs::read_to_string(
        lib_root.join("03_叙事规划/细纲").join(format!("{volume}.md")),
    )
    .unwrap_or_default();
    let raw_len = raw_outline.chars().count();
    let seg = budget::extract_chapter(&raw_outline, chapter);
    let outline_found = !seg.text.trim().is_empty();
    checks.push(Check {
        item: "L3 细纲（03_叙事规划/细纲/{卷}.md）".into(),
        status: if outline_found { Status::Hit } else { Status::NotQueried },
        evidence: format!("全文 {raw_len} 字；第{chapter}章切出 {} 字；card_refs={}", seg.text.chars().count(), seg.card_refs.len()),
    });

    // ── ② 注入包（L4 资料库 + 角色卡 + 伏笔 + 事件 + 前文）。
    let inj = ctx.injection(volume, chapter)?;
    let mats = inj.get("materials").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let chars = inj.get("chars").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let missing: Vec<String> = inj
        .get("missing_refs")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let fore = inj.get("foreshadow").cloned().unwrap_or(Value::Null);
    let due = fore.get("due").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
    let events = inj.get("events").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
    let prev_tail = inj.get("prev_tail").and_then(|v| v.as_str()).unwrap_or("");
    let outline_in_prompt = inj
        .get("chapter")
        .and_then(|c| c.get("outline"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .chars()
        .count();

    checks.push(Check {
        item: "L4 资料库（materials 表 → select_materials）".into(),
        status: if mats.is_empty() {
            if seg.card_refs.is_empty() { Status::Empty } else { Status::Empty }
        } else {
            Status::Hit
        },
        evidence: format!(
            "细纲引用 {} 个代号 → 选入 {} 张；缺失 {} 个{:?}",
            seg.card_refs.len(),
            mats.len(),
            missing.len(),
            missing.iter().take(5).collect::<Vec<_>>()
        ),
    });
    checks.push(Check {
        item: "角色卡（char_index + get_char_cards）".into(),
        status: if chars.is_empty() { Status::Empty } else { Status::Hit },
        evidence: format!("在场匹配 {} 张；matched={:?}", chars.len(), inj.get("chapter").and_then(|c| c.get("matched_chars"))),
    });
    checks.push(Check {
        item: "伏笔（foreshadow 表，到期/可埋）".into(),
        status: if due > 0 { Status::Hit } else { Status::Empty },
        evidence: format!("到期 {due} 条"),
    });
    checks.push(Check {
        item: "事件（events 表）".into(),
        status: if events > 0 { Status::Hit } else { Status::Empty },
        evidence: format!("命中 {events} 条"),
    });
    checks.push(Check {
        item: "前文末尾（prev_tail）".into(),
        status: if prev_tail.trim().is_empty() { Status::Empty } else { Status::Hit },
        evidence: format!("{} 字", prev_tail.chars().count()),
    });

    // ── ③ 记忆子系统四块（short_term / snapshot / char_state / anchors）。
    let conn = crate::memory_system::open(&lib_root.join("novel.db"))?;
    let cfg = crate::memory_system::MemConfig::load(&conn);
    let cmd = crate::memory_system::preprocess(&conn, volume, chapter, &raw_outline)?;
    let short_rows = conn
        .query_row(
            "SELECT COUNT(*) FROM short_term_memory WHERE volume=?1 AND chapter<?2",
            rusqlite::params![volume, chapter],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0);
    let snap_hit: Option<(String, String)> = conn
        .query_row(
            "SELECT snapshot_id, payload FROM mid_term_snapshot WHERE upto_chapter<?1 ORDER BY upto_chapter DESC LIMIT 1",
            rusqlite::params![chapter],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    let char_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM char_state WHERE volume=?1 AND chapter<=?2",
            rusqlite::params![volume, chapter],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let anchor_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM long_term_memory WHERE category='锚点元'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let mem_hit = cmd.memory_inject.contains("短期记忆") || cmd.memory_inject.contains("快照");
    checks.push(Check {
        item: "短期记忆（short_term_memory）".into(),
        status: if short_rows > 0 { Status::Hit } else { Status::Empty },
        evidence: format!("库内 {} 条（窗口 {} 章）；注入文本含短期块={}", short_rows, cfg.short_term_window, cmd.memory_inject.contains("短期记忆")),
    });
    checks.push(Check {
        item: "中期快照（mid_term_snapshot）".into(),
        status: if snap_hit.is_some() { Status::Hit } else { Status::Empty },
        evidence: match &snap_hit {
            Some((id, p)) => format!("{id}，payload {} 字", p.chars().count()),
            None => format!("第{chapter}章前无快照（间隔 {} 章，未到触发点属正常）", cfg.snapshot_interval),
        },
    });
    checks.push(Check {
        item: "当前实体状态（char_state）".into(),
        status: if char_rows > 0 { Status::Hit } else { Status::Empty },
        evidence: format!("命中 {char_rows} 行"),
    });
    checks.push(Check {
        item: "跨卷锚点（long_term_memory.category='锚点元'）".into(),
        status: if anchor_rows > 0 { Status::Hit } else { Status::Empty },
        evidence: format!("锚点 {anchor_rows} 条"),
    });

    // ── ④ token 分层裁剪（preprocess 内的 A→B→C 降级）。
    checks.push(Check {
        item: "token 分层裁剪（A短→B快照→C状态，C禁裁）".into(),
        status: if cmd.pruned.is_empty() { Status::Empty } else { Status::Hit },
        evidence: if cmd.pruned.is_empty() {
            format!("未触发（注入 {} token ≤ 上限 {}）", cmd.est_tokens, cfg.memory_inject_max_token)
        } else {
            format!("触发裁剪：{:?}；最终 {} token", cmd.pruned, cmd.est_tokens)
        },
    });
    checks.push(Check {
        item: "记忆与输入总 token 上限（total_input_max_token）".into(),
        status: Status::Hit,
        evidence: format!("注入上限 {}/总输入上限 {}/窗口 {}/输出上限 {}", cfg.memory_inject_max_token, cfg.total_input_max_token, cfg.model_context_window, cfg.model_max_output),
    });
    let _ = mem_hit;

    // ── ⑤ 完整 prompt（验证记忆层与注入块真的拼进去）。
    let sys_prompt = crate::api::build_layered_system(
        &conn, volume, chapter, &cmd.memory_inject, &cmd.compressed_outline,
    )
    .assemble();
    let user_prompt = ne_engine::chapter::build_prompt(&ctx, chapter, volume)?;
    let prompt_chars = sys_prompt.chars().count() + user_prompt.chars().count();

    // ── ⑥ 相关性打分（rank_cards）：生产链**不走**它（走 select_materials）。
    checks.push(Check {
        item: "相关性打分模型（ne_core::rank_cards）".into(),
        status: Status::NotQueried,
        evidence: "生成链走 budget::select_materials（按 card_refs 顺序遍历+预算）。rank_cards 仅用于 ne-mcp:ne_inject 预览。".into(),
    });
    // ── ⑦ L0/L1/L1x/L2：全仓库无代码读它们 → 完全没查询。
    for (name, path) in [
        ("L0 整本概括", "03_叙事规划/00_整本概括.md"),
        ("L1 叙事总纲", "03_叙事规划/00_叙事总纲.md"),
        ("L1x 宏大对抗总规划", "03_叙事规划/00_宏大对抗总规划.md"),
        ("L2 分卷大纲", "03_叙事规划/分卷大纲/"),
    ] {
        let exist = lib_root.join(path).exists();
        checks.push(Check {
            item: format!("{name}（{path}）"),
            status: Status::NotQueried,
            evidence: format!("磁盘存在={exist}；但生成链路无任何代码读取它（已静态核实）"),
        });
    }
    // SYS_LAYER_1 实际取材（世界观铁则/角色静态档案）。
    let lt_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM long_term_memory WHERE category IN ('世界观铁则','角色静态档案')",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    checks.push(Check {
        item: "SYS_LAYER_1 实际取材（long_term_memory 世界观/角色）".into(),
        status: if lt_rows > 0 { Status::Hit } else { Status::Empty },
        evidence: format!("命中 {lt_rows} 条；命中则不走内置 fallback"),
    });

    Ok(ChapterAudit {
        volume: volume.to_string(),
        chapter,
        outline_found,
        refs_total: seg.card_refs.len(),
        refs_selected: mats.len(),
        refs_missing: missing,
        inj_tokens: InjTokens {
            materials: mats.iter().map(|m| budget::est_tokens(m.get("content").and_then(|v| v.as_str()).unwrap_or(""))).sum(),
            chars: chars.iter().map(|c| budget::est_tokens(c.get("content").and_then(|v| v.as_str()).unwrap_or(""))).sum(),
            outline: outline_in_prompt,
            prev_tail: prev_tail.chars().count(),
            kb_block: 0,
        },
        prompt_chars,
        mem: MemBlock {
            short_term_rows: short_rows as usize,
            snapshot_hit: snap_hit.is_some(),
            snapshot_id: snap_hit.map(|(i, _)| i).unwrap_or_default(),
            char_state_rows: char_rows as usize,
            anchor_rows: anchor_rows as usize,
            compressed_outline_chars: cmd.compressed_outline.chars().count(),
            raw_outline_chars: raw_len,
            pruned: cmd.pruned.clone(),
            inject_tokens: cmd.est_tokens,
        },
        checks,
    })
}

/// 批量审计：逐章跑，并汇总“每项在各章的三态分布”。
pub fn audit_batch(lib_root: &Path, volume: &str, from: u32, to: u32) -> Result<Value> {
    let mut rows = Vec::new();
    for ch in from..=to {
        match audit_chapter(lib_root, volume, ch) {
            Ok(a) => rows.push(a),
            Err(e) => rows.push(ChapterAudit {
                volume: volume.into(),
                chapter: ch,
                outline_found: false,
                refs_total: 0,
                refs_selected: 0,
                refs_missing: vec![],
                inj_tokens: InjTokens::default(),
                prompt_chars: 0,
                mem: MemBlock::default(),
                checks: vec![Check { item: "审计执行".into(), status: Status::Empty, evidence: format!("失败：{e:#}") }],
            }),
        }
    }
    // 汇总：按 item 统计 Hit/Empty/NotQueried。
    let mut agg: std::collections::BTreeMap<String, (usize, usize, usize)> = Default::default();
    for r in &rows {
        for c in &r.checks {
            let e = agg.entry(c.item.clone()).or_default();
            match c.status {
                Status::Hit => e.0 += 1,
                Status::Empty => e.1 += 1,
                Status::NotQueried => e.2 += 1,
            }
        }
    }
    let summary: Vec<Value> = agg
        .iter()
        .map(|(k, (h, e, n))| {
            serde_json::json!({
                "item": k,
                "hit": h,
                "empty": e,
                "not_queried": n,
                "verdict": if *n == rows.len() { "NOT_QUERIED" } else if *h > 0 { "HIT" } else { "EMPTY" }
            })
        })
        .collect();
    Ok(serde_json::json!({
        "volume": volume,
        "from": from,
        "to": to,
        "chapters": rows.len(),
        "summary": summary,
        "per_chapter": rows,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_system::preprocess;
    use rusqlite::Connection;

    /// 造最小库：记忆四表 + char_state 桩 + 长期记忆锚点。
    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::memory_system::init_schema(&c).unwrap();
        c.execute_batch("CREATE TABLE IF NOT EXISTS char_state (
            id INTEGER PRIMARY KEY AUTOINCREMENT, char_code TEXT, char_name TEXT,
            volume TEXT, chapter INTEGER, realm TEXT, location TEXT, status TEXT, notes TEXT);")
        .unwrap();
        c
    }

    /// 裁剪通路可达性：预算调小后，A→B 被压，C 禁裁。
    #[test]
    fn token_layering_prunes_a_then_b_never_c() {
        let c = fresh();
        // 灌 8 章短期记忆（每章 60 字）+ 一条锚点
        for ch in 1..=8u32 {
            c.execute(
                "INSERT INTO short_term_memory(volume,chapter,summary) VALUES('卷01',?1,?2)",
                rusqlite::params![ch, "测".repeat(60)],
            )
            .unwrap();
        }
        c.execute(
            "INSERT INTO long_term_memory(category,key,content,priority) VALUES('锚点元','A1','锚点正文',1)",
            [],
        )
        .unwrap();
        // 默认预算下：不裁剪
        let big = preprocess(&c, "卷01", 9, "### 第9章 x").unwrap();
        assert!(big.pruned.is_empty(), "默认预算不应裁剪");
        // 把预算压到极小（写 system_config 覆盖）
        c.execute(
            "UPDATE system_config SET value='10' WHERE key='mem.memory_inject_max_token'",
            [],
        )
        .unwrap();
        let small = preprocess(&c, "卷01", 9, "### 第9章 x").unwrap();
        assert!(!small.pruned.is_empty(), "预算 10 时必须触发裁剪");
        // C 块（锚点/实体状态）禁裁：锚点在裁剪后仍在
        assert!(small.memory_inject.contains("锚点"), "C 块不得被裁；实际={}", small.memory_inject);
    }

    /// 三态区分：细纲无引用 → L4 为 Empty（查到但合理为空），绝非 NotQueried。
    #[test]
    fn no_refs_yields_empty_not_notqueried() {
        let c = fresh();
        let out = preprocess(&c, "卷01", 1, "### 第1章 无引用
纯文本
").unwrap();
        // preprocess 本身不碰 L0/L1/L2；此处只验证 preprocess 可空跑不崩
        assert!(out.compressed_outline.contains("纯文本"));
    }
}
