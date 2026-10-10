//! neim —— 资料库导入 / 重编号 CLI。
//!
//! 用法：
//!   neim scan   --root <资料库根>          # 只扫描统计，不落库
//!   neim import --root <资料库根>          # 全量导入全文 → materials
//!   neim map    --root <资料库根>          # 生成重编号映射（不应用）
//!   neim verify --root <资料库根>          # 对账：磁盘 vs DB
//!
//! ⚠ 默认**只读**；写库需显式给 `--write`。

use anyhow::Result;
use rusqlite::params;
use std::collections::HashMap;
use std::path::PathBuf;

fn arg(args: &[String], k: &str) -> Option<String> {
    args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned()
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).cloned().unwrap_or_default();
    let root = PathBuf::from(
        arg(&args, "--root").unwrap_or_else(|| "/sdcard/Download/小说相关备份/projects/projects/系统/资料库".into()),
    );
    let write = args.iter().any(|a| a == "--write");
    let lib = root.join("02_资料库");

    match cmd.as_str() {
        "scan" => {
            let s = ne_import::scan_library(&lib)?;
            let total: usize = s.cards.iter().map(|c| c.content_chars).sum();
            println!("扫描根 = {}", s.root);
            println!("卡片数 = {}", s.cards.len());
            println!("净字数 = {total}");
            println!("无编号 = {}", s.no_code.len());
            let mut by: HashMap<&str, (usize, usize)> = HashMap::new();
            for c in &s.cards {
                let e = by.entry(c.category.as_str()).or_insert((0, 0));
                e.0 += 1;
                e.1 += c.content_chars;
            }
            let mut v: Vec<_> = by.into_iter().collect();
            v.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
            println!("\n{:<12} {:>5} {:>10}", "类别", "张数", "净字数");
            for (k, (n, ch)) in v {
                println!("{k:<12} {n:>5} {ch:>10}");
            }
        }
        "import" => {
            let s = ne_import::scan_library(&lib)?;
            if !write {
                println!("[dry-run] 将导入 {} 张卡（加 --write 才落库）", s.cards.len());
                return Ok(());
            }
            let con = ne_db::connect(&root.join("novel.db").to_string_lossy())?;
            let n = ne_import::import_cards(&con, &s)?;
            ne_import::record_ledger(&con, &s)?;
            println!("✅ 已导入 {n} 张卡（含 content 全文）");
        }
        "map" => {
            let s = ne_import::scan_library(&lib)?;
            let m = ne_import::renumber::build_map(&s.cards);
            println!("映射条目 = {}", m.len());
            let changed = m.iter().filter(|e| e.old_code != e.new_code).count();
            println!("需变更 = {changed}");
            if write {
                let con = ne_db::connect(&root.join("novel.db").to_string_lossy())?;
                let n = ne_import::renumber::save_map(&con, &m)?;
                println!("✅ 映射已入库 {n} 条（未应用）");
            } else {
                println!("\n[dry-run] 前 15 条：");
                for e in m.iter().take(15) {
                    println!("  {} → {}  ({})", e.old_code, e.new_code, e.rule);
                }
            }
        }
        "renumber" => {
            // 生成映射（若未生成）→ 显示影响面 → 可选应用
            let scan = ne_import::scan_library(&lib)?;
            let m = ne_import::renumber::build_map(&scan.cards);
            let con = ne_db::connect(&root.join("novel.db").to_string_lossy())?;
            if write {
                ne_import::renumber::save_map(&con, &m)?;
            }
            let map = ne_import::renumber::load_map(&con)?;
            println!("映射条目 = {}（当前库内有效 {}）", m.len(), map.len());
            for e in m.iter().filter(|e| e.old_code != e.new_code).take(12) {
                println!("  {} → {}   [{}]", e.old_code, e.new_code, e.rule);
            }
            let changed = m.iter().filter(|e| e.old_code != e.new_code).count();
            println!("需变更 = {changed}");

            // 统计引用影响
            let refs_docs = ne_import::renumber::count_refs_in_docs(&con, &map)?;
            println!("文档引用替换 = {refs_docs}");
            let (f, r) = ne_import::renumber::rewrite_files(&lib, &map, true)?;
            println!("卡片文件受影响 = {f} 个文件 / {r} 处引用");

            if args.iter().any(|a| a == "--apply") {
                println!("\n>>> 应用变更 <<<");
                let st = ne_import::renumber::apply_to_materials(&con, &map, false)?;
                println!("  库内改 code = {}，改引用 = {}，动卡 = {}", st.codes_updated, st.refs_in_materials, st.cards_touched);
                let (f2, r2) = ne_import::renumber::rewrite_files(&lib, &map, false)?;
                println!("  磁盘改引用 = {f2} 文件 / {r2} 处");
                let (rn, cf) = ne_import::renumber::rename_files(&lib, &m, false)?;
                println!("  磁盘改名 = {rn} 个（冲突 {cf}）");
                let (hf, hn) = ne_import::renumber::fix_h1_codes(&lib, &m, false)?;
                println!("  H1 编号修复 = {hf} 文件 / {hn} 处");
            } else {
                println!("\n[dry-run] 加 --apply 才真正改写");
            }
        }
        "index" => {
            // 生成索引目录：catalog_index 已含卡片与文档；这里补目录节点 + 导出 INDEX.md
            let con = ne_db::connect(&root.join("novel.db").to_string_lossy())?;
            // 目录节点
            let mut dirs = std::collections::BTreeSet::new();
            for p in con.prepare("SELECT node_path FROM catalog_index")?.query_map([], |r| r.get::<_, String>(0))?.flatten() {
                let mut acc = String::new();
                let parts: Vec<&str> = p.split('/').collect();
                for seg in &parts[..parts.len().saturating_sub(1)] {
                    if !acc.is_empty() { acc.push('/'); }
                    acc.push_str(seg);
                    dirs.insert(acc.clone());
                }
            }
            for c in con.prepare("SELECT DISTINCT category FROM materials WHERE content_chars IS NOT NULL")?.query_map([], |r| r.get::<_, String>(0))?.flatten() {
                dirs.insert(format!("02_资料库/{c}"));
            }
            let mut n = 0;
            for d in &dirs {
                con.execute(
                    "INSERT INTO catalog_index(node_path,node_type,category,src_kind)
                     VALUES(?1,'dir',?2,'dir') ON CONFLICT(node_path) DO UPDATE SET node_type='dir'",
                    params![d, d.split('/').next().unwrap_or("")],
                )?;
                n += 1;
            }
            println!("目录节点 = {n}");
            let (files, dirs_n): (i64, i64) = con.query_row(
                "SELECT COALESCE(SUM(CASE WHEN node_type='file' THEN 1 ELSE 0 END),0), COALESCE(SUM(CASE WHEN node_type='dir' THEN 1 ELSE 0 END),0) FROM catalog_index", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
            println!("索引: {files} 文件 / {dirs_n} 目录");

            if args.iter().any(|a| a == "--md") {
                let out = root.join("INDEX.md");
                let mut buf = String::from("# 资料库索引目录\n\n> 自动生成，请勿手改。\n\n## 卡片（按类别）\n\n");
                let mut st = con.prepare("SELECT category, COUNT(*), COALESCE(SUM(content_chars),0) FROM materials WHERE content_chars IS NOT NULL GROUP BY category ORDER BY 2 DESC")?;
                buf.push_str("| 类别 | 张数 | 字数 |\n|---|---|---|\n");
                for r in st.query_map([], |r| Ok((r.get::<_,String>(0)?, r.get::<_,i64>(1)?, r.get::<_,Option<i64>>(2)?.unwrap_or(0))))?.flatten() {
                    buf.push_str(&format!("| {} | {} | {} |\n", r.0, r.1, r.2));
                }
                buf.push_str("\n## 文档（按来源）\n\n| 来源 | 文件数 | 字数 |\n|---|---|---|\n");
                let mut st2 = con.prepare("SELECT src_kind, COUNT(*), COALESCE(SUM(char_count),0) FROM catalog_index WHERE node_type='file' AND src_kind<>'dir' GROUP BY src_kind ORDER BY 3 DESC")?;
                for r in st2.query_map([], |r| Ok((r.get::<_,String>(0)?, r.get::<_,i64>(1)?, r.get::<_,Option<i64>>(2)?.unwrap_or(0))))?.flatten() {
                    buf.push_str(&format!("| {} | {} | {} |\n", r.0, r.1, r.2));
                }
                std::fs::write(&out, buf)?;
                println!("✅ 已导出 {}", out.display());
            }
        }
        "ensure-h1" => {
            let (f, n, skip) = ne_import::renumber::ensure_h1_code(&lib, !write)?;
            println!("H1 补编号: {f} 文件 / {n} 处{}", if write { "" } else { "（dry-run）" });
            for s in skip.iter().take(10) {
                println!("  ⚠ {s}");
            }
        }
        "dangling" => {
            let con = ne_db::connect(&root.join("novel.db").to_string_lossy())?;
            let d = ne_import::renumber::find_dangling(&con)?;
            println!("悬空引用 = {}", d.len());
            for x in d.iter().take(20) { println!("  {x}"); }
        }
        "docs" => {
            let docs = ne_import::scan_docs(&root)?;
            let total: usize = docs.iter().map(|d| d.net_chars).sum();
            let mut by: HashMap<&str, (usize, usize)> = HashMap::new();
            for d in &docs {
                let e = by.entry(d.src_kind.as_str()).or_insert((0, 0));
                e.0 += 1;
                e.1 += d.net_chars;
            }
            let mut v: Vec<_> = by.into_iter().collect();
            v.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
            println!("文档数 = {}  净字数 = {}", docs.len(), total);
            for (k, (n, ch)) in v {
                println!("  {k:<12} {n:>4} 文件 {ch:>8} 字");
            }
            if write {
                let con = ne_db::connect(&root.join("novel.db").to_string_lossy())?;
                let n = ne_import::import_docs(&con, &docs, &root.to_string_lossy())?;
                println!("✅ 已导入 {n} 个文档到 catalog_index");
            } else {
                println!("[dry-run] 加 --write 才落库");
            }
        }
        "verify-docs" => {
            let con = ne_db::connect(&root.join("novel.db").to_string_lossy())?;
            let bad = ne_import::verify_docs(&con, &root)?;
            if bad.is_empty() {
                println!("✅ 文档对账通过");
            } else {
                println!("❌ 差异 {} 条", bad.len());
                for (p, a, b) in bad.iter().take(15) {
                    println!("  {p}  磁盘{a} vs 台账{b}");
                }
            }
        }
        "verify" => {
            let con = ne_db::connect(&root.join("novel.db").to_string_lossy())?;
            let bad = ne_import::verify_import(&con, &lib)?;
            if bad.is_empty() {
                println!("✅ 对账通过：磁盘与 DB 字数一致");
            } else {
                println!("❌ 对账差异 {} 条：", bad.len());
                for (p, s, d) in bad.iter().take(20) {
                    println!("  {p}  源{s} vs DB{d}");
                }
            }
        }
        _ => {
            eprintln!("用法: neim <scan|import|map|verify> --root <资料库根> [--write]");
            std::process::exit(2);
        }
    }
    Ok(())
}
