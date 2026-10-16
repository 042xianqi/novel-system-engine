use ne_engine::config;

#[test]
fn ch1_final_gate_check() {
    let lib = std::path::Path::new("/sdcard/Download/小说相关备份/projects/projects/系统/资料库");
    let text = std::fs::read_to_string(lib.join("04_正文/卷01/第1章.md")).unwrap();
    let cfg = config::load(lib).unwrap();
    let ctx = ne_engine::ctx::RepoCtx::open(lib).unwrap();
    let report = ne_engine::gates::run_all(&text, 1, "卷01", &cfg, &ctx);
    println!("verdict={}", report.verdict);
    for f in &report.p0 { println!("P0 {}: {}", f.gate, f.issue); }
    for f in &report.p1 { println!("P1 {}: {}", f.gate, f.issue); }
    // 断言：无 P0（不强制 P1 清零——G13 库规模属库数据问题，非正文问题）
    assert!(report.p0.is_empty(), "存在 P0 打回项");
    let hard_p1: Vec<_> = report
        .p1
        .iter()
        .filter(|f| f.gate.starts_with("G02") || f.gate.starts_with("G07"))
        .collect();
    assert!(hard_p1.is_empty(), "G02/G07 仍有硬伤：{hard_p1:?}");
    // 应用 P1-PARAGRAPH 排版修复（仅分段，一字不改），与 novel 落盘行为一致
    if let Some(fixed) = &report.fixed_text {
        assert_ne!(fixed, &text, "fixed_text 与原文相同却非 None？");
        std::fs::write(lib.join("04_正文/卷01/第1章.md"), fixed).unwrap();
        println!("已回写排版修复版");
    }
}