//! prompt_builder —— 探针数据 → 结构化 prompt 文本（**规则层，单一职责**）。
//!
//! # 定位
//! 只做一件事：把 scan-library / check-outline / inspect-card 产出的**探针 JSON**
//! 组装成**结构化 prompt 文本**，交给上层（`--semantic` 通路或人工预览）。
//!
//! 本层**不做**：不调用模型、不碰网络、不写库、不做任何语义判定、不生成任何真实结论。
//!
//! # 三条硬性设计约束（对应用户要求）
//! 1. **单向组装**：输入探针 JSON → 输出 prompt 文本；无回写、无副作用。
//! 2. **溯源标记**：每个注入片段都挂 `［来源 layer=.. key=.. @..］`，模型引用时必须回指。
//! 3. **裁剪防护**：单段超 [`MAX_SECTION_CHARS`] 截断；总量超 [`MAX_TOTAL_CHARS`] 降级为
//!    “仅溯源头 + 省略提示”。所有截断按**字符**切（中文安全），绝不按字节切。
//!
//! # 铁则
//! - ❌ 不内置模型地址 / 密钥（那不是本层职责）。
//! - ❌ 本层不是推理路径：它只产文本，是否外呼由 `semantic_runtime` + 三重门控决定。
//! - ✅ `prompt-preview` 子命令只调本层，**独立于推理管线**，方便迭代提示词而无需跑队列。

use serde::Serialize;
use serde_json::Value;

// ────────────────────────────────── 常量

/// 法理优先级文本（本层独立常量，避免反向依赖 world_gov）。
pub const LAW_CHAIN_TEXT: &str = "L0 ≫ L0.5 ≫ L1 ≫ L1x ≫ ANCHOR ≫ GUARD ≫ L3 ≫ L4";

/// 单段注入文本上限（字符）。
pub const MAX_SECTION_CHARS: usize = 6000;

/// user 消息总量上限（字符）。
pub const MAX_TOTAL_CHARS: usize = 24000;

/// 总量超限时，被降级片段仍保留的摘要字符数（只留溯源 + 这点摘要）。
pub const DEGRADED_SUMMARY_CHARS: usize = 160;

// ────────────────────────────────── 结构

/// 一个可溯源的 prompt 片段。
#[derive(Debug, Clone, Serialize)]
pub struct PromptSection {
    /// 片段标题（如 “待检视卡片正文”）
    pub heading: String,
    /// 来源法理层（L0/L4/L3…），未知可空
    pub layer: String,
    /// 来源 key
    pub key: String,
    /// 来源位置（library_index / long_term_memory / world_baseline …）
    pub found_in: String,
    /// 实际注入文本（可能已截断）
    pub text: String,
    /// 截断前原始字符数
    pub original_chars: usize,
    /// 本段是否被截断
    pub truncated: bool,
}

impl PromptSection {
    /// 溯源标记行（模型引用时必须回指此标识）。
    pub fn provenance_line(&self) -> String {
        format!(
            "［来源 layer={} key={} @{}］",
            or_dash(&self.layer),
            or_dash(&self.key),
            or_dash(&self.found_in)
        )
    }

    /// 渲染为一段：标题 + 溯源 + 正文（含截断提示）。
    pub fn render_block(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("\n### {}\n", self.heading));
        s.push_str(&format!("{}\n", self.provenance_line()));
        s.push_str(&self.text);
        if self.truncated {
            s.push_str(&format!(
                "\n［本段已裁剪：原文 {} 字，仅注入前 {} 字］",
                self.original_chars, MAX_SECTION_CHARS
            ));
        }
        s.push('\n');
        s
    }
}

/// 组装统计（供预览与审计）。
#[derive(Debug, Clone, Serialize)]
pub struct BuildStats {
    pub section_count: usize,
    pub total_chars: usize,
    pub truncated_sections: usize,
    pub truncated_total: bool,
    pub max_section_chars: usize,
    pub max_total_chars: usize,
}

