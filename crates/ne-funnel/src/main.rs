//! ne-funnel CLI —— 六层漏斗的查看与推进。
//!
//! 用法：
//!   ne-funnel status  [--root DIR]      # 六层总览 + 下一个推进目标
//!   ne-funnel plan    [--root DIR] --json
//!   ne-funnel gates                     # 16 门禁注册表
//!   ne-funnel deps    [--root DIR]      # 依赖图（文本/DOT）
//!   ne-funnel verify  [--root DIR]      # 校验产物存在性 + 完成判据
//!   ne-funnel advance --chapter N [--root DIR] [--dry-run]
//!   ne-funnel selftest  [--root DIR]

use anyhow::Result;
use ne_funnel::{engine, find_root, funnel, gates, model::LayerState};
use std::path::PathBuf;

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
    let start = match arg(args, "--root") {
        Some(r) => PathBuf::from(r),
        None => std::env::current_dir()?,
    };
    find_root(&start).ok_or_else(|| {
        anyhow::anyhow!(
            "在 {} 向上找不到含 `02_资料库` 的仓库根（用 --root 指定）",
            start.display()
        )
    })
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("status");
    let json_out = args.iter().any(|a| a == "--json");

    match cmd {
        "status" => {
            let root = resolve_root(&args)?;
            let l = funnel::load(&root)?;
            let plan = engine::plan(&l);
            let sum = engine::summarize(&plan);
            if json_out {
                println!("{}", serde_json::to_string_pretty(&plan)?);
                return Ok(());
            }
            println!("仓库根：{}", plan.root);
            println!("事实源：{}", plan.funnel_path);
            println!(
                "进度：{}/{} 层已完成   执行序：{}",
                sum.done,
                sum.total,
                plan.exec_order.join(" → ")
            );
            println!();
            println!("{:<5} {:<12} {:<16} {:<8} {}", "层", "名称", "状态", "产物", "说明");
            println!("{}", "-".repeat(78));
            for lp in &plan.layers {
                let art = match lp.artifact_present {
                    Some(true) => "✓",
                    Some(false) => "✗ 缺失",
                    None => "—",
                };
                println!(
                    "{:<5} {:<12} {:<16} {:<8} {}",
                    lp.id,
                    lp.name,
                    lp.state_raw,
                    art,
                    lp.blocked_reason.clone().unwrap_or_default()
                );
            }
            println!();
            match &plan.next {
                Some(n) => println!("▶ 下一个应推进：{n}"),
                None => println!("▶ 无待推进层（全部 done）"),
            }
            if let Some(b) = &plan.blocked {
                println!("⛔ 首个阻塞层：{b}（依赖未满足，on_blocked=abort_with_report）");
            }
            if !plan.warnings.is_empty() {
                println!();
                for w in &plan.warnings {
                    println!("⚠ {w}");
                }
            }
            Ok(())
        }

        "plan" => {
            let root = resolve_root(&args)?;
            let l = funnel::load(&root)?;
            let plan = engine::plan(&l);
            if json_out {
                println!("{}", serde_json::to_string_pretty(&plan)?);
            } else {
                for lp in &plan.layers {
                    println!(
                        "{:<5} {:<12} done={:<5} runnable={:<5} missing={:?}",
                        lp.id, lp.name, lp.is_done, lp.runnable, lp.missing_deps
                    );
                }
                println!("next={:?} blocked={:?}", plan.next, plan.blocked);
            }
            Ok(())
        }

        "gates" => {
            println!("16 门禁注册表（事实源：rules.md §5 + engine/gate14.py）");
            println!();
            println!("{:<24} {:<14} {:<14} {}", "ID", "名称", "类型", "未过级别");
            println!("{}", "-".repeat(70));
            for g in gates::GATES {
                println!(
                    "{:<24} {:<14} {:<14} {:?}",
                    g.id,
                    g.name,
                    match g.kind {
                        gates::GateKind::Deterministic => "确定性",
                        gates::GateKind::Llm => "LLM",
                        gates::GateKind::Hybrid => "混合",
                    },
                    g.severity
                );
            }
            println!();
            let d = gates::GATES.iter().filter(|g| g.kind == gates::GateKind::Deterministic).count();
            let h = gates::GATES.iter().filter(|g| g.kind == gates::GateKind::Hybrid).count();
            let l = gates::GATES.iter().filter(|g| g.kind == gates::GateKind::Llm).count();
            println!("确定性 {d} / 混合 {h} / LLM {l}   共 {} 门", gates::GATES.len());
            println!();
            println!("verdict 规则：有 P0 → reject（不入库）｜仅 P1 → fix（可修）｜无 → pass");
            Ok(())
        }

        "deps" => {
            let root = resolve_root(&args)?;
            let l = funnel::load(&root)?;
            if args.iter().any(|a| a == "--dot") {
                println!("digraph funnel {{");
                println!("  rankdir=LR;");
                for layer in &l.funnel.layers {
                    println!("  \"{}\" [label=\"{}\\n{}\"];", layer.id, layer.id, layer.name);
                }
                for layer in &l.funnel.layers {
                    for d in &layer.deps {
                        println!("  \"{d}\" -> \"{}\";", layer.id);
                    }
                }
                println!("}}");
                return Ok(());
            }
            println!("六层依赖图（→ 表示「依赖」）");
            println!();
            for layer in engine::ordered_layers(&l.funnel) {
                println!(
                    "  {:<5} {:<12} 依赖: {}",
                    layer.id,
                    layer.name,
                    if layer.deps.is_empty() {
                        "（无）".to_string()
                    } else {
                        layer.deps.join(", ")
                    }
                );
            }
            println!();
            println!("注意执行序与声明序不同：L4 必须在 L3 之前（细纲引用需先有卡）。");
            Ok(())
        }

        "verify" => {
            let root = resolve_root(&args)?;
            let l = funnel::load(&root)?;
            let plan = engine::plan(&l);
            println!("产物存在性校验（相对仓库根 {})", plan.root);
            println!();
            let mut bad = 0;
            for layer in engine::ordered_layers(&l.funnel) {
                let art = layer.artifact().unwrap_or("(未声明)");
                let exists = l.artifact_exists(layer).unwrap_or(false);
                if !exists {
                    bad += 1;
                }
                println!(
                    "  {} {:<5} {:<28} {}",
                    if exists { "✓" } else { "✗" },
                    layer.id,
                    art,
                    if exists { "存在" } else { "缺失" }
                );
            }
            println!();
            println!("完成判据（funnel.yaml verify）：");
            if let Some(v) = &l.funnel.verify {
                for (k, val) in &v.criteria {
                    println!("  {k}: {val}");
                }
            }
            println!();
            if bad > 0 {
                println!("⚠ {bad} 层的产物路径缺失（注意：L5 是 chapters_1_to_N，目录本身以 04_正文/卷01/ 计）");
            } else {
                println!("✓ 全部层产物路径存在");
            }
            Ok(())
        }

        "advance" => {
            let root = resolve_root(&args)?;
            let ch: u32 = arg(&args, "--chapter")
                .ok_or_else(|| anyhow::anyhow!("advance 需要 --chapter N"))?
                .parse()?;
            let dry = args.iter().any(|a| a == "--dry-run");
            // 安全闸：先展示将要发生什么
            let l = funnel::load(&root)?;
            let before = l
                .funnel
                .layer("L5")
                .map(|x| x.status.clone())
                .unwrap_or_default();
            println!("L5 status：{before} → chapters_1_to_{ch}");
            if dry {
                println!("（--dry-run，未写文件）");
                return Ok(());
            }
            let changed = engine::advance_l5(&root, ch)?;
            if changed {
                // 复核：必须能重新解析，避免写出损坏的 YAML
                let l2 = funnel::load(&root)?;
                match l2.funnel.layer("L5") {
                    Some(x) if x.status == format!("chapters_1_to_{ch}") => {
                        println!("✓ 已写入并通过重解析校验")
                    }
                    _ => println!("⚠ 已写入，但重解析结果不符预期，请人工检查"),
                }
            } else {
                println!("（无变化）");
            }
            Ok(())
        }

        "selftest" => {
            let root = resolve_root(&args)?;
            let l = funnel::load(&root)?;
            let plan = engine::plan(&l);
            println!("root={}", root.display());
            // 关键不变量：L4 必须排在 L3 前
            let order = &plan.exec_order;
            let pos = |x: &str| order.iter().position(|o| o == x);
            let l4_before_l3 = match (pos("L4"), pos("L3")) {
                (Some(a), Some(b)) => a < b,
                _ => false,
            };
            println!("层数={}", l.funnel.layers.len());
            println!("执行序={}", order.join(" → "));
            println!("L4 在 L3 前={l4_before_l3}");
            println!("门禁数={}", gates::GATES.len());
            for layer in &l.funnel.layers {
                let s: LayerState = layer.state();
                println!("  {} [{}] {}", layer.id, s.raw(), layer.artifact().unwrap_or("-"));
            }
            let ok = l4_before_l3 && gates::GATES.len() == 16 && !plan.layers.is_empty();
            println!("selftest={}", if ok { "PASS" } else { "FAIL" });
            if !ok {
                std::process::exit(1);
            }
            Ok(())
        }

        other => {
            eprintln!("未知子命令：{other}");
            eprintln!("可用：status | plan | gates | deps | verify | advance | selftest");
            std::process::exit(2);
        }
    }
}
