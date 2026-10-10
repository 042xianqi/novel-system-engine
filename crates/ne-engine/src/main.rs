//! ne-engine CLI —— 全量 Rust 化的质检与推进入口。
//!
//! 用法：
//!   ne-engine check  --text <文件> [--chapter N] [--volume 卷01] [--root DIR]
//!   ne-engine check-file <章节md> [--chapter N] [--volume 卷01] [--root DIR]
//!   ne-engine gates                     # 门禁清单
//!   ne-engine selftest --root DIR       # 自检（重放性 + 门禁数）
//!   ne-engine llm-probe [--root DIR]    # 只用配置探测 LLM 端点连通性（不发正文）

use anyhow::{Context, Result};
use ne_engine::{config, gates, llm, RepoCtx};
use std::path::{Path, PathBuf};

fn arg(args: &[String], key: &str) -> Option<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == key {
            return it.next().cloned();
        }
        if let Some(r) = a.strip_prefix(&format!("{key}=")) {
            return Some(r.to_string());
        }
    }
    None
}

fn resolve_root(args: &[String]) -> Result<PathBuf> {
    let start = arg(args, "--root")
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let mut d = start.clone();
    for _ in 0..8 {
        if d.join("02_资料库").is_dir() {
            return Ok(d);
        }
        if !d.pop() {
            break;
        }
    }
    anyhow::bail!("在 {} 向上找不到含 `02_资料库` 的仓库根", start.display())
}

fn do_check(root: &Path, text: &str, chapter: u32, volume: &str) -> Result<()> {
    let cfg = config::load(root)?;
    let ctx = RepoCtx::open(root)?;
    let r = gates::run_all(text, chapter, volume, &cfg, &ctx);
    println!("verdict = {}", r.verdict);
    println!("字数 = {}  门禁 = {}", r.stats.words, r.stats.gates_run);
    println!();
    println!("{:<26} {:<6} {}", "门禁", "通过", "说明");
    println!("{}", "-".repeat(96));
    for (name, g) in &r.gates {
        println!(
            "{:<26} {:<6} {}",
            name,
            if g.pass { "✓" } else { "✗" },
            g.detail.chars().take(60).collect::<String>()
        );
    }
    if !r.p0.is_empty() {
        println!("\n─── P0（打回，不入库）───");
        for f in &r.p0 {
            println!("  [{}] {}\n      → {}", f.gate, f.issue, f.action);
        }
    }
    if !r.p1.is_empty() {
        println!("\n─── P1（提示/可修）───");
        for f in &r.p1 {
            println!("  [{}] {}\n      → {}", f.gate, f.issue, f.action);
        }
    }
    let code = if r.verdict == "reject" { 1 } else { 0 };
    std::process::exit(code);
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("gates");

    match cmd {
        "check" => {
            let root = resolve_root(&args)?;
            let path = arg(&args, "--text").context("check 需要 --text <文件>")?;
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("读不到 {path}"))?;
            let chapter: u32 = arg(&args, "--chapter").and_then(|s| s.parse().ok()).unwrap_or(0);
            let volume = arg(&args, "--volume").unwrap_or_else(|| "卷01".into());
            do_check(&root, &text, chapter, &volume)
        }
        "check-file" => {
            let root = resolve_root(&args)?;
            let path = args.get(2).context("check-file 需要 <章节md 路径>")?.clone();
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("读不到 {path}"))?;
            let chapter: u32 = arg(&args, "--chapter")
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(|| infer_chapter(&path));
            let volume = arg(&args, "--volume").unwrap_or_else(|| infer_volume(&path));
            do_check(&root, &text, chapter, &volume)
        }
        "gates" => {
            println!("16 门禁（Rust 版，逐条移植 engine/gate14.py）\n");
            for (i, g) in gates::GATE_COUNT_ORDER.iter().enumerate() {
                println!("  {:2}. {}", i + 1, g);
            }
            println!("\n共 {} 门。verdict：有 P0→reject｜仅 P1→fix｜无→pass", gates::GATE_COUNT_ORDER.len());
            Ok(())
        }
        "selftest" => {
            let root = resolve_root(&args)?;
            let cfg = config::load(&root)?;
            let ctx = RepoCtx::open(&root)?;
            let sample = "陈寂睁开眼，冰凉的水灌进领口。锚点已建立。这具身体不是他的。\
                          他想起残响之河，心头一沉。岸边，他问：到底会怎样？";
            let r1 = gates::run_all(sample, 1, "卷01", &cfg, &ctx);
            let r2 = gates::run_all(sample, 1, "卷01", &cfg, &ctx);
            let j1 = serde_json::to_string(&r1)?;
            let j2 = serde_json::to_string(&r2)?;
            let replay = j1 == j2;
            println!("门禁数 = {}", r1.stats.gates_run);
            println!("verdict = {}", r1.verdict);
            println!("重放一致 = {replay}");
            println!("配置 word_max = {}", cfg.word_max());
            let ok = replay && r1.stats.gates_run == 16;
            println!("selftest = {}", if ok { "PASS" } else { "FAIL" });
            if !ok {
                std::process::exit(1);
            }
            Ok(())
        }
        "llm-probe" => {
            let root = resolve_root(&args)?;
            let p = llm::LlmParams {
                model: arg(&args, "--model").unwrap_or_else(|| "deepseek-chat".into()),
                ..Default::default()
            };
            println!("端点 = {}/chat/completions", p.base_url.trim_end_matches('/'));
            println!("key 数 = {}（未配置则不可调用）", p.keys.len());
            println!("注：本命令只做配置展示，不发起真实请求（避免误烧额度）。");
            let _ = root;
            Ok(())
        }
        other => {
            eprintln!("未知子命令：{other}");
            eprintln!("可用：check | check-file | gates | selftest | llm-probe");
            std::process::exit(2);
        }
    }
}

/// 从路径推断章号（第N章.md）
fn infer_chapter(p: &str) -> u32 {
    let re = regex::Regex::new(r"第(\d+)章").unwrap();
    re.captures(p).and_then(|c| c.get(1)).and_then(|m| m.as_str().parse().ok()).unwrap_or(0)
}
/// 从路径推断卷号（卷NN）
fn infer_volume(p: &str) -> String {
    let re = regex::Regex::new(r"(卷\d+)").unwrap();
    re.captures(p).and_then(|c| c.get(1)).map(|m| m.as_str().to_string()).unwrap_or_else(|| "卷01".into())
}
