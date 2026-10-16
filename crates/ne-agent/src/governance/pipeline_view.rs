//! 管线视图（Pipeline View）—— 把 6 层漏斗 + ne 引擎能力暴露为 HTTP 只读接口。
//!
//! # 为什么放 ne-agent（而非调 ne-mcp）
//! ne-mcp 依赖 ne-agent（单向），不能反向。而 ne-mcp/tools.rs 的 funnel/materials/cardgen
//! 实现只用到 ne_core 的类型（Funnel/extract_chapter/outline_dir），这些 ne-agent 已依赖。
//! 故复用同一库同一类型，不是重复实现，也不引入循环依赖。
//!
//! # 数据来源（全部只读）
//! - 05_管线框架/funnel.yaml —— 六层漏斗状态（单一事实源）
//! - 03_叙事规划/细纲/卷NN.md —— 细纲（待建卡提取）
//! - 02_资料库/** —— L4 资料卡（对账）
//! - novel.db.chapters —— 已生成正文
//!
//! # 铁则
//! - 只读：不推进漏斗、不写库、不落盘。
//! - 不调 LLM：门禁用离线判据（不编造分数）。
//! - 找不到文件给明确错误，不静默返回空。
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

/// 漏斗配置相对路径。
const FUNNEL_REL: &str = "05_管线框架/funnel.yaml";

fn funnel_path(lib_root: &Path) -> PathBuf {
    lib_root.join(FUNNEL_REL)
}

/// 六层漏斗进度（页面① 核心数据）。
pub fn funnel_view(lib_root: &Path) -> Result<Value> {
    let p = funnel_path(lib_root);
    if !p.is_file() {
        bail!("找不到漏斗配置：{}（库根是否正确？）", p.display());
    }
    let txt = fs::read_to_string(&p)?;
    let f = ne_core::Funnel::parse(&txt)?;

    let stages: Vec<Value> = f
        .layers
        .iter()
        .map(|s| {
            json!({
                "id": s.id, "name": s.name, "status": s.status,
                "file": s.file, "dir": s.dir, "deps": s.deps,
                "done": s.status == "done",
                "chapter_state": s.status.starts_with("chapters"),
            })
        })
        .collect();

    let pending = f.pending();
    let done_n = f.layers.iter().filter(|s| s.status == "done").count();
    let total = f.layers.len();

    let next = match pending.first() {
        None => json!({
            "layer": null, "action": "全部完成",
            "hint": "六层漏斗已跑完；可在 L5 正文继续写后续章。",
        }),
        Some(s) => {
            let (action, hint) = if s.status.starts_with("chapters") {
                ("继续写正文", format!(
                    "该层为章节态（{}）。下一步：ne_inject 取注入包 → 生成 → ne_check 过 16 门禁 → sync 落库。",
                    s.status))
            } else {
                ("推进该层", format!(
                    "该层未完成（{}）。按漏斗顺序：先看依赖是否满足，再跑对应引擎能力。", s.status))
            };
            json!({
                "layer": s.id, "name": s.name, "status": s.status,
                "file": s.file, "deps": s.deps, "action": action, "hint": hint,
            })
        }
    };

    Ok(json!({
        "command": "webui pipeline funnel",
        "funnel_file": FUNNEL_REL,
        "root": f.root,
        "total": total, "done": done_n,
        "pending_count": pending.len(),
        "progress": format!("{done_n}/{total}"),
        "next": next,
        "stages": stages,
        "note": "只读；status==done 才算完成，chapters_* 属 pending（还有后续章）。",
    }))
}