/// 组装结果。
#[derive(Debug, Clone, Serialize)]
pub struct BuiltPrompt {
    pub system: String,
    pub user: String,
    pub sections: Vec<PromptSection>,
    pub stats: BuildStats,
}

impl BuiltPrompt {
    /// 溯源总表（key → layer@found_in），供审计与人工核对。
    pub fn provenance_index(&self) -> Vec<Value> {
        self.sections
            .iter()
            .map(|s| {
                serde_json::json!({
                    "heading": s.heading,
                    "layer": s.layer,
                    "key": s.key,
                    "found_in": s.found_in,
                    "chars": s.text.chars().count(),
                    "original_chars": s.original_chars,
                    "truncated": s.truncated,
                })
            })
            .collect()
    }
}

fn or_dash(s: &str) -> &str {
    if s.is_empty() {
        "-"
    } else {
        s
    }
}

// ────────────────────────────────── 组装累积器（做裁剪防护）

#[derive(Default)]
struct Acc {
    sections: Vec<PromptSection>,
}

impl Acc {
    /// 追加一个片段：单段超限则按**字符**截断（中文安全）。
    fn add(&mut self, heading: impl Into<String>, layer: &str, key: &str, found_in: &str, text: String) {
        let original_chars = text.chars().count();
        let (body, truncated) = if original_chars > MAX_SECTION_CHARS {
            (text.chars().take(MAX_SECTION_CHARS).collect::<String>(), true)
        } else {
            (text, false)
        };
        self.sections.push(PromptSection {
            heading: heading.into(),
            layer: layer.to_string(),
            key: key.to_string(),
            found_in: found_in.to_string(),
            text: body,
            original_chars,
            truncated,
        });
    }

    /// 渲染 user 文本并做**总量**裁剪（超预算片段降级为“溯源头 + 摘要”）。
    fn render(self) -> (Vec<PromptSection>, String, bool) {
        let mut out = String::new();
        let mut used = 0usize;
        let mut truncated_total = false;
        for s in &self.sections {
            let block = s.render_block();
            let blen = block.chars().count();
            if used + blen <= MAX_TOTAL_CHARS {
                out.push_str(&block);
                used += blen;
            } else {
                // 降级：只保留溯源头 + 极简摘要（仍可回指来源）
                truncated_total = true;
                let head = format!(
                    "\n### {}\n{}\n［超总量预算，已降级：原文 {} 字，仅留摘要］\n",
                    s.heading,
                    s.provenance_line(),
                    s.original_chars
                );
                out.push_str(&head);
                let tail: String = s.text.chars().take(DEGRADED_SUMMARY_CHARS).collect();
                out.push_str(&tail);
                out.push('\n');
                used += head.chars().count() + tail.chars().count();
            }
        }
        (self.sections, out, truncated_total)
    }
}

// ────────────────────────────────── system prompt（统一角色 + 铁则）

/// 统一的“世界观法务官”system prompt。
pub fn system_prompt() -> String {
    format!(
        "你是《残响之河》的世界观法务官（world-gov）。\n\
法理优先级：{LAW_CHAIN_TEXT}；上层是法理来源，下层只能细化/实例化，不得反向否定。\n\
你的任务：只出**证据与建议**，不改写、不删改任何内容。\n\
铁律：\n\
1) 判“冲突/违背”必须引用上层基线的**原话或原文片段**作为依据；找不到依据一律判 UNCERTAIN，禁止幻觉式报警。\n\
2) 输出严格 JSON：{{\"key\":\"...\",\"verdict\":\"OK|WARN|HIGH-RISK|UNCERTAIN\",\"findings\":[{{\"kind\":\"内部矛盾|推翻上层|法理倒转|应遵从约束\",\"law_ref\":\"违反/依据的上层条目key与原话\",\"card_snippet\":\"原文片段\",\"explain\":\"...\",\"suggest\":\"仅供人参考\"}}]}}。\n\
3) 无法确证的一律进 findings 且 kind 标注清楚、verdict 至少 UNCERTAIN。\n\
4) user 中每个片段都带 ［来源 layer=.. key=.. @..］ 标记，你引用时必须回指该来源标识。"
    )
}

