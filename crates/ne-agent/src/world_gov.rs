//! world-gov —— 世界观认知治理层（独立外挂，**默认只读 / 默认关**）。
//!
//! # 与 library-ctl 的严格分工
//! - `library-ctl` = **档案保管员**：管文件、结构、索引、数字数；**不做任何语义判断**。
//! - `world-gov`   = **世界观法务官**：带着整套层级法理去理解内容、做语义级审计；
//!   只出**证据 + 建议**，不独裁、不自改、不删任何东西。
//!
//! # 法理契约（硬编码，全模块统一引用）
//! ```text
//! L0（宇宙顶层总纲）
//!   ≫ L0.5（世界脊梁 / 核心基调）
//!   ≫ L1（全系列主线）
//!   ≫ L1x（跨卷长线 / 暗线）
//!   ≫ ANCHOR（跨卷锚点：跨卷不可破约束）
//!   ≫ GUARD（护栏：禁止事项、边界红线）
//!   ≫ L3（单卷细纲、单卷目标）
//!   ≫ L4（资料卡：角色/势力/物品/规则等实例素材）
//! ```
//! - 上层 = 法理来源；下层可细化、实例化，**不能反向否定上层**。
//! - 每张 L4 卡必须可回溯：它遵从哪一条上层约束。
//! - “干净” = 逻辑自洽、层级不逆、引用有理、定义无自噬（≠文件名不乱）。
//!
//! # 铁则
//! - **绝不侵入**主生成 / LTM 注入 / 短期记忆 / 快照 / prompt 组装链路。
//! - 语义类能力一律**显式开关**（`--semantic`），默认零模型：只产**探针包 probe_pack**。
//! - `--dry-run` 沙盘：不写库、不落盘内容。
//! - 拿不准统一标 `UNCERTAIN`，绝不幻觉式乱报警告；判冲突必须引用基线原话。
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;

use crate::governance::{
    code_prefix, extract_codes, extract_title, layer_priority, ltm_category_to_layer,
};

// ────────────────────────────────── 法理契约

/// 法理链（从上到下，上层约束下层）。
pub const LAW_CHAIN: &[(&str, &str)] = &[
    ("L0", "宇宙顶层总纲"),
    ("L0.5", "世界脊梁 / 核心基调"),
    ("L1", "全系列主线"),
    ("L1x", "跨卷长线 / 暗线"),
    ("ANCHOR", "跨卷锚点：跨卷不可破约束"),
    ("GUARD", "护栏：禁止事项、边界红线"),
    ("L3", "单卷细纲、单卷目标"),
    ("L4", "资料卡：角色/势力/物品/规则等实例素材"),
];

/// 上层法理层（可作为“来源”的层；L3/L4 是被约束方，不作来源）。
pub fn is_upper_layer(layer: &str) -> bool {
    matches!(layer, "L0" | "L0.5" | "L1" | "L1x" | "ANCHOR" | "GUARD")
}

fn layer_name(layer: &str) -> &'static str {
    LAW_CHAIN.iter().find(|(l, _)| *l == layer).map(|(_, n)| *n).unwrap_or("")
}

// ────────────────────────────────── 基线模型

#[derive(Debug, Clone, Serialize)]
pub struct BaselineEntry {
    pub layer: String,
    pub category: String,
    pub key: String,
    pub chars: usize,
    /// 摘要（截断后文本，**非完整原文**）
    pub summary: String,
    /// 边界 / 禁止域线索（粗粒度提示，非判定）
    pub boundaries: Vec<String>,
    /// 强指纹（FNV-1a 64 位，用于变更检测，**不含原文**）
    pub fingerprint: String,
}

