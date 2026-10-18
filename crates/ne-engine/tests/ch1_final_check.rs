//! 第 1 章终局门禁检查（集成测试，依赖真实资料库数据）。
//!
//! 【2026-10-18 修】原实现硬编码 `/sdcard/Download/...` 并用 `.unwrap()`，
//! 在 Linux CI / 容器里该路径不存在 → 直接 panic → CI 必挂。
//! 现改为：
//!   · 优先读 NOVEL_LIB_ROOT 环境变量定位资料库
//!   · 找不到资料库或第 1 章时 **跳过**（eprintln + return），不判失败
//!   · 真跑时设置 NOVEL_LIB_ROOT=/path/to/资料库 即可

use ne_engine::config;

/// 定位资料库根目录。
fn lib_root() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("NOVEL_LIB_ROOT") {
        let pb = std::path::PathBuf::from(p);
        if pb.join("novel.db").exists() {
            return Some(pb);
        }
    }
    let default =
        std::path::Path::new("/sdcard/Download/小说相关备份/projects/projects/系统/资料库");
    if default.join("novel.db").exists() {
        return Some(default.to_path_buf());
    }
    None
}

#[test]
fn ch1_final_gate_check() {
    // 【2026-10-18 修】本测试依赖真实资料库 + 第 1 章正文 + 章节质量达标。
    // 三个条件任一不满足，CI 就会红——但这不是代码缺陷，是环境/数据问题。
    //
    // 语义修正：**默认跳过**，只有显式设置 NOVEL_LIB_ROOT 才真跑。
    // 这样 CI（无 /sdcard）稳定通过；本机要验收时手动开跑：
    //     NOVEL_LIB_ROOT=/sdcard/.../资料库 cargo test -p ne-engine --test ch1_final_check
    let lib = match std::env::var("NOVEL_LIB_ROOT") {
        Ok(p) => {
            let pb = std::path::PathBuf::from(p);
            if !pb.join("novel.db").exists() {
                eprintln!("SKIP: NOVEL_LIB_ROOT={} 下没有 novel.db", pb.display());
                return;
            }
            pb
        }
        Err(_) => {
            eprintln!("SKIP: 未设置 NOVEL_LIB_ROOT（依赖真实数据的集成测试默认跳过）");
            eprintln!("      要真跑：NOVEL_LIB_ROOT=/path/to/资料库 cargo test ...");
            return;
        }
    };

    let ch1_path = lib.join("04_正文/卷01/第1章.md");
    let text = match std::fs::read_to_string(&ch1_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("SKIP: 读不到 {}（{e}）", ch1_path.display());
            return;
        }
    };

    let cfg = config::load(&lib).expect("config::load 失败");
    let ctx = ne_engine::ctx::RepoCtx::open(&lib).expect("RepoCtx::open 失败");

    let report = ne_engine::gates::run_all(&text, 1, "卷01", &cfg, &ctx);
    println!("verdict={}", report.verdict);
    for f in &report.p0 {
        println!("P0 {}: {}", f.gate, f.issue);
    }
    for f in &report.p1 {
        println!("P1 {}: {}", f.gate, f.issue);
    }

    // 断言：无 P0（不强制 P1 清零——G13 库规模属库数据问题，非正文问题）
    assert!(report.p0.is_empty(), "存在 P0 打回项");

    let hard_p1: Vec<_> = report
        .p1
        .iter()
        .filter(|f| f.gate.starts_with("G02") || f.gate.starts_with("G07"))
        .collect();
    assert!(hard_p1.is_empty(), "G02/G07 仍有硬伤：{hard_p1:?}");

    // 应用 P1-PARAGRAPH 排版修复（仅分段，一字不改），与 novel 落盘行为一致。
    // 【注意】这一步会**写回**第 1 章文件，属破坏性操作，仅在显式设置
    // NOVEL_LIB_ROOT 时才可能走到，避免误改 /sdcard 上的正式数据。
    if std::env::var("NOVEL_LIB_ROOT").is_ok() {
        if let Some(fixed) = &report.fixed_text {
            assert_ne!(fixed, &text, "fixed_text 与原文相同却非 None？");
            std::fs::write(&ch1_path, fixed).expect("回写第 1 章失败");
            println!("已回写排版修复版");
        }
    } else {
        if report.fixed_text.is_some() {
            println!("（自动回写已禁用：未设置 NOVEL_LIB_ROOT，避免误改正式数据）");
        }
    }
}