// ────────────────────────────────── 工具

/// 从探针的 `law_chain` 渲染法理链文本；缺失则回退常量。
fn law_chain_from(v: &Value) -> String {
    match v.get("law_chain").and_then(|c| c.as_array()) {
        Some(arr) if !arr.is_empty() => arr
            .iter()
            .map(|e| e.get("layer").and_then(|x| x.as_str()).unwrap_or("?"))
            .collect::<Vec<_>>()
            .join(" ≫ "),
        _ => LAW_CHAIN_TEXT.to_string(),
    }
}

fn s_of<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("")
}

fn questions_block(v: &Value) -> String {
    let mut s = String::new();
    if let Some(qs) = v.get("questions").and_then(|c| c.as_array()) {
        s.push_str("待裁决问题：\n");
        for q in qs {
            s.push_str(&format!("- {}\n", q.as_str().unwrap_or("")));
        }
    }
    if let Some(rule) = v.get("verdict_rule").and_then(|x| x.as_str()) {
        s.push_str(&format!("裁决规则：{rule}\n"));
    }
    s
}

fn finalize(acc: Acc) -> BuiltPrompt {
    let (sections, user, truncated_total) = acc.render();
    let truncated_sections = sections.iter().filter(|s| s.truncated).count();
    let stats = BuildStats {
        section_count: sections.len(),
        total_chars: user.chars().count(),
        truncated_sections,
        truncated_total,
        max_section_chars: MAX_SECTION_CHARS,
        max_total_chars: MAX_TOTAL_CHARS,
    };
    BuiltPrompt {
        system: system_prompt(),
        user,
        sections,
        stats,
    }
}

// ────────────────────────────────── ① inspect-card 探针 → prompt

/// inspect-card 探针包 → 结构化 prompt。
///
/// 输入为 `world_gov::inspect_card` 产出的 `probe_pack`。
pub fn build_inspect_prompt(probe: &Value) -> BuiltPrompt {
    let mut acc = Acc::default();
    let key = s_of(probe, "key");
    let layer = s_of(probe, "layer");
    let found_in = s_of(probe, "found_in");

    // 头部：任务定位（不涉法理层）
    acc.add(
        "检视目标",
        layer,
        key,
        found_in,
        format!(
            "key={key}  layer={layer}  标题：{}\n法理链（本次适用）：{}\n",
            s_of(probe, "title"),
            law_chain_from(probe)
        ),
    );

    // 卡片正文（溯源到卡片本体）
    acc.add(
        "待检视卡片正文",
        layer,
        key,
        found_in,
        s_of(probe, "card_text").to_string(),
    );

    // 候选上层法理（逐条溯源，模型引用时必须回指到具体 key）
    if let Some(laws) = probe.get("candidate_laws").and_then(|c| c.as_array()) {
        for (i, l) in laws.iter().enumerate() {
            let mut body = String::new();
            body.push_str(&format!(
                "优先级 {}  层={}  摘要：{}\n",
                l.get("priority").and_then(|x| x.as_i64()).unwrap_or(0),
                s_of(l, "layer"),
                s_of(l, "basis_snippet")
            ));
            if let Some(bs) = l.get("boundaries").and_then(|c| c.as_array()) {
                for b in bs {
                    body.push_str(&format!("- 边界线索：{}\n", b.as_str().unwrap_or("")));
                }
            }
            acc.add(
                format!("候选上层法理 #{}", i + 1),
                s_of(l, "layer"),
                s_of(l, "key"),
                "world_baseline",
                body,
            );
        }
    }

    // 问题 + 裁决规则
    acc.add("待裁决问题与规则", layer, key, found_in, questions_block(probe));

    finalize(acc)
}

