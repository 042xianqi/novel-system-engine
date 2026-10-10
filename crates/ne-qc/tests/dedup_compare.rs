//! t1：比对两份 `deterministic_check`（固化差异结论，防回归）。
//!
//! A. `ne_qc::deterministic_check`      → 委托 validate_six（2026-10-08 六维框架）
//! B. `ne_engine::chapter::deterministic_check` → 照抄 chapter_engine.py（已与 Python 对拍）
//!
//! **实测结论（2026-10-10）**：两者**语义不等价**，且 ne-qc 更宽松。
//! 用干净的 3000 句号做输入时：
//!   * Python 权威实现 → reject（"最长段 3000 字 >200" P0 + "平均句长1.0<22" P1）
//!   * ne-engine（Rust 照抄）→ reject（同上，逐字一致）
//!   * ne-qc（六维框架）→ Pass（**丢了段落粒度与断句节奏两维**）
//!
//! ⇒ **2026-10-10 已合并**：`ne_qc::deterministic_check` 现委托
//!   `ne_engine::chapter::deterministic_check`（与 Python 逐值对拍零差异），
//!   口径已统一。`validate_six` 保留其 Packet 驱动的独有能力。
//!   本文件中的「ne-qc 更宽松」断言已随之更新为「两者一致」。

use serde_json::json;

fn text_of(n: usize, extra: &str) -> String {
    let mut s: String = std::iter::repeat('。').take(n).collect();
    s.push_str(extra);
    s
}

/// 输入「纯标点无段落」→ 合并后三者（Python / ne-engine / ne-qc）都判 reject。
#[test]
fn merged_deterministic_check_matches_python_on_no_paragraph_text() {
    let t = text_of(3000, "");
    let a = ne_qc::deterministic_check(&t, 3);
    let b = ne_engine::chapter::deterministic_check(&t, 3);

    println!("[clean] ne-qc    verdict={:?} findings={}", a.verdict, a.findings.len());
    println!("[clean] ne-eng   verdict={} p0={:?}", b.verdict,
             b.p0.iter().map(|f| &f.dim).collect::<Vec<_>>());

    // 合并后：两者都应 reject（口径统一）
    assert_eq!(b.verdict, "reject");
    assert!(b.p0.iter().any(|f| f.dim == "B.段落"));
    assert_eq!(format!("{:?}", a.verdict), "Reject", "合并后应与 ne-engine 一致");
}

/// 禁词：两边一致（都 reject）——共同维度。
#[test]
fn both_reject_on_banned_word() {
    let t = text_of(3000, "归墟");
    let a = ne_qc::deterministic_check(&t, 3);
    let b = ne_engine::chapter::deterministic_check(&t, 3);
    assert_eq!(format!("{:?}", a.verdict), "Reject");
    assert_eq!(b.verdict, "reject");
}

/// AI 痕迹词：ne-qc=Fix，ne-engine=reject（因段落 P0 先命中）。
/// 关键：ne-engine 的 AI 词确实被检出（同为 p1/p1 级）。
#[test]
fn ai_words_detected_by_both() {
    let t = text_of(3000, "不禁 竟然 缓缓");
    let a = ne_qc::deterministic_check(&t, 3);
    let b = ne_engine::chapter::deterministic_check(&t, 3);
    assert_eq!(a.stats.ai_trace_hits, 3);
    assert_eq!(b.stats["ai_words"], json!(3));
    // 合并后：a 也走 ne-engine 口径 → 段落 P0 压过 AI 词 P1 → reject
    assert_eq!(format!("{:?}", a.verdict), "Reject");
    assert_eq!(b.verdict, "reject");
}

