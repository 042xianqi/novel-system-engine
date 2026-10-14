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
pub fn semantic_messages(probe: &Value) -> (String, String) {
    let system = "你是《残响之河》的**世界观法务官**。法理优先级：L0 ≫ L0.5 ≫ L1 ≫ L1x ≫ ANCHOR ≫ GUARD ≫ L3 ≫ L4；\
上层是法理来源，下层只能细化/实例化，不得反向否定。\n\
你的任务：只出**证据与建议**，不改写、不删改任何内容。\n\
铁律：\n\
1) 判“冲突/违背”必须引用**上层基线的原话或原文片段**作为依据，找不到依据一律判 UNCERTAIN，禁止幻觉式报警。\n\
2) 输出严格 JSON：{\"key\":\"...\",\"verdict\":\"OK|WARN|HIGH-RISK|UNCERTAIN\",\"findings\":[{\"kind\":\"内部矛盾|推翻上层|法理倒转|应遵从约束\",\"law_ref\":\"违反/依据的上层条目key与原话\",\"card_snippet\":\"卡片原文片段\",\"explain\":\"...\",\"suggest\":\"仅供人参考\"}]}。\n\
3) 无法确证的一律进 findings 且 kind 标注清楚、verdict 至少 UNCERTAIN。";
    let user = format!(
        "【待检视卡片探针包】\n{}\n\n请按上述 JSON 格式输出检视结论。",
        serde_json::to_string_pretty(probe).unwrap_or_default()
    );
    (system.to_string(), user.to_string())
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
}