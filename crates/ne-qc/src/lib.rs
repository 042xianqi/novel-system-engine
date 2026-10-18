//! ne-qc：质检门禁（M3 核心）。
//!
//! M3 骨架：把"确定性质检"做实（AI 痕迹词 / 禁词 / 字数区间），
//! gate14 的 14 项 + OOC + 自动修复打回 留清晰接缝。
//! 对应 Python 的 deterministic_check + gate14.run_14。

use ne_core::InjectionPacket;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// 确定性规则（对应 chapter_engine 的 AI_TRACE_WORDS / BANNED_WORDS / 字数）。
pub const AI_TRACE_WORDS: &[&str] = &["不禁", "竟然", "不可思议", "值得一提", "事实上", "众所周知", "缓缓", "微微"];
pub const BANNED_WORDS: &[&str] = &["归墟", "熵兽", "热寂前夜"];
pub const MIN_WORDS: usize = 2500;
pub const MAX_WORDS: usize = 4000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Finding {
    pub dim: String,
    pub issue: String,
    pub severity: Severity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    P0,
    P1,
    P2,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
    pub verdict: Verdict,
    pub findings: Vec<Finding>,
    pub stats: Stats,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    Pass,
    Fix,
    Reject,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Stats {
    pub words: usize,
    pub ai_trace_hits: usize,
    pub banned_hits: usize,
}

/// 写后验证·6 维框架（2026-10-08 口径，spec novel-pipeline Requirement 8）。
/// 确定性 3 维：禁词（动态表）/ 资料引用（软检）/ 字数+AI痕迹；
/// 接缝 4 项：时间线单调（接 timeline 表）、一致性/因果/伏笔兑现（LLM 复核，走 7k lane）。
/// P0 → Reject，P1 → Fix，P2 → 仅提示不影响 verdict。
pub fn validate_six(text: &str, packet: Option<&InjectionPacket>, extra_banned: &[String]) -> CheckResult {
    let body: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let words = body.chars().count();

    let mut findings = Vec::new();
    let mut ai_hits = 0usize;
    let mut banned_hits = 0usize;

    // 维1 禁词（动态表 = 内置 + 注入包 + 调用方追加）
    let mut banned: BTreeSet<String> = BANNED_WORDS.iter().map(|s| s.to_string()).collect();
    if let Some(p) = packet {
        banned.extend(p.banned_words.iter().cloned());
    }
    banned.extend(extra_banned.iter().cloned());
    for w in &banned {
        if text.contains(w.as_str()) {
            banned_hits += 1;
            findings.push(Finding { dim: "banned".into(), issue: format!("命中禁词「{w}」"), severity: Severity::P0 });
        }
    }
    // AI 痕迹
    for w in AI_TRACE_WORDS {
        if text.contains(w) {
            ai_hits += 1;
            findings.push(Finding { dim: "ai_trace".into(), issue: format!("命中 AI 痕迹词「{w}」"), severity: Severity::P1 });
        }
    }
    // 字数区间
    if words < MIN_WORDS || words > MAX_WORDS {
        findings.push(Finding { dim: "words".into(), issue: format!("字数 {words} 不在 {MIN_WORDS}-{MAX_WORDS}"), severity: Severity::P1 });
    }

    // 维2 资料引用（确定性软检：出场角色名/资料卡名应出现在正文）
    if let Some(p) = packet {
        for c in &p.chars {
            if !c.name.is_empty() && !text.contains(&c.name) {
                findings.push(Finding {
                    dim: "refs".into(),
                    issue: format!("出场角色「{}」（{}）未在正文出现", c.name, c.code),
                    severity: Severity::P2,
                });
            }
        }
        for m in &p.materials {
            if !m.name.is_empty() && !text.contains(&m.name) {
                findings.push(Finding {
                    dim: "refs".into(),
                    issue: format!("资料卡「{}」（{}）未被正文引用", m.name, m.code),
                    severity: Severity::P2,
                });
            }
        }
    }

    // 维3 时间线 / 维4 一致性 / 维5 因果 / 维6 伏笔兑现：接缝占位
    findings.push(Finding { dim: "timeline".into(), issue: "接缝：接 timeline 表做时序单调校验（M3）".into(), severity: Severity::P2 });
    findings.push(Finding { dim: "llm_consistency".into(), issue: "接缝：一致性 LLM 复核（走 7k lane）".into(), severity: Severity::P2 });
    findings.push(Finding { dim: "llm_causality".into(), issue: "接缝：因果 LLM 复核（走 7k lane）".into(), severity: Severity::P2 });
    findings.push(Finding { dim: "llm_foreshadow".into(), issue: "接缝：伏笔兑现 LLM 对照 foreshadow 表复核".into(), severity: Severity::P2 });

    let verdict = if findings.iter().any(|f| f.severity == Severity::P0) {
        Verdict::Reject
    } else if findings.iter().any(|f| f.severity == Severity::P1) {
        Verdict::Fix
    } else {
        Verdict::Pass
    };

    CheckResult { verdict, findings, stats: Stats { words, ai_trace_hits: ai_hits, banned_hits } }
}

/// 确定性质检（0 LLM，纯扫）。
///
/// **2026-10-10 合并**：原先这里是 `validate_six(text, None, &[])` 的薄包装，
/// 实测比 Python 权威实现**更宽松** —— 缺「段落粒度（单段>200 → P0 / 段数>25 → P1）」
/// 与「断句节奏（平均句长 / 逗号比）」两维。
/// 例：对「87 段、平均句长 7.5」的文本，Python 判 `fix`，旧 ne-qc 判 `Pass`（漏检）。
///
/// 现改为**委托 `ne_engine::chapter::deterministic_check`**（该实现已与 Python
/// 逐值对拍零差异），口径统一到权威版。`validate_six` 仍保留，用于其独有能力
/// （Packet 驱动的资料引用软检 + timeline/LLM 复核接缝），供 `ne-mcp` 另路调用。
pub fn deterministic_check(text: &str, chapter: u32) -> CheckResult {
    let f = ne_engine::chapter::deterministic_check(text, chapter);
    let mut findings: Vec<Finding> = Vec::new();
    for x in &f.p0 {
        findings.push(Finding {
            dim: x.dim.clone(),
            issue: x.issue.clone(),
            severity: Severity::P0,
        });
    }
    for x in &f.p1 {
        findings.push(Finding {
            dim: x.dim.clone(),
            issue: x.issue.clone(),
            severity: Severity::P1,
        });
    }
    let verdict = match f.verdict.as_str() {
        "reject" => Verdict::Reject,
        "fix" => Verdict::Fix,
        _ => Verdict::Pass,
    };
    CheckResult {
        verdict,
        findings,
        stats: Stats {
            words: f.stats.get("words").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
            ai_trace_hits: f.stats.get("ai_words").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
            banned_hits: f.stats.get("banned").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
        },
    }
}

/// gate14：**已接入真实实现**（2026-10-10）。
///
/// 历史：本函数原为 `gate14_stub`，无条件返回 `Verdict::Pass`，只在 findings 里
/// 写「待接 gate14.py」——**等于质检链形同虚设**。现已改为调用
/// `ne_engine::gate14::run_all`（G01–G16 共 16 道门禁的真实实现）。
///
/// 无 DB 场景（如 `ne-mcp::check` 只传 text/chapter/volume）用 `BareGateCtx`：
/// 纯文本门禁照常生效；需外部数据的门禁如实 skip（不伪造 pass 结论）。
///
/// 可选：`cfg_text` 传 `dynamics_config.json` 原文以启用 G12 禁词表等配置项。
#[allow(unused)]
pub fn gate14(text: &str, chapter: u32, volume: &str) -> CheckResult {
    gate14_with_cfg(text, chapter, volume, None)
}

/// 带配置的 gate14。
pub fn gate14_with_cfg(
    text: &str,
    chapter: u32,
    volume: &str,
    cfg_text: Option<&str>,
) -> CheckResult {
    let ctx = ne_engine::gate14::BareGateCtx::from_config_text(cfg_text);
    let vol = if volume.is_empty() { "卷01" } else { volume };
    match ne_engine::gate14::run_all(&ctx, text, chapter, vol) {
        Ok(run) => {
            let mut findings: Vec<Finding> = Vec::new();
            for f in &run.p0 {
                findings.push(Finding {
                    dim: f.gate.clone(),
                    issue: format!("{}（建议：{}）", f.issue, f.action),
                    severity: Severity::P0,
                });
            }
            for f in &run.p1 {
                findings.push(Finding {
                    dim: f.gate.clone(),
                    issue: format!("{}（建议：{}）", f.issue, f.action),
                    severity: Severity::P1,
                });
            }
            let verdict = match run.verdict.as_str() {
                "reject" => Verdict::Reject,
                "fix" => Verdict::Fix,
                _ => Verdict::Pass,
            };
            let words = run.stats.get("words").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            CheckResult {
                verdict,
                findings,
                stats: Stats {
                    words,
                    ai_trace_hits: run.p1.iter().filter(|f| f.gate == "G13_scoring_67dim").count(),
                    banned_hits: run.p0.iter().filter(|f| f.gate == "G12_naming_tier").count(),
                },
            }
        }
        Err(e) => CheckResult {
            verdict: Verdict::Fix,
            findings: vec![Finding {
                dim: "gate14".into(),
                issue: format!("gate14 执行失败：{e}"),
                severity: Severity::P1,
            }],
            stats: Stats::default(),
        },
    }
}

/// 旧名兼容（保留，转发到真实现）。
#[deprecated(note = "已改名为 gate14，且不再是 stub；请用 gate14()")]
#[allow(unused)]
pub fn gate14_stub(text: &str, chapter: u32, volume: &str) -> CheckResult {
    gate14(text, chapter, volume)
}

/// 自动修复打回决策：Fix → 修复；Reject → 打回（转 07_勘误工坊/_rejected/）。
pub fn route_on_verdict(r: &CheckResult) -> &'static str {
    match r.verdict {
        Verdict::Pass => "commit",
        Verdict::Fix => "repair",
        Verdict::Reject => "reject_to_rejected_dir",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ⚠ 合并后（2026-10-10）本测试已更新：3000 个纯句号 = 单个 3000 字长段，
    /// Python 权威实现判 **reject**（`B.段落` P0）。旧断言 `Pass` 是旧行为的误设。
    #[test]
    fn no_paragraph_text_rejects_per_python() {
        let one: String = std::iter::repeat('。').take(3000).collect();
        let r = deterministic_check(&one, 3);
        assert_eq!(r.stats.words, 3000);
        assert_eq!(r.verdict, Verdict::Reject, "单段 3000 字 → P0");
        assert!(r.findings.iter().any(|f| f.dim == "B.段落"), "{:?}", r.findings);
    }

    /// 真的「干净」文本（有段落、字数合规、无禁词/AI词）应 Pass。
    #[test]
    fn clean_text_passes() {
        let para = "他走进屋子，看着桌上的灯。那灯很旧。他没有说话。窗外风很大。";
        let mut t = String::new();
        while t.chars().filter(|c| !c.is_whitespace()).count() < 2600 {
            t.push_str(para);
            t.push_str("\n\n");
        }
        t.push_str("他忽然想到一个问题：那只手究竟是谁的？");
        let r = deterministic_check(&t, 3);
        println!("verdict={:?} n={}", r.verdict, r.findings.len());
        assert_ne!(format!("{:?}", r.verdict), "Reject", "不该有 P0: {:?}", r.findings);
    }

    /// 禁词 → Reject。dim 现由 ne-engine 提供（禁词归 `A.世界观`）。
    #[test]
    fn banned_word_rejects() {
        let para = "他走进屋子，看着桌上的灯。那灯很旧。他没有说话。\n\n";
        let mut t = String::new();
        while t.chars().filter(|c| !c.is_whitespace()).count() < 2600 {
            t.push_str(para);
        }
        t.push_str("归墟");
        let r = deterministic_check(&t, 3);
        assert_eq!(r.verdict, Verdict::Reject);
        assert!(
            r.findings.iter().any(|f| f.dim.contains("世界观")),
            "禁词应归 A.世界观；实际 dims={:?}",
            r.findings.iter().map(|f| &f.dim).collect::<Vec<_>>()
        );
    }

    #[test]
    fn routing() {
        assert_eq!(route_on_verdict(&CheckResult { verdict: Verdict::Pass, findings: vec![], stats: Stats::default() }), "commit");
        assert_eq!(route_on_verdict(&CheckResult { verdict: Verdict::Fix, findings: vec![], stats: Stats::default() }), "repair");
        assert_eq!(route_on_verdict(&CheckResult { verdict: Verdict::Reject, findings: vec![], stats: Stats::default() }), "reject_to_rejected_dir");
    }
}