/// L4 资料对账（页面② 核心数据）。
pub fn materials_view(lib_root: &Path) -> Result<Value> {
    let lib = lib_root.join("02_资料库");
    let mut groups: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
    let mut total = 0i64;
    if lib.is_dir() {
        for e in fs::read_dir(&lib)?.flatten() {
            let p = e.path();
            if p.is_dir() {
                let name = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                if name.starts_with('_') {
                    continue;
                }
                let n = fs::read_dir(&p)
                    .map(|it| it.flatten().filter(|x| x.path().is_file()).count())
                    .unwrap_or(0) as i64;
                *groups.entry(name).or_insert(0) += n;
                total += n;
            }
        }
    }
    let mut declared = json!(null);
    let fp = funnel_path(lib_root);
    if fp.is_file() {
        if let Ok(txt) = fs::read_to_string(&fp) {
            if let Ok(v) = serde_yaml::from_str::<Value>(&txt) {
                if let Some(m) = v.get("materials") {
                    declared = m.clone();
                }
            }
        }
    }
    Ok(json!({
        "command": "webui pipeline materials",
        "lib_dir": "02_资料库",
        "total_files": total,
        "groups": groups.iter().map(|(k, v)| json!({"name": k, "count": v})).collect::<Vec<_>>(),
        "declared": declared,
        "note": "只读统计；缺口对照见 cardgen 的【待建卡】清单。",
    }))
}

/// 卷号归一化（01 → 卷01）。
fn norm_volume(v: &str) -> String {
    let s = v.trim();
    if s.is_empty() {
        return "卷01".to_string();
    }
    if s.starts_with('卷') {
        s.to_string()
    } else if s.chars().all(|c| c.is_ascii_digit()) {
        format!("卷{s:0>2}")
    } else {
        s.to_string()
    }
}

/// 找卷细纲文件。
fn find_outline(lib_root: &Path, vol: &str) -> Result<PathBuf> {
    let cands = [
        lib_root.join("03_叙事规划/细纲").join(format!("{vol}.md")),
        lib_root.join("03_叙事规划/细纲").join(format!("{}.md", vol.trim_start_matches('卷'))),
    ];
    for c in &cands {
        if c.is_file() {
            return Ok(c.clone());
        }
    }
    bail!(
        "找不到 {} 的细纲（试过：{}）",
        vol,
        cands.iter().map(|c| c.display().to_string()).collect::<Vec<_>>().join(" / ")
    )
}

/// 待建卡工单（页面③ 核心数据）：从细纲提取【待建卡】与卡号引用。
pub fn cardgen_view(lib_root: &Path, volume: &str) -> Result<Value> {
    let vol = norm_volume(volume);
    let outline = find_outline(lib_root, &vol)?;
    let txt = fs::read_to_string(&outline)
        .map_err(|e| anyhow::anyhow!("读细纲失败（{}）：{e}", outline.display()))?;

    let mut jobs: Vec<Value> = Vec::new();
    let mut in_block = false;
    for line in txt.lines() {
        let l = line.trim();
        if l.contains("待建卡") {
            in_block = true;
            continue;
        }
        if in_block {
            if l.starts_with("##") || (l.is_empty() && jobs.len() > 3) {
                in_block = false;
                continue;
            }
            let t = l.trim_start_matches(['-', '*', ' ']).trim();
            if t.is_empty() || t.starts_with('【') {
                continue;
            }
            if let Some((cat, name)) = t.split_once(['：', ':']) {
                jobs.push(json!({"category": cat.trim(), "name": name.trim()}));
            }
        }
    }

    let chapter_count = txt.matches("### 第").count();
    let chars = txt.chars().count();
    let rel = outline.strip_prefix(lib_root).unwrap_or(&outline).display().to_string();

    Ok(json!({
        "command": "webui pipeline cardgen",
        "volume": vol,
        "outline_file": rel,
        "outline_chars": chars,
        "chapters_in_outline": chapter_count,
        "pending_cards": jobs.len(),
        "cards": jobs,
        "note": "从细纲【待建卡】段提取；实际建卡由 ne_cardgen（MCP）执行，WebUI 只列工单。",
    }))
}

