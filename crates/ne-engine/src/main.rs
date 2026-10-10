//! ne-engine CLI —— 全量 Rust 化的质检与推进入口。
//!
//! 用法：
//!   ne-engine check  --text <文件> [--chapter N] [--volume 卷01] [--root DIR]
//!   ne-engine check-file <章节md> [--chapter N] [--volume 卷01] [--root DIR]
//!   ne-engine gates                     # 门禁清单
//!   ne-engine selftest --root DIR       # 自检（重放性 + 门禁数）
//!   ne-engine llm-probe [--root DIR]    # 读 LLM 配置（不烧额度）
//!   ne-engine ai-probe  [--root DIR] [--model M] [--prompt P]  # 真发一次请求

use anyhow::{Context, Result};
use ne_engine::{audit, config, gates, llm, providers, RepoCtx};
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
            let d = providers::resolve_llm_dir(Some(&root));
            println!("配置目录 = {}", d.display());
            let (provs, roles) = providers::load_from_dir(&d)?;
            for (k, v) in &provs.map {
                println!(
                    "  [{k}] baseUrl={} keys={} models={:?}",
                    if v.base_url.is_empty() { "(未设)" } else { &v.base_url },
                    v.keys.len(),
                    v.models
                );
            }
            println!("  modelRoles.default = {:?}", roles.get("default"));
            let p = providers::build_params(&provs, &roles, None);
            println!("→ 选用 baseUrl = {}", p.base_url);
            println!("→ 选用 model   = {}", if p.model.is_empty() { "(未定)" } else { &p.model });
            println!("→ 可用 key 数  = {}", p.keys.len());
            if p.keys.is_empty() {
                println!("⚠ 没有可用 key：请在 {} 配置 providers.<name>.task.keyPool", d.display());
            }
            println!("注：本命令只读配置不烧额度；要真发请求请用 `ai-probe`。");
            Ok(())
        }
        "audit" => {
            // 用 20 类模板审计存量库，输出覆盖率与缺陷清单。
            let root = resolve_root(&args)?;
            let lib = root.join("02_资料库");
            let rep = audit::audit_library(&lib)?;
            if args.iter().any(|a| a == "--json") {
                println!("{}", serde_json::to_string_pretty(&rep)?);
            } else {
                println!("审计根 = {}", rep.lib_root);
                println!("卡片总数 = {}  完全合规 = {}", rep.total_cards, rep.ok_cards);
                println!();
                println!("{:<12} {:>5} {:>5}   字段覆盖率", "类别", "总数", "合规");
                for c in &rep.categories {
                    let cov: Vec<String> = c
                        .field_hits
                        .iter()
                        .map(|(k, v)| format!("{}={}%", k, v * 100 / c.total.max(1)))
                        .collect();
                    println!(
                        "{:<12} {:>5} {:>5}   {}",
                        c.category, c.total, c.ok,
                        cov.join(" ")
                    );
                }
                // 缺陷汇总
                let mut issue_cnt: std::collections::BTreeMap<&str, usize> =
                    std::collections::BTreeMap::new();
                for card in &rep.cards {
                    for i in &card.issues {
                        let key = i.split([':', '=']).next().unwrap_or(i);
                        *issue_cnt.entry(key).or_insert(0) += 1;
                    }
                }
                if !issue_cnt.is_empty() {
                    println!();
                    println!("结构缺陷汇总：");
                    let mut v: Vec<_> = issue_cnt.into_iter().collect();
                    v.sort_by(|a, b| b.1.cmp(&a.1));
                    for (k, n) in v {
                        println!("  {:<20} {n}", k);
                    }
                }
            }
            Ok(())
        }
        "ai-probe" => {
            // 真发一次最小请求，验证 LLM 链路端到端可用。
            let root = resolve_root(&args)?;
            let d = providers::resolve_llm_dir(Some(&root));
            let (provs, roles) = providers::load_from_dir(&d)?;
            let mut p = providers::build_params(&provs, &roles, None);
            if let Some(m) = arg(&args, "--model") {
                p.model = m;
            }
            if p.keys.is_empty() {
                anyhow::bail!("没有可用 key（配置目录 {}）", d.display());
            }
            if p.model.is_empty() {
                anyhow::bail!("未指定模型（config.yml 的 modelRoles.default 或 --model）");
            }
            let prompt = arg(&args, "--prompt")
                .unwrap_or_else(|| "只回四个字：接通成功".to_string());
            println!("→ 端点 = {}/chat/completions", p.base_url.trim_end_matches('/'));
            println!("→ 模型 = {}", p.model);
            println!("→ key  = {} 把（不打印内容）", p.keys.len());
            let msgs = vec![llm::Message {
                role: "user".into(),
                content: prompt.clone(),
            }];
            let t0 = std::time::Instant::now();
            match llm::call(&p, &msgs) {
                Ok(o) => {
                    println!("✅ 调用成功（{} ms）", t0.elapsed().as_millis());
                    println!("   prompt = {prompt}");
                    println!("   回复   = {}", o.content.trim());
                    if !o.reasoning.trim().is_empty() {
                        println!("   推理   = {}…", o.reasoning.chars().take(120).collect::<String>());
                    }
                }
                Err(e) => {
                    println!("❌ 调用失败（{} ms）：{e}", t0.elapsed().as_millis());
                    std::process::exit(1);
                }
            }
            Ok(())
        }
        other => {
            eprintln!("未知子命令：{other}");
            eprintln!("可用：check | check-file | gates | selftest | llm-probe | ai-probe | audit");
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