/// FNV-1a 64 位指纹。
fn fnv1a(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// 从条目正文抽取“边界 / 禁止域”线索行（粗粒度，仅供人判读）。
fn extract_boundaries(text: &str) -> Vec<String> {
    const KEYS: &[&str] = &[
        "禁止", "不得", "不可", "严禁", "红线", "必须", "只能", "永远", "绝不", "铁则",
    ];
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for line in text.lines() {
        let t = line.trim().trim_start_matches(['-', '*', '#', '>', ' ']).trim();
        if t.is_empty() || t.chars().count() > 80 {
            continue;
        }
        if KEYS.iter().any(|k| t.contains(k)) && seen.insert(t.to_string()) {
            out.push(t.to_string());
            if out.len() >= 12 {
                break;
            }
        }
    }
    out
}

/// 读取当前生效的上层法理条目（L0/L0.5/L1/L1x/ANCHOR/GUARD）。
pub fn baseline_entries(conn: &Connection) -> Result<Vec<BaselineEntry>> {
    let mut stmt = conn.prepare(
        "SELECT category,key,content,priority,IFNULL(source,'') FROM long_term_memory
         ORDER BY priority, category, key",
    )?;
    let rows: Vec<(String, String, String, i64, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
        .filter_map(|x| x.ok())
        .collect();
    let mut out = Vec::new();
    for (cat, key, content, _pri, _src) in rows {
        let layer = ltm_category_to_layer(&cat, &key).to_string();
        if !is_upper_layer(&layer) {
            continue;
        }
        let chars = content.chars().count();
        let summary: String = content.trim().chars().take(160).collect();
        let boundaries = extract_boundaries(&content);
        let fingerprint = fnv1a(&content);
        out.push(BaselineEntry { layer, category: cat, key, chars, summary, boundaries, fingerprint });
    }
    // 按法理优先级从高到低排序
    out.sort_by(|a, b| layer_priority(&b.layer).cmp(&layer_priority(&a.layer)));
    Ok(out)
}

/// 基线整体指纹（各条指纹拼接后再哈希；用于变更检测）。
pub fn baseline_fingerprint(entries: &[BaselineEntry]) -> String {
    let mut s = String::new();
    for e in entries {
        s.push_str(&e.layer);
        s.push('/');
        s.push_str(&e.key);
        s.push('=');
        s.push_str(&e.fingerprint);
        s.push('\n');
    }
    fnv1a(&s)
}

// ────────────────────────────────── 1. world-gov bootstrap

/// 建立并持久化「世界观法理基线」（只存摘要指纹，不存完整原文）。
pub fn bootstrap(conn: &Connection, lib_root: &Path, dry_run: bool) -> Result<Value> {
    let entries = baseline_entries(conn)?;
    let fp = baseline_fingerprint(&entries);
    let report = json!({
        "command": "world-gov bootstrap",
        "law_chain": LAW_CHAIN.iter().map(|(l, n)| json!({"layer": l, "name": n})).collect::<Vec<_>>(),
        "entries": entries,
        "entry_count": entries.len(),
        "baseline_fingerprint": fp,
        "note": "只存摘要/边界线索/指纹，不存完整原文；不修改任何文件。",
        "dry_run": dry_run,
    });
    if !dry_run {
        let path = lib_root.join("07_勘误工坊").join("world_baseline.json");
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(&path, serde_json::to_string_pretty(&report)?)?;
    }
    Ok(report)
}

// ────────────────────────────────── 定位条目 / 卡片

/// 在库索引 + LTM 里定位一个 key：返回 (layer, source, title, text, types)。
fn locate(conn: &Connection, lib_root: &Path, key: &str) -> Result<Option<Value>> {
    // ① 先查 library_index（磁盘文件的索引）
    let row: Option<(String, String, String, String)> = conn
        .query_row(
            "SELECT layer,IFNULL(source,''),IFNULL(title,''),IFNULL(types,'[]')
             FROM library_index WHERE key=?1 LIMIT 1",
            params![key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    if let Some((layer, source, title, types)) = row {
        let path = lib_root.join(&source);
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        return Ok(Some(json!({
            "layer": layer, "source": source, "title": title,
            "text": text, "types": serde_json::from_str::<Value>(&types).unwrap_or(json!([])),
            "found_in": "library_index",
        })));
    }
    // ② 回退查 long_term_memory
    let row2: Option<(String, String, String)> = conn
        .query_row(
            "SELECT category,key,content FROM long_term_memory WHERE key=?1 LIMIT 1",
            params![key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    if let Some((cat, k, content)) = row2 {
        let layer = ltm_category_to_layer(&cat, &k).to_string();
        let title = extract_title(&content);
        let types: Vec<String> = extract_codes(&content).iter().map(|c| code_prefix(c)).collect();
        return Ok(Some(json!({
            "layer": layer, "source": format!("long_term_memory:{cat}"),
            "title": title, "text": content, "types": types, "found_in": "long_term_memory",
        })));
    }
    Ok(None)
}

/// 计算某卡“候选应遵从的上层条目”：按法理优先级排序，标注代码重叠相关度。
fn candidate_laws(entries: &[BaselineEntry], card_types: &[String]) -> Vec<Value> {
    let mut out: Vec<Value> = entries
        .iter()
        .map(|e| {
            let overlap: Vec<String> = e
                .boundaries
                .iter()
                .flat_map(|b| extract_codes(b))
                .map(|c| code_prefix(&c))
                .filter(|p| card_types.iter().any(|t| t == p))
                .collect();
            json!({
                "layer": e.layer,
                "layer_name": layer_name(&e.layer),
                "key": e.key,
                "priority": layer_priority(&e.layer),
                "char_overlap": overlap,
                "basis_snippet": e.summary.chars().take(80).collect::<String>(),
                "boundaries": e.boundaries,
            })
        })
        .collect();
    // 优先级降序（上层在前）
    out.sort_by(|a, b| b["priority"].as_i64().cmp(&a["priority"].as_i64()));
    out
}

// ────────────────────────────────── 2. world-gov inspect-card

/// 单卡深度检视：产出**探针包 probe_pack**（默认零模型）。
///
/// 探针包 = 结构化事实 + 待推理问题清单，交上层 agent（或 `--semantic` 进程内模型）裁决。
pub fn inspect_card(conn: &Connection, lib_root: &Path, key: &str) -> Result<Value> {
    let Some(found) = locate(conn, lib_root, key)? else {
        return Ok(json!({
            "command": "world-gov inspect-card", "key": key,
            "status": "NOT_FOUND",
            "note": "library_index 与 long_term_memory 均无此 key；不做任何判定。",
        }));
    };
    let text = found["text"].as_str().unwrap_or("").to_string();
    let card_types: Vec<String> = found["types"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let entries = baseline_entries(conn)?;
    let laws = candidate_laws(&entries, &card_types);
    let codes = extract_codes(&text);

    let probe = json!({
        "key": key,
        "layer": found["layer"],
        "title": found["title"],
        "found_in": found["found_in"],
        "source": found["source"],
        "card_chars": text.chars().count(),
        "card_codes": codes,
        "card_types": card_types,
        "card_text": text,
        "law_chain": LAW_CHAIN.iter().map(|(l, n)| json!({"layer": l, "name": n})).collect::<Vec<_>>(),
        "candidate_laws": laws,
        "questions": [
            "本卡是否内部自相矛盾（同卡内前后定义打架）？",
            "本卡是否直接或间接推翻任一上层法理（引用哪一条原话）？",
            "本卡应遵从的上层约束是哪几条（逐条列出并给出依据片段）？",
            "本卡是否存在“法理倒转”——下层定义了一个推翻上层边界的规则？",
        ],
        "verdict_rule": "无依据不得判冲突；拿不准标 UNCERTAIN；每条判定必须附【原文片段 + 依据的上层原话】。",
    });
    Ok(json!({
        "command": "world-gov inspect-card",
        "mode": "probe_pack",
        "status": "PROBE_READY",
        "probe_pack": probe,
    }))
}

/// 组装 `--semantic` 用的对话消息（system, user）。main.rs 负责真正调用模型。
///
/// **自第 6 批起委托给 `governance::prompt_builder`**（单一事实源）：
/// 结构化片段 + 溯源标记 + 裁剪防护统一在那里实现，避免此处另起一套。
pub fn semantic_messages(probe: &Value) -> (String, String) {
    let built = crate::governance::prompt_builder::build_inspect_prompt(probe);
    (built.system, built.user)
}

/// 解析模型返回（宽容）：能解析出 JSON 就用，否则整体标 UNCERTAIN。
pub fn parse_semantic_verdict(key: &str, raw: &str) -> Value {
    if let Ok(v) = serde_json::from_str::<Value>(raw.trim()) {
        if v.get("verdict").is_some() {
            return v;
        }
    }
    // 尝试从文本里抠出第一个 {...}
    if let (Some(a), Some(b)) = (raw.find('{'), raw.rfind('}')) {
        if a < b {
            if let Ok(v) = serde_json::from_str::<Value>(&raw[a..=b]) {
                if v.get("verdict").is_some() {
                    return v;
                }
            }
        }
    }
    json!({
        "key": key,
        "verdict": "UNCERTAIN",
        "findings": [{
            "kind": "解析失败",
            "law_ref": "", "card_snippet": "",
            "explain": "模型返回无法解析为约定 JSON；按铁则标 UNCERTAIN，不强行归为风险。",
            "suggest": "人工复核或重跑。"
        }],
        "raw_excerpt": raw.chars().take(400).collect::<String>(),
    })
}

// ────────────────────────────────── 3. world-gov trace

/// 溯源链路：`key ← 受约束于 ← 上层各条 ← L0 总纲源头`。
pub fn trace(conn: &Connection, lib_root: &Path, key: &str) -> Result<Value> {
    let entries = baseline_entries(conn)?;
    let found = locate(conn, lib_root, key)?;
    let (card_layer, card_types, card_title) = match &found {
        Some(f) => (
            f["layer"].as_str().unwrap_or("").to_string(),
            f["types"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect::<Vec<_>>()).unwrap_or_default(),
            f["title"].as_str().unwrap_or("").to_string(),
        ),
        None => {
            return Ok(json!({
                "command": "world-gov trace", "key": key, "status": "NOT_FOUND",
                "note": "未在 library_index / long_term_memory 定位到该条目。",
            }));
        }
    };

    let card_priority = layer_priority(&card_layer);
    // 只保留“高于该条目”的上层条目作为约束来源
    let chain: Vec<Value> = entries
        .iter()
        .filter(|e| layer_priority(&e.layer) > card_priority)
        .map(|e| json!({
            "layer": e.layer,
            "layer_name": layer_name(&e.layer),
            "key": e.key,
            "priority": layer_priority(&e.layer),
            "basis_snippet": e.summary.chars().take(100).collect::<String>(),
        }))
        .collect();

    let mut visual = format!("{key}");
    for c in &chain {
        visual.push_str(&format!(
            "\n  ← 受约束于 ← [{} {}] {}",
            c["layer"].as_str().unwrap_or(""),
            c["layer_name"].as_str().unwrap_or(""),
            c["key"].as_str().unwrap_or("")
        ));
    }
    if let Some(l0) = chain.iter().find(|c| c["layer"] == "L0") {
        visual.push_str(&format!("\n  ← 源头 ← L0 总纲：{}", l0["key"].as_str().unwrap_or("")));
    }

    Ok(json!({
        "command": "world-gov trace",
        "key": key,
        "status": "OK",
        "card": { "layer": card_layer, "title": card_title, "types": card_types },
        "chain": chain,
        "chain_len": chain.len(),
        "visual": visual,
        "note": "仅按法理优先级列出上层来源，供人判读；不做自动修改。",
    }))
}

// ────────────────────────────────── 4. world-gov scan-library（骨架）

/// 解析 `--slice N/M`（N=第几片从 1 起，M=总片数）。
/// 边界防御：分母<2 拒绝（分1片等于没分）；非数字/空段拒绝；N>M 拒绝。
/// 返回 Err(友好文案)，调用方退出非 0——**禁止静默截断**。
pub fn parse_slice_strict(s: &str) -> Result<(usize, usize), String> {
    let t = s.trim();
    if t.is_empty() {
        return Err("--slice 不能为空，格式应为 N/M（如 1/4）".into());
    }
    let (a, b) = t
        .split_once('/')
        .ok_or_else(|| format!("--slice 缺少 '/'：格式应为 N/M（如 1/4），收到: {s}"))?;
    let bad_num = |x: &str| format!("--slice 含非数字段：'{x}'（格式 N/M，如 1/4）");
    let n: usize = a.trim().parse().map_err(|_| bad_num(a))?;
    let m: usize = b.trim().parse().map_err(|_| bad_num(b))?;
    if m < 2 {
        return Err(format!("--slice 分母必须 ≥2（分 1 片等于没分批），收到: {s}"));
    }
    if n == 0 {
        return Err("--slice 起始片 N 必须 ≥1".into());
    }
    if n > m {
        return Err(format!("--slice 越界：N({n}) 不得大于 M({m})"));
    }
    Ok((n, m))
}

/// 校验分片粒度：单分片条数不得超过 max_items（防一次性爆算力）。
/// 超限返回 Err，调用方应提示调大 M。
pub fn check_slice_granularity(n: usize, m: usize, total: usize, max_items: usize) -> Result<(), String> {
    let size = total.div_ceil(m.max(1));
    if size > max_items {
        return Err(format!(
            "单分片 {size} 条超过上限 {max_items}（全库 {total} 条 / {m} 片）；请调大 M（建议 ≥ {}）",
            total.div_ceil(max_items)
        ));
    }
    let _ = n;
    Ok(())
}

/// 第 n 片（共 m 片）在 len 条列表上的连续区间 [start, end)。
/// 连续分块：片大小 = ceil(len/m)，末片允许不满。
pub fn slice_range(n: usize, m: usize, len: usize) -> (usize, usize) {
    let size = len.div_ceil(m.max(1));
    let start = (n - 1).saturating_mul(size).min(len);
    let end = (start + size).min(len);
    (start, end)
}

/// 把（未来的）语义项路由进 risk_index：WARN/HIGH-RISK 入语义桶（is_semantic=1 + law_basis）；
/// UNCERTAIN/OK **不入库**（铁则：拿不准不强行归为风险）。返回真实入库条数。
///
/// 幂等键 = risk_type + source + message；message 由 **key + kind + verdict + law_hash** 复合构成，
/// 同一条语义风险（同 key 同判定同依据）重复路由不会重复刷库。
pub fn emit_semantic_findings(
    conn: &Connection,
    source_cmd: &str,
    findings: &[Value],
) -> Result<usize> {
    let mut n = 0usize;
    for f in findings {
        let v = f["verdict"].as_str().unwrap_or("UNCERTAIN");
        if v != "WARN" && v != "HIGH-RISK" {
            continue; // UNCERTAIN / OK 不入库
        }
        let key = f["key"].as_str().unwrap_or("");
        let kind = f["kind"].as_str().unwrap_or("语义风险");
        let law = f["law_ref"].as_str().unwrap_or("");
        // law_hash：依据文本的轻量指纹（FNV-1a 截 8 hex），使幂等键对"依据被改写"敏感
        let law_hash = {
            let mut h: u64 = 0xcbf29ce484222325;
            for b in law.as_bytes() {
                h ^= *b as u64;
                h = h.wrapping_mul(0x100000001b3);
            }
            format!("{h:08x}")
        };
        let msg = format!(
            "语义{kind}[{v}] {key} :: law#{law_hash} :: {}",
            f["explain"].as_str().unwrap_or("")
        );
        if crate::governance::record_risk_semantic(
            conn, "world_gov", None, None, source_cmd, v, &msg, law,
        )? {
            n += 1; // 只统计真实插入；幂等跳过不计
        }
    }
    Ok(n)
}

/// scan-library 骨架：产出**探针分片清单**（零模型；不产出任何语义判定）。
///
/// - `--slice N/M` 把 library_index 全条目切成连续分块，输出第 N 片元数据清单；
/// - `--semantic` **仅参数占位**：通路已预埋（emit_semantic_findings → risk_index.is_semantic=1），
///   真实模型推理等单独放行；骨架阶段 findings 恒空；
/// - `--dry-run` 沙盘：只打印，不落盘清单文件。
pub fn scan_library(
    conn: &Connection,
    lib_root: &Path,
    slice: Option<&str>,
    semantic: bool,
    dry_run: bool,
) -> Result<Value> {
    let (sn, sm) = match slice {
        Some(s) => parse_slice_strict(s).map_err(|e| anyhow::anyhow!("{e}"))?,
        None => (1, 1),
    };
    let mut stmt = conn.prepare(
        "SELECT layer,key,IFNULL(title,''),chars,IFNULL(source,'') FROM library_index
         ORDER BY layer,key",
    )?;
    let all: Vec<Value> = stmt
        .query_map([], |r| {
            Ok(json!({
                "layer": r.get::<_,String>(0)?,
                "key": r.get::<_,String>(1)?,
                "title": r.get::<_,String>(2)?,
                "chars": r.get::<_,i64>(3)?,
                "source": r.get::<_,String>(4)?,
            }))
        })?
        .filter_map(|x| x.ok())
        .collect();
    let total = all.len();
    // 粒度防御：单分片 >250 条拒绝（防一次性爆算力），提示调大 M
    check_slice_granularity(sn, sm, total, 250).map_err(|e| anyhow::anyhow!("{e}"))?;
    let (start, end) = slice_range(sn, sm, total);
    let probe_items: Vec<Value> = all[start..end].to_vec();

    let report = json!({
        "command": "world-gov scan-library",
        "mode": "probe_slice",
        "slice": {"n": sn, "m": sm, "offset": start, "items": end - start, "total": total},
        "semantic": if semantic {
            "requested_but_locked（--semantic 仅占位；真实推理等单独放行）"
        } else {
            "off（默认零模型）"
        },
        "semantic_channel": "ready（emit_semantic_findings → risk_index.is_semantic=1 + law_basis）",
        "probe_items": probe_items,
        "law_chain": LAW_CHAIN.iter().map(|(l, n)| json!({"layer": l, "name": n})).collect::<Vec<_>>(),
        "questions": [
            "a) 内容层面真重复/近义冲突（两张卡讲同一东西但设定打架）？",
            "b) L4 卡违背 L0~GUARD 上层法理（引用哪条原话）？",
            "c) 同层条目内部自噬？",
            "d) 隐形悬空：L3 逻辑依赖某设定但无显式 key 可锚定？",
            "e) 法理倒转：下层定义推翻上层边界的规则？",
        ],
        "verdict_rule": "严禁幻觉式乱报警告；拿不准统一标 UNCERTAIN，不强行归为风险；判冲突必须附【双方原文片段 + 上层法理原话】。",
        "findings": [], // 骨架阶段恒空（无模型不产出语义判定）
        "dry_run": dry_run,
        "note": "骨架阶段只产分片清单；模型放行后本清单即逐片模型输入。",
    });
    if !dry_run {
        let path = lib_root
            .join("07_勘误工坊")
            .join(format!("scan_probe_{sn}of{sm}.json"));
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(&path, serde_json::to_string_pretty(&report)?)?;
    }
    Ok(report)
}

// ────────────────────────────────── 5. world-gov check-outline（骨架）

/// 卷名合法性防护：仅允许 `卷01`~`卷99`（防路径逃逸/绝对路径注入）。
/// 用 strip_prefix 而非字节切片——"卷"是 3 字节 UTF-8，`[1..]` 会切在字符中间 panic。
pub fn validate_volume_name(volume: &str) -> Result<(), String> {
    let digits = match volume.strip_prefix('卷') {
        Some(d) => d,
        None => {
            return Err(format!(
                "卷名非法：'{volume}'（仅允许 卷01~卷99；拒绝 ../、绝对路径等注入）"
            ))
        }
    };
    let shape_ok = !digits.is_empty()
        && digits.len() <= 2
        && digits.chars().all(|c| c.is_ascii_digit());
    let n = digits.parse::<u32>().ok();
    if !(shape_ok && matches!(n, Some(1..=99))) {
        return Err(format!(
            "卷名非法：'{volume}'（仅允许 卷01~卷99；拒绝 ../、绝对路径等注入）"
        ));
    }
    Ok(())
}

/// check-outline 骨架：输出**卷细纲探针包**（零模型；只出报告不改细纲）。
///
/// 定位该卷 L3 细纲 → 附上层法理基线摘要 + 越界/自洽/L4支撑三问 + verdict_rule；
/// 大文件只读头部（32KB）防百万字爆栈，全量等模型放行后分片。
pub fn check_outline(
    conn: &Connection,
    lib_root: &Path,
    volume: &str,
    semantic: bool,
    dry_run: bool,
) -> Result<Value> {
    // 路径逃逸防护：仅允许 卷01~卷99
    validate_volume_name(volume).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut stmt = conn.prepare(
        "SELECT key,IFNULL(source,''),IFNULL(title,''),chars FROM library_index
         WHERE layer='L3' AND (source LIKE '%'||?1||'%' OR key LIKE '%'||?1||'%')
         ORDER BY key",
    )?;
    let files: Vec<Value> = stmt
        .query_map([volume], |r| {
            Ok(json!({
                "key": r.get::<_,String>(0)?,
                "source": r.get::<_,String>(1)?,
                "title": r.get::<_,String>(2)?,
                "chars": r.get::<_,i64>(3)?,
            }))
        })?
        .filter_map(|x| x.ok())
        .collect();

    // 附上每个文件的头部探针（大文件只读 32KB）
    let mut probe_files = Vec::new();
    for f in &files {
        let src = f["source"].as_str().unwrap_or("");
        let path = lib_root.join(src);
        let head = if path.is_file() {
            crate::governance::read_head(&path, 32 * 1024)
        } else {
            String::new()
        };
        probe_files.push(json!({
            "key": f["key"], "source": src, "chars": f["chars"],
            "title": f["title"],
            "head_present": !head.is_empty(),
            "head_32k": head,
        }));
    }

    let baseline = baseline_entries(conn)?;
    if files.is_empty() {
        return Err(anyhow::anyhow!(
            "未找到卷 {volume} 的 L3 细纲（library_index 层无匹配文件）；请确认卷号或先跑 library-ctl init"
        ));
    }
    let baseline_brief: Vec<Value> = baseline
        .iter()
        .map(|e| json!({
            "layer": e.layer, "key": e.key,
            "summary": e.summary.chars().take(80).collect::<String>(),
            "boundaries": e.boundaries,
        }))
        .collect();

    let report = json!({
        "command": "world-gov check-outline",
        "mode": "outline_probe",
        "volume": volume,
        "semantic": if semantic {
            "requested_but_locked（--semantic 仅占位；真实推理等单独放行）"
        } else {
            "off（默认零模型）"
        },
        "files_found": files.len(),
        "outline_probe_files": probe_files,
        "upper_law_baseline": baseline_brief,
        "law_chain": LAW_CHAIN.iter().map(|(l, n)| json!({"layer": l, "name": n})).collect::<Vec<_>>(),
        "questions": [
            "① 本卷目标是否越出 L1/L1x/锚点边界（引用哪条原话）？",
            "② 细纲内部逻辑是否自洽（前后矛盾点定位）？",
            "③ 细纲引用的 L4 在世界观层面能否支撑剧情（逐 key 列依据）？",
        ],
        "verdict_rule": "只出评估报告，绝不修改细纲；无依据不判违规；拿不准 UNCERTAIN。",
        "findings": [], // 骨架阶段恒空（无模型不产出语义判定）
        "dry_run": dry_run,
        "note": "骨架只产探针包；大文件只读头部 32KB，全量评估待模型放行后分片。",
    });
    if !dry_run {
        let safe_vol: String = volume
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let path = lib_root
            .join("07_勘误工坊")
            .join(format!("outline_probe_{safe_vol}.json"));
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(&path, serde_json::to_string_pretty(&report)?)?;
    }
    Ok(report)
}

// ────────────────────────────────── 单测（锁定法理契约关键行为）

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a_stable_and_sensitive() {
        let a = fnv1a("残响之河");
        let b = fnv1a("残响之河");
        let c = fnv1a("残响之和");
        assert_eq!(a, b, "同输入必须同指纹");
        assert_ne!(a, c, "一字之差必须变指纹");
        assert_eq!(a.len(), 16, "64位 FNV 输出 16 hex");
    }

    #[test]
    fn baseline_fingerprint_changes_when_entry_changes() {
        let mut e1 = vec![BaselineEntry {
            layer: "L0".into(), category: "顶层设定".into(), key: "整本概括".into(),
            chars: 10, summary: "旧内容".into(), boundaries: vec![], fingerprint: fnv1a("旧内容"),
        }];
        let fp1 = baseline_fingerprint(&e1);
        e1[0].fingerprint = fnv1a("新内容");
        let fp2 = baseline_fingerprint(&e1);
        assert_ne!(fp1, fp2, "条目变更必须反映到基线指纹");
    }

    #[test]
    fn extract_boundaries_keeps_rule_lines_and_caps() {
        let mut text = String::from("# 标题行（无关键词）\n");
        for i in 0..20 {
            text.push_str(&format!("- 禁止第{i}号行为\n"));
        }
        text.push_str("普通描述行，没有任何禁令词。\n");
        let out = extract_boundaries(&text);
        assert_eq!(out.len(), 12, "必须封顶 12 条");
        assert!(out.iter().all(|x| x.contains("禁止")), "只收规则行");
    }

    #[test]
    fn candidate_laws_sorted_by_priority_desc() {
        let mk = |layer: &str, key: &str| BaselineEntry {
            layer: layer.into(), category: "c".into(), key: key.into(),
            chars: 1, summary: "s".into(), boundaries: vec![], fingerprint: "f".into(),
        };
        let entries = vec![mk("L4", "x"), mk("L0", "a"), mk("GUARD", "g"), mk("L1", "b")];
        let laws = candidate_laws(&entries, &[]);
        let pris: Vec<i64> = laws.iter().map(|l| l["priority"].as_i64().unwrap()).collect();
        let mut sorted = pris.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(pris, sorted, "候选法理必须按优先级降序（上层在前）");
        assert_eq!(laws[0]["layer"], "L0");
    }

    #[test]
    fn parse_verdict_accepts_json_and_text_json() {
        let direct = parse_semantic_verdict("K", r#"{"key":"K","verdict":"OK","findings":[]}"#);
        assert_eq!(direct["verdict"], "OK");
        let embedded = parse_semantic_verdict("K", "前言 ```json\n{\"key\":\"K\",\"verdict\":\"WARN\",\"findings\":[]}\n``` 后记");
        assert_eq!(embedded["verdict"], "WARN");
    }

    #[test]
    fn parse_verdict_garbage_becomes_uncertain() {
        let v = parse_semantic_verdict("K", "模型胡言乱语，没有 JSON。");
        assert_eq!(v["verdict"], "UNCERTAIN", "解析失败必须标 UNCERTAIN，不强行归为风险");
    }

    #[test]
    fn semantic_risk_goes_to_semantic_bucket() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("governance_schema.sql")).unwrap();
        crate::governance::ensure_risk_index_cols(&conn).unwrap();
        crate::governance::record_risk_semantic(&conn, "world_gov", None, None, "t", "HIGH-RISK", "m1", "违反《全局世界观铁则》原文…").unwrap();
        crate::governance::record_risk_semantic(&conn, "world_gov", None, None, "t", "HIGH-RISK", "m1", "重复插入应幂等").unwrap();
        let r = crate::governance::risk_list(&conn, false).unwrap();
        assert_eq!(r["semantic_count"], 1, "语义风险入语义桶且幂等");
        assert_eq!(r["structural_count"], 0);
        assert_eq!(r["items"][0]["law_basis"], "违反《全局世界观铁则》原文…");
    }

    #[test]
    fn law_chain_order_is_the_contract() {
        let names: Vec<&str> = LAW_CHAIN.iter().map(|(l, _)| *l).collect();
        assert_eq!(names, vec!["L0", "L0.5", "L1", "L1x", "ANCHOR", "GUARD", "L3", "L4"]);
        assert!(is_upper_layer("ANCHOR") && is_upper_layer("GUARD"));
        assert!(!is_upper_layer("L3") && !is_upper_layer("L4"), "L3/L4 是被约束方");
    }

    // ─────── 第 2 批：骨架行为锁定 ───────

    #[test]
    fn parse_slice_accepts_valid_and_rejects_bad() {
        assert_eq!(parse_slice_strict("1/4").unwrap(), (1, 4));
        assert_eq!(parse_slice_strict("4/4").unwrap(), (4, 4));
        assert!(parse_slice_strict("0/4").is_err(), "N 必须 ≥1");
        assert!(parse_slice_strict("5/4").is_err(), "N 不得大于 M");
        assert!(parse_slice_strict("abc").is_err());
        assert!(parse_slice_strict("2/").is_err());
    }

    #[test]
    fn slice_range_covers_all_without_overlap() {
        // 10 条分 4 片：片大小 ceil(10/4)=3 → [0,3)[3,6)[6,9)[9,10)
        assert_eq!(slice_range(1, 4, 10), (0, 3));
        assert_eq!(slice_range(2, 4, 10), (3, 6));
        assert_eq!(slice_range(3, 4, 10), (6, 9));
        assert_eq!(slice_range(4, 4, 10), (9, 10));
        // 边界：空库 / n 超界被钳制
        assert_eq!(slice_range(1, 1, 0), (0, 0));
        assert_eq!(slice_range(3, 2, 5), (5, 5), "n>m 由 parse_slice 拦，此处防御性钳制");
    }

    #[test]
    fn emit_routes_only_real_risks_into_semantic_bucket() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("governance_schema.sql")).unwrap();
        crate::governance::ensure_risk_index_cols(&conn).unwrap();
        let findings = vec![
            serde_json::json!({"key":"K1","verdict":"HIGH-RISK","kind":"推翻上层","law_ref":"GUARD/全局世界观铁则：禁止X","explain":"e1"}),
            serde_json::json!({"key":"K2","verdict":"UNCERTAIN","kind":"内部矛盾","law_ref":"","explain":"e2"}),
            serde_json::json!({"key":"K3","verdict":"OK","kind":"应遵从约束","law_ref":"L0","explain":"e3"}),
            serde_json::json!({"key":"K4","verdict":"WARN","kind":"法理倒转","law_ref":"ANCHOR/跨卷线锚点表：Y 不可破","explain":"e4"}),
        ];
        let n = emit_semantic_findings(&conn, "world-gov scan-library", &findings).unwrap();
        assert_eq!(n, 2, "只有 WARN/HIGH-RISK 入库；UNCERTAIN/OK 不入库");
        let r = crate::governance::risk_list(&conn, false).unwrap();
        assert_eq!(r["semantic_count"], 2);
        assert_eq!(r["structural_count"], 0);
        let items = r["items"].as_array().unwrap();
        assert!(items.iter().all(|i| i["is_semantic"] == 1));
        assert!(items.iter().all(|i| !i["law_basis"].as_str().unwrap_or("").is_empty()),
            "每条语义风险必须带法理依据");
    }

    #[test]
    fn emit_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("governance_schema.sql")).unwrap();
        crate::governance::ensure_risk_index_cols(&conn).unwrap();
        let f = vec![serde_json::json!({"key":"K","verdict":"HIGH-RISK","kind":"k","law_ref":"L","explain":"e"})];
        let n1 = emit_semantic_findings(&conn, "s", &f).unwrap();
        let n2 = emit_semantic_findings(&conn, "s", &f).unwrap();
        assert_eq!(n1, 1);
        assert_eq!(n2, 0, "同 finding 重复路由必须幂等");
    }

    // ─────── 第 2 批加固：门控 + 边界防御 ───────

    #[test]
    fn gate_logic_all_8_combinations() {
        use crate::governance::gate_logic;
        // 三条件全真才开门；8 种组合全覆盖
        assert!(!gate_logic(false, false, false));
        assert!(!gate_logic(false, false, true));
        assert!(!gate_logic(false, true, false));
        assert!(!gate_logic(false, true, true), "编译侧 false 时其余再真也不开门");
        assert!(!gate_logic(true, false, false));
        assert!(!gate_logic(true, false, true));
        assert!(!gate_logic(true, true, false));
        assert!(gate_logic(true, true, true), "唯一开门组合");
    }

    #[test]
    fn semantic_gate_compile_default_is_false() {
        // 编译侧硬开关默认必须 false（防误提交 true）
        assert!(!crate::governance::SEMANTIC_ENABLE_GATE);
    }

    #[test]
    fn parse_slice_strict_rejects_all_bad_shapes() {
        // 分母 < 2
        assert!(parse_slice_strict("1/1").is_err(), "分1片等于没分批");
        // 非数字 / 空段 / 缺斜杠
        assert!(parse_slice_strict("a/b").is_err());
        assert!(parse_slice_strict("2/").is_err());
        assert!(parse_slice_strict("/4").is_err());
        assert!(parse_slice_strict("abc").is_err());
        assert!(parse_slice_strict("").is_err());
        // 越界 / 零
        assert!(parse_slice_strict("5/4").is_err(), "N 不得大于 M");
        assert!(parse_slice_strict("0/4").is_err(), "N 必须 ≥1");
        // 合法
        assert_eq!(parse_slice_strict("1/4").unwrap(), (1, 4));
        assert_eq!(parse_slice_strict("4/4").unwrap(), (4, 4));
    }

    #[test]
    fn slice_granularity_blocks_oversized_slice() {
        // 684 条 / 2 片 = 342 条/片 > 250 上限 → 拒绝
        assert!(check_slice_granularity(1, 2, 684, 250).is_err());
        // 684 / 4 = 171 ≤ 250 → 放行
        assert!(check_slice_granularity(1, 4, 684, 250).is_ok());
        // 边界：恰好 250 放行，251 拒绝
        assert!(check_slice_granularity(1, 2, 500, 250).is_ok());
        assert!(check_slice_granularity(1, 2, 501, 250).is_err());
    }

    #[test]
    fn volume_name_blocks_path_escape() {
        assert!(validate_volume_name("卷01").is_ok());
        assert!(validate_volume_name("卷99").is_ok());
        // 路径逃逸 / 注入 / 越界数字
        assert!(validate_volume_name("../etc").is_err(), "路径逃逸必须被拦");
        assert!(validate_volume_name("/etc/passwd").is_err());
        assert!(validate_volume_name("卷100").is_err(), "仅允许 卷01~卷99");
        assert!(validate_volume_name("卷0").is_err());
        assert!(validate_volume_name("卷").is_err());
        assert!(validate_volume_name("卷ab").is_err());
        assert!(validate_volume_name("").is_err());
    }

    #[test]
    fn emit_idempotent_key_changes_when_law_changes() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("governance_schema.sql")).unwrap();
        crate::governance::ensure_risk_index_cols(&conn).unwrap();
        let f1 = vec![serde_json::json!({"key":"K","verdict":"HIGH-RISK","kind":"k","law_ref":"铁则原文A","explain":"e"})];
        let f2 = vec![serde_json::json!({"key":"K","verdict":"HIGH-RISK","kind":"k","law_ref":"铁则原文B","explain":"e"})];
        assert_eq!(emit_semantic_findings(&conn, "s", &f1).unwrap(), 1);
        assert_eq!(emit_semantic_findings(&conn, "s", &f1).unwrap(), 0, "同 key+verdict+law 重复 → 幂等拦");
        assert_eq!(emit_semantic_findings(&conn, "s", &f2).unwrap(), 1, "依据文本变化 → law_hash 变 → 允许新入库");
    }
}