// ────────────────────────────────── ② scan-library 探针 → prompt

/// scan-library 分片清单 → 结构化 prompt。
///
/// 输入为 `world_gov::scan_library` 产出的报告（含 `probe_items` 元数据）。
pub fn build_scan_prompt(report: &Value) -> BuiltPrompt {
    let mut acc = Acc::default();

    let sl = report.get("slice").cloned().unwrap_or(Value::Null);
    acc.add(
        "扫描分片元信息",
        "",
        "",
        "world-gov scan-library",
        format!(
            "分片 {}/{}  偏移 {}  本片 {} 条 / 全库 {} 条\n法理链：{}\n",
            sl.get("n").and_then(|x| x.as_i64()).unwrap_or(0),
            sl.get("m").and_then(|x| x.as_i64()).unwrap_or(0),
            sl.get("offset").and_then(|x| x.as_i64()).unwrap_or(0),
            sl.get("items").and_then(|x| x.as_i64()).unwrap_or(0),
            sl.get("total").and_then(|x| x.as_i64()).unwrap_or(0),
            law_chain_from(report)
        ),
    );

    // 逐条待检条目（溯源到条目 key/来源文件）
    if let Some(items) = report.get("probe_items").and_then(|c| c.as_array()) {
        for (i, it) in items.iter().enumerate() {
            acc.add(
                format!("待检条目 #{}", i + 1),
                s_of(it, "layer"),
                s_of(it, "key"),
                s_of(it, "source"),
                format!(
                    "层={}  key={}  标题：{}  字数：{}\n",
                    s_of(it, "layer"),
                    s_of(it, "key"),
                    s_of(it, "title"),
                    it.get("chars").and_then(|x| x.as_i64()).unwrap_or(0)
                ),
            );
        }
    }

    acc.add("待裁决问题与规则", "", "", "world-gov scan-library", questions_block(report));

    finalize(acc)
}

// ────────────────────────────────── ③ check-outline 探针 → prompt

/// check-outline 卷细纲探针 → 结构化 prompt。
///
/// 输入为 `world_gov::check_outline` 产出的报告。
pub fn build_outline_prompt(report: &Value) -> BuiltPrompt {
    let mut acc = Acc::default();
    let volume = s_of(report, "volume");

    acc.add(
        "卷评估定位",
        "L3",
        volume,
        "world-gov check-outline",
        format!(
            "卷={volume}  L3 细纲文件 {} 个\n法理链：{}\n",
            report.get("files_found").and_then(|x| x.as_i64()).unwrap_or(0),
            law_chain_from(report)
        ),
    );

    // 上层法理基线（逐条溯源）
    if let Some(base) = report.get("upper_law_baseline").and_then(|c| c.as_array()) {
        for (i, b) in base.iter().enumerate() {
            let mut body = String::new();
            body.push_str(&format!("摘要：{}\n", s_of(b, "summary")));
            if let Some(bs) = b.get("boundaries").and_then(|c| c.as_array()) {
                for x in bs {
                    body.push_str(&format!("- 边界线索：{}\n", x.as_str().unwrap_or("")));
                }
            }
            acc.add(
                format!("上层法理基线 #{}", i + 1),
                s_of(b, "layer"),
                s_of(b, "key"),
                "world_baseline",
                body,
            );
        }
    }

    // L3 细纲文件头部（大文件已在探针侧只读 32KB）
    if let Some(files) = report.get("outline_probe_files").and_then(|c| c.as_array()) {
        for (i, f) in files.iter().enumerate() {
            acc.add(
                format!("L3 细纲文件 #{}", i + 1),
                "L3",
                s_of(f, "key"),
                s_of(f, "source"),
                s_of(f, "head_32k").to_string(),
            );
        }
    }

    acc.add("待裁决问题与规则", "L3", volume, "world-gov check-outline", questions_block(report));

    finalize(acc)
}

