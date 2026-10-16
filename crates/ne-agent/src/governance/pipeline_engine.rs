//! 引擎能力封装（页面⑦ L5 正文）—— 注入包 + 16 门禁。
//!
//! 复用同一实现，不另起一套：
//! - 注入包：`ne_injection::build_injection_db`（与 ne-mcp 同函数）
//! - 16 门禁：`ne_engine::gates::run_all`（**唯一权威实现**，与 CLI 一致）
//!
//! 铁则：不调 LLM（离线门禁）；找不到文件给明确错误。
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

/// 找卷细纲（与 pipeline_view 同规）。
fn outline_file(lib_root: &Path, volume: &str) -> Result<std::path::PathBuf> {
    let vol = if volume.starts_with('卷') { volume.to_string() }
        else if volume.chars().all(|c| c.is_ascii_digit()) { format!("卷{volume:0>2}") }
        else { volume.to_string() };
    let cands = [
        lib_root.join("03_叙事规划/细纲").join(format!("{vol}.md")),
        lib_root.join("03_叙事规划/细纲").join(format!("{}.md", vol.trim_start_matches('卷'))),
    ];
    for c in &cands { if c.is_file() { return Ok(c.clone()); } }
    bail!("找不到 {vol} 的细纲")
}

/// 取某章的注入包（ne_inject）。
pub fn inject_view(lib_root: &Path, volume: &str, chapter: u32) -> Result<Value> {
    let outline = outline_file(lib_root, volume)?;
    let text = fs::read_to_string(&outline)
        .map_err(|e| anyhow::anyhow!("读细纲失败（{}）：{e}", outline.display()))?;
    let ch = ne_core::extract_chapter(&text, chapter)
        .ok_or_else(|| anyhow::anyhow!("细纲里切不到第{chapter}章"))?;

    let lib_root_data = ne_core::data_lib_root(lib_root.to_str().unwrap_or("."));
    let db = lib_root.join("novel.db");
    let content_dir = lib_root.join("04_正文");
    let usage: Vec<ne_core::UsageStat> = Vec::new();
    let banned: Vec<String> = ne_qc::BANNED_WORDS.iter().map(|s| s.to_string()).collect();
    let prev = chapter.saturating_sub(1);

    let packet = ne_injection::build_injection_db(
        &ch, &lib_root_data,
        if db.is_file() { Some(db.as_path()) } else { None },
        &content_dir, volume, prev, &usage, &banned,
    );

    // 12 块清单（给 UI 逐块展示）
    let blocks = json!({
        "chars": packet.chars.len(),
        "materials": packet.materials.len(),
        "prev_tail_chars": packet.prev_tail.chars().count(),
        "foreshadow_due": packet.foreshadow_due.len(),
        "foreshadow_plant": packet.foreshadow_plant.len(),
        "events": packet.events.len(),
        "missing_refs": packet.missing_refs.len(),
        "banned_words": packet.banned_words.len(),
        "world": packet.world,
    });

    Ok(json!({
        "command": "webui pipeline inject",
        "volume": volume, "chapter": chapter,
        "title": ch.title,
        "characters": ch.characters,
        "card_refs": ch.card_refs,
        "missing_refs": packet.missing_refs,
        "blocks_summary": blocks,
        "packet": packet,
        "note": "注入包由 ne_injection 产出（与 ne-mcp ne_inject 同实现）；只读不写库。",
    }))
}

/// 跑 **16 门禁**（`ne_engine::gates::run_all` —— 唯一权威实现，与 CLI 一致）。
///
/// `text`：正文全文；`chapter`/`volume`：章号卷号。
/// 返回 verdict(pass/fix/reject) + 各门禁简述 + P0/P1 清单。
pub fn check_view(lib_root: &Path, text: &str, chapter: u32, volume: &str) -> Result<Value> {
    if text.trim().is_empty() {
        bail!("正文为空，无法过门禁");
    }
    // 与 ne-engine CLI 同构造：config::load + RepoCtx::open
    let cfg = ne_engine::config::load(lib_root)
        .map_err(|e| anyhow::anyhow!("载入引擎配置失败（{}）：{e}", lib_root.display()))?;
    let ctx = ne_engine::ctx::RepoCtx::open(lib_root)
        .map_err(|e| anyhow::anyhow!("打开仓库上下文失败：{e}"))?;
    let r = ne_engine::gates::run_all(text, chapter, volume, &cfg, &ctx);

    let gates: Vec<Value> = r.gates.iter().map(|(k, v)| json!({
        "name": k, "pass": v.pass, "detail": v.detail,
    })).collect();

    Ok(json!({
        "command": "webui pipeline check",
        "chapter": chapter, "volume": volume,
        "verdict": r.verdict,
        "words": r.stats.words,
        "gates_run": r.stats.gates_run,
        "gates": gates,
        "p0": r.p0,
        "p1": r.p1,
        "p0_count": r.p0.len(),
        "p1_count": r.p1.len(),
        "note": "走 ne_engine::gates::run_all（与 CLI check 同实现）；确定性演算，不调 LLM。",
    }))
}

/// 对已落库的某章跑门禁（从 04_正文 读文件）。
pub fn check_chapter_file(lib_root: &Path, volume: &str, chapter: u32) -> Result<Value> {
    let dir = lib_root.join("04_正文").join(volume);
    if !dir.is_dir() {
        bail!("正文目录不存在：{}", dir.display());
    }
    // 找形如「第N章.md」或「第N章*.md」
    let mut hit: Option<std::path::PathBuf> = None;
    for e in fs::read_dir(&dir)?.flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        if n.starts_with(&format!("第{chapter}章")) {
            hit = Some(e.path());
            break;
        }
    }
    let Some(p) = hit else {
        bail!("找不到 {} 第{chapter}章的正文文件", volume);
    };
    let text = fs::read_to_string(&p)?;
    let mut v = check_view(lib_root, &text, chapter, volume)?;
    v["file"] = json!(p.strip_prefix(lib_root).unwrap_or(&p).display().to_string());
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outline_missing_reports_error() {
        let d = std::env::temp_dir().join("pe_nooutline");
        let _ = std::fs::create_dir_all(&d);
        assert!(inject_view(&d, "卷99", 1).is_err());
    }

    #[test]
    fn check_rejects_empty_text() {
        let d = std::env::temp_dir().join("pe_empty");
        let _ = std::fs::create_dir_all(&d);
        assert!(check_view(&d, "   ", 1, "卷01").is_err(), "空正文应拒");
    }
}