/// L5 正文进度（页面④ 核心数据）：已写章节 + 下一章。
pub fn chapters_view(conn: &rusqlite::Connection, lib_root: &Path, volume: &str) -> Result<Value> {
    let vol = norm_volume(volume);
    let mut written: Vec<i64> = Vec::new();
    let t_exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='chapters'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if t_exists > 0 {
        let mut st = conn.prepare(
            "SELECT DISTINCT chapter_no FROM chapters WHERE volume=?1 ORDER BY chapter_no",
        )?;
        let it = st.query_map(rusqlite::params![vol], |r| r.get::<_, i64>(0))?;
        written = it.filter_map(|x| x.ok()).collect();
    }

    let dir = lib_root.join("04_正文").join(&vol);
    let mut files: Vec<String> = Vec::new();
    if dir.is_dir() {
        for e in fs::read_dir(&dir)?.flatten() {
            let p = e.path();
            if p.is_file() {
                files.push(p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default());
            }
        }
    }
    files.sort();

    let outline_chapters = find_outline(lib_root, &vol)
        .ok()
        .and_then(|p| fs::read_to_string(p).ok())
        .map(|t| t.matches("### 第").count())
        .unwrap_or(0);
    let next_chapter = written.iter().max().copied().unwrap_or(0) + 1;

    Ok(json!({
        "command": "webui pipeline chapters",
        "volume": vol,
        "db_chapters": written.len(),
        "db_chapter_nos": written,
        "disk_files": files.len(),
        "outline_chapters": outline_chapters,
        "next_chapter": next_chapter,
        "progress": format!("{}/{}", written.len(), outline_chapters),
        "next_action": if outline_chapters > 0 && written.len() >= outline_chapters {
            "本卷章节已写完，可进入下一卷或全书复核"
        } else {
            "下一步：ne_inject 取注入包 → 生成正文 → ne_check 过 16 门禁 → sync 落库"
        },
        "note": "只读；真实写作由 ne 引擎（MCP）执行。",
    }))
}

/// 全书总览（首页用）。
pub fn overview_view(conn: &rusqlite::Connection, lib_root: &Path, volume: &str) -> Result<Value> {
    let f = funnel_view(lib_root).unwrap_or_else(|e| json!({"error": e.to_string()}));
    let m = materials_view(lib_root).unwrap_or_else(|e| json!({"error": e.to_string()}));
    let c = chapters_view(conn, lib_root, volume).unwrap_or_else(|e| json!({"error": e.to_string()}));
    Ok(json!({
        "command": "webui pipeline overview",
        "funnel": f, "materials": m, "chapters": c,
        "note": "只读聚合；页面① 数据源。",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_norm() {
        assert_eq!(norm_volume("01"), "卷01");
        assert_eq!(norm_volume("卷03"), "卷03");
        assert_eq!(norm_volume(""), "卷01");
    }

    #[test]
    fn funnel_view_reports_missing_file() {
        let d = std::env::temp_dir().join("pv_nofile");
        let _ = std::fs::create_dir_all(&d);
        assert!(funnel_view(&d).is_err(), "无 funnel.yaml 应报错而非静默空");
    }

    #[test]
    fn funnel_view_parses_yaml() {
        let d = std::env::temp_dir().join("pv_funnel_ok");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("05_管线框架")).unwrap();
        std::fs::write(
            d.join("05_管线框架/funnel.yaml"),
            "root: test\nlayers:\n  - id: L0\n    name: 概括\n    status: done\n  - id: L1\n    name: 总纲\n    status: pending\n",
        ).unwrap();
        let v = funnel_view(&d).unwrap();
        assert_eq!(v["total"], 2);
        assert_eq!(v["done"], 1);
        assert_eq!(v["next"]["layer"], "L1");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn materials_counts_dirs() {
        let d = std::env::temp_dir().join("pv_mat");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("02_资料库/角色")).unwrap();
        std::fs::write(d.join("02_资料库/角色/A.md"), "x").unwrap();
        let v = materials_view(&d).unwrap();
        assert_eq!(v["total_files"], 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn outline_missing_reports_error() {
        let d = std::env::temp_dir().join("pv_nooutline");
        let _ = std::fs::create_dir_all(&d);
        assert!(cardgen_view(&d, "卷99").is_err());
    }
}