// ────────────────────────────────── 统一入口（按探针类型分派）

/// 按类型分派：`inspect-card` / `scan-library` / `check-outline`。
///
/// - inspect-card：传 `probe_pack`（非整包）；
/// - 其余：传整份报告。
pub fn build_by_kind(kind: &str, payload: &Value) -> anyhow::Result<BuiltPrompt> {
    match kind {
        "inspect-card" => Ok(build_inspect_prompt(payload)),
        "scan-library" => Ok(build_scan_prompt(payload)),
        "check-outline" => Ok(build_outline_prompt(payload)),
        other => anyhow::bail!(
            "未知探针类型 '{other}'（仅支持 inspect-card | scan-library | check-outline）"
        ),
    }
}

// ────────────────────────────────── 单测

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn inspect_prompt_has_provenance_for_every_section() {
        let probe = json!({
            "key": "CHAR-10-001", "layer": "L4", "title": "测试卡",
            "found_in": "library_index", "source": "04_人物/CHAR-10-001.md",
            "card_text": "这是一段卡片正文，讲一个角色。",
            "law_chain": [{"layer":"L0","name":"总纲"},{"layer":"L4","name":"细节"}],
            "candidate_laws": [
                {"layer":"L0","key":"L0-001","priority":100,"basis_snippet":"宇宙总纲","boundaries":["禁止A"]},
                {"layer":"L1","key":"L1-002","priority":80,"basis_snippet":"世界脊梁","boundaries":[]}
            ],
            "questions": ["自相矛盾？","推翻上层？"],
            "verdict_rule": "无依据不判冲突；拿不准 UNCERTAIN。"
        });
        let b = build_inspect_prompt(&probe);
        // 每段都有溯源标记
        for s in &b.sections {
            assert!(s.provenance_line().contains("layer="), "缺少溯源标记: {}", s.heading);
            assert!(s.provenance_line().contains("key="));
        }
        // 上游法理逐条溯源到具体 key
        let keys: Vec<&str> = b.sections.iter().map(|s| s.key.as_str()).collect();
        assert!(keys.contains(&"L0-001"));
        assert!(keys.contains(&"L1-002"));
        // 正文与问题都进了 user
        assert!(b.user.contains("这是一段卡片正文"));
        assert!(b.user.contains("自相矛盾？"));
        assert!(b.user.contains("［来源"));
        assert!(!b.stats.truncated_total);
    }

    #[test]
    fn scan_prompt_lists_items_with_provenance() {
        let report = json!({
            "slice": {"n": 1, "m": 4, "offset": 0, "items": 2, "total": 684},
            "law_chain": [{"layer":"L0","name":"总纲"}],
            "probe_items": [
                {"layer":"L4","key":"A-1","title":"甲","chars":120,"source":"04/A-1.md"},
                {"layer":"L4","key":"A-2","title":"乙","chars":90,"source":"04/A-2.md"}
            ],
            "questions": ["重复？","违背上层？"],
            "verdict_rule": "严禁幻觉；拿不准 UNCERTAIN。"
        });
        let b = build_scan_prompt(&report);
        assert!(b.user.contains("分片 1/4"));
        assert!(b.user.contains("A-1"));
        assert!(b.user.contains("A-2"));
        assert!(b.sections.iter().any(|s| s.key == "A-1" && s.found_in == "04/A-1.md"));
        assert_eq!(b.stats.section_count, b.sections.len());
    }

    #[test]
    fn outline_prompt_keeps_baseline_and_files() {
        let report = json!({
            "volume": "卷01", "files_found": 1,
            "law_chain": [{"layer":"L0","name":"总纲"},{"layer":"L3","name":"细纲"}],
            "upper_law_baseline": [
                {"layer":"L1","key":"L1-9","summary":"世界脊梁摘要","boundaries":["边界X"]}
            ],
            "outline_probe_files": [
                {"key":"L3-卷01","source":"03_细纲/卷01.md","chars":500000,"title":"卷一细纲","head_present":true,"head_32k":"细纲开头……"}
            ],
            "questions": ["越界？","自洽？","L4支撑？"],
            "verdict_rule": "只出报告不改细纲；拿不准 UNCERTAIN。"
        });
        let b = build_outline_prompt(&report);
        assert!(b.user.contains("L1-9"));
        assert!(b.user.contains("细纲开头"));
        assert!(b.user.contains("越界？"));
    }

    #[test]
    fn section_truncation_is_char_safe_and_marked() {
        // 用中文构造超长正文：按 char 切，绝不能 panic
        let long: String = "中".repeat(MAX_SECTION_CHARS + 500);
        let probe = json!({
            "key": "K", "layer": "L4", "found_in": "lib",
            "card_text": long,
            "questions": [], "verdict_rule": ""
        });
        let b = build_inspect_prompt(&probe);
        let card = b.sections.iter().find(|s| s.heading.contains("卡片正文")).unwrap();
        assert!(card.truncated);
        assert_eq!(card.text.chars().count(), MAX_SECTION_CHARS);
        assert_eq!(card.original_chars, MAX_SECTION_CHARS + 500);
        assert!(b.user.contains("本段已裁剪"));
        assert_eq!(b.stats.truncated_sections, 1);
    }

    #[test]
    fn total_budget_degrades_tail_sections() {
        // 造很多段，累计超过总预算 → 末尾段降级
        let mut items = Vec::new();
        for i in 0..50 {
            items.push(json!({
                "layer":"L4","key":format!("K-{i}"),"title":"x","chars":10,
                "source":"s"
            }));
        }
        let report = json!({
            "slice": {"n":1,"m":1,"offset":0,"items":50,"total":50},
            "probe_items": items,
            "questions": [], "verdict_rule": ""
        });
        // 直接注入超长正文难，改用很多段：这里段短，可能不触发；改为断言统计字段存在
        let b = build_scan_prompt(&report);
        assert!(b.stats.total_chars > 0);
        // 显式构造总超预算场景
        let mut acc = Acc::default();
        for i in 0..6 {
            acc.add(format!("块{i}"), "L4", "k", "s", "中".repeat(MAX_SECTION_CHARS));
        }
        let (_, user, truncated_total) = acc.render();
        assert!(truncated_total, "总量超预算必须触发降级");
        assert!(user.contains("已降级"));
        assert!(user.chars().count() < 6 * MAX_SECTION_CHARS);
    }

    #[test]
    fn build_by_kind_dispatches_and_rejects_unknown() {
        let probe = json!({"key":"k","layer":"L4","found_in":"lib","card_text":"t","questions":[],"verdict_rule":""});
        assert!(build_by_kind("inspect-card", &probe).is_ok());
        let e = build_by_kind("bogus", &probe).unwrap_err();
        assert!(e.to_string().contains("未知探针类型"));
    }

    #[test]
    fn provenance_index_mirrors_sections() {
        let probe = json!({
            "key":"K","layer":"L4","found_in":"lib","card_text":"t",
            "candidate_laws":[{"layer":"L0","key":"L0-1","priority":1,"basis_snippet":"s","boundaries":[]}],
            "questions":[],"verdict_rule":""
        });
        let b = build_inspect_prompt(&probe);
        let idx = b.provenance_index();
        assert_eq!(idx.len(), b.sections.len());
        assert!(idx.iter().any(|v| v["key"] == "L0-1"));
    }

    #[test]
    fn system_prompt_carries_law_chain_and_iron_rules() {
        let s = system_prompt();
        assert!(s.contains("L0 ≫ L0.5 ≫ L1"));
        assert!(s.contains("UNCERTAIN"));
        assert!(s.contains("［来源"));
    }
}