/// 有段落的文本：ne-qc=Pass，但 **Python 权威实现 = fix**（段数 87 > 25）。
/// ⇒ 再次证明 ne-qc 比 Python 宽松：它不做段落粒度/断句节奏两维。
/// 本测试的作用是**固定这个偏差**，并证明 ne-engine 与 Python 一致。
#[test]
fn merged_check_recovers_paragraph_granularity() {
    let para = "他走进屋子，看着桌上的灯。那灯很旧。他没有说话。窗外风很大。";
    let mut t = String::new();
    while t.chars().filter(|c| !c.is_whitespace()).count() < 2600 {
        t.push_str(para);
        t.push_str("\n\n");
    }
    let a = ne_qc::deterministic_check(&t, 3);
    let b = ne_engine::chapter::deterministic_check(&t, 3);

    println!("[wellformed] ne-qc={:?}", a.verdict);
    println!("[wellformed] ne-eng={} p1={:?}", b.verdict,
             b.p1.iter().map(|f| &f.dim).collect::<Vec<_>>());

    // Python 权威输出 = fix（段数 87>25 + 平均句长 7.5<22）
    assert_eq!(b.verdict, "fix");
    assert!(b.p1.iter().any(|f| f.dim == "B.段落"));
    assert!(b.p1.iter().any(|f| f.dim == "B.断句节奏"));
    // 合并后：a 也应 fix，且带上这两维
    assert_eq!(format!("{:?}", a.verdict), "Fix", "合并后应与 Python 一致");
    assert!(a.findings.iter().any(|f| f.dim == "B.段落"), "合并后应含段落维度");
}

/// ne-qc 独有能力：注入包驱动的资料引用软检（ne-engine 无此概念）。
#[test]
fn ne_qc_only_has_packet_driven_refs_check() {
    let t = text_of(2600, "\n\n他站着。");
    let extra_banned = vec!["自定义禁词".to_string()];
    let r = ne_qc::validate_six(&t, None, &extra_banned);
    // 无 packet 时，refs 维度不该出现（只有 timeline/LLM 接缝）
    let dims: Vec<&str> = r.findings.iter().map(|f| f.dim.as_str()).collect();
    println!("dims = {dims:?}");
    assert!(!dims.contains(&"refs"), "无 packet 不应有 refs 维度");
    assert!(dims.contains(&"timeline"), "接缝占位应存在");
}

/// **关键回归**：`gate14` 不再是 stub。
/// 旧 stub 无条件返回 Pass —— 本测试证明现在**真的会判 reject**。
#[test]
fn gate14_is_no_longer_a_stub() {
    // 造一段必然触发 G12（U0-1 禁词「传说」）的文本
    let text = format!("{}传说{}", "字".repeat(2600), "。");
    let r = ne_qc::gate14(&text, 3, "卷01");
    println!("verdict={:?} findings={}", r.verdict, r.findings.len());
    for f in &r.findings {
        println!("  [{:?}] {} | {}", f.severity, f.dim, f.issue.chars().take(60).collect::<String>());
    }
    assert_ne!(format!("{:?}", r.verdict), "Pass", "旧 stub 恒 Pass；真实现必须能判非 Pass");
    assert!(
        r.findings.iter().any(|f| f.dim.contains("G12")),
        "应命中 G12 命名阶位门禁；实际 dims={:?}",
        r.findings.iter().map(|f| &f.dim).collect::<Vec<_>>()
    );
}

/// 干净文本应能 Pass（证明不是**恒** reject）——需满足 G01 开篇断言（ch<=3）。
#[test]
fn gate14_can_actually_pass() {
    // 满足 G01：A 陈寂 / B 推演·锚点·刻痕·面板 / C 残响·代价 / D 穿越·这具身体
    let mut t = String::from(
        "陈寂看着推演面板上浮现的锚点刻痕。他穿越到这具身体已经三个月。\n\
         残响在他耳边低语，他知道代价正在积累。但那盏灯还是很旧，他没有说话。\n\n",
    );
    while t.chars().filter(|c| !c.is_whitespace()).count() < 2600 {
        t.push_str("他坐下，拿起一本书，翻开第一页，慢慢读着。灯影在墙上晃动。\n\n");
    }
    t.push_str("他合上书，忽然想到一个问题：那只手究竟是谁的？");
    let r = ne_qc::gate14(&t, 3, "卷01");
    println!("verdict={:?} n_findings={}", r.verdict, r.findings.len());
    for f in &r.findings {
        println!("  [{:?}] {} | {}", f.severity, f.dim, f.issue.chars().take(55).collect::<String>());
    }
    // 无 P0 即算通过（P1 允许存在）
    assert!(
        !r.findings.iter().any(|f| f.severity == ne_qc::Severity::P0),
        "不该有 P0；实际 P0 dims={:?}",
        r.findings.iter().filter(|f| f.severity == ne_qc::Severity::P0)
            .map(|f| &f.dim).collect::<Vec<_>>()
    );
}
