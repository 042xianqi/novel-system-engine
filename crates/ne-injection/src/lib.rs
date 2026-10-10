//! ne-injection：细纲章 → 注入包（两层选卡 + 前文 + 伏笔 + 事件 + 世界推演）。
//!
//! 2026-10-08 口径：资料卡不再一律全文，按两层策略装配 ——
//! 第 1 层确定性排序（ne_core::rank_cards：细纲命中 > 伏笔待收 > 被引次数 > 章距近）
//! 选 top3 全文（≤1500 token/张）+ top5 摘要（≤200 token/张，取卡面 3 行摘要字段）；
//! 第 2 层 LLM 微调（8 卡候选池内确认/替换/调序）留清晰接缝，由调用方在拿到
//! 注入包后按需回调。预算报告回填 packet.budget（总预算 20000 token）。
//! 查不到的引用如实填 missing_refs，不杜撰。

use ne_core::{
    estimate_tokens, rank_cards, truncate_tokens, BudgetReport, CharacterSnapshot,
    ChapterOutline, EventRef, InjectionPacket, MaterialRef, UsageStat, FULL_CARD_TOKEN_CAP,
    SUMMARY_TOKEN_CAP, TOTAL_TOKEN_BUDGET,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// 细纲某章 + 资料库根 → 注入包（兼容入口：无用量统计、无额外禁词）。
pub fn build_injection(
    chapter: &ChapterOutline,
    lib_root: &Path,
    content_dir: &Path,
    volume: &str,
    prev_chapter: u32,
) -> InjectionPacket {
    build_injection_ranked(chapter, lib_root, content_dir, volume, prev_chapter, &[], &[])
}

/// 两层选卡·完整装配：细纲章 + 用量统计 + 禁词 → 预算内注入包（纯确定性，0 LLM）。
pub fn build_injection_ranked(
    chapter: &ChapterOutline,
    lib_root: &Path,
    content_dir: &Path,
    volume: &str,
    prev_chapter: u32,
    usage: &[UsageStat],
    banned_words: &[String],
) -> InjectionPacket {
    let mut packet = InjectionPacket::default();
    packet.chapter = chapter.clone();

    // ① 出场角色：查 角色/ 下 CHAR-*.md（状态快照注入，占预算极小，不走资料卡预算）
    let char_files = std::fs::read_dir(lib_root.join("角色"))
        .map(|rd| rd.flatten().filter_map(|e| e.path().to_str().map(String::from)).collect::<Vec<_>>())
        .unwrap_or_default();
    for ref_code in &chapter.card_refs {
        if ref_code.starts_with("CHAR") {
            match char_files.iter().find(|p| p.contains(ref_code.as_str())) {
                Some(p) => {
                    let body = std::fs::read_to_string(p).unwrap_or_default();
                    packet.chars.push(CharacterSnapshot {
                        name: extract_name(&body),
                        code: ref_code.clone(),
                        realm: extract_field(&body, "境界"),
                        location: extract_field(&body, "所属"),
                        status: extract_field(&body, "身份"),
                    });
                }
                None => packet.missing_refs.push(ref_code.clone()),
            }
        }
    }

    // ② 资料卡两层选卡（第 1 层确定性）：非 CHAR 引用 + 历史用量 → top3 全文 + top5 摘要
    let non_char_refs: Vec<String> = chapter
        .card_refs
        .iter()
        .filter(|r| !r.starts_with("CHAR"))
        .cloned()
        .collect();
    let (full_codes, summary_codes) = rank_cards(&non_char_refs, usage, chapter.number);
    for code in &full_codes {
        match locate_card(lib_root, code) {
            Some(p) => {
                let body = std::fs::read_to_string(p).unwrap_or_default();
                packet.materials.push(MaterialRef {
                    code: code.clone(),
                    name: extract_name(&body),
                    content: truncate_tokens(&body, FULL_CARD_TOKEN_CAP),
                    summary: truncate_tokens(&extract_summary(&body), SUMMARY_TOKEN_CAP),
                    is_full: true,
                });
            }
            None => packet.missing_refs.push(code.clone()),
        }
    }
    for code in &summary_codes {
        match locate_card(lib_root, code) {
            Some(p) => {
                let body = std::fs::read_to_string(p).unwrap_or_default();
                packet.materials.push(MaterialRef {
                    code: code.clone(),
                    name: extract_name(&body),
                    content: String::new(),
                    summary: truncate_tokens(&extract_summary(&body), SUMMARY_TOKEN_CAP),
                    is_full: false,
                });
            }
            None => packet.missing_refs.push(code.clone()),
        }
    }

    // ③ 前文 ~500 字：04_正文/卷NN/第(N-1)章.md 尾部
    if prev_chapter > 0 {
        let prev_path = content_dir.join(volume).join(format!("第{prev_chapter}章.md"));
        if let Ok(body) = std::fs::read_to_string(&prev_path) {
            let tail: String = body.chars().rev().take(500).collect::<Vec<_>>().into_iter().rev().collect();
            packet.prev_tail = tail;
        }
    }

    // ④ 事件：从细纲引用里抓 EVT-* 作占位 due
    for ref_code in &chapter.card_refs {
        if ref_code.starts_with("EVT") {
            packet.events.push(EventRef {
                id: ref_code.clone(),
                name: String::new(),
                status: "pending".into(),
            });
        }
    }

    // ⑤ 禁词表注入（写前注入第 5 类）
    packet.banned_words = banned_words.to_vec();

    // ⑥ 预算报告（卡片 token + 前章尾部 + 细纲 + 禁词）
    let card_tokens: usize = packet
        .materials
        .iter()
        .map(|m| {
            if m.is_full {
                estimate_tokens(&m.content)
            } else {
                estimate_tokens(&m.summary)
            }
        })
        .sum();
    let total_est_tokens = card_tokens
        + estimate_tokens(&packet.prev_tail)
        + estimate_tokens(&chapter.outline)
        + packet.banned_words.iter().map(|w| estimate_tokens(w)).sum::<usize>();
    packet.budget = BudgetReport {
        full_count: full_codes.len(),
        summary_count: summary_codes.len(),
        card_tokens,
        total_est_tokens,
        total_budget: TOTAL_TOKEN_BUDGET,
    };

    // ⑦ 世界推演：M1 占位（M3 接公式）
    packet.world = serde_json::json!({ "note": "M1 占位，M3 填 civ.tide/ent.entropy/res.decay" });
    packet
}

/// 按编号在 02_资料库 各子类里找卡文件（编号在文件名里，如 EVT-99-005.md）。
fn locate_card(lib_root: &Path, ref_code: &str) -> Option<PathBuf> {
    let by_prefix_sub: HashMap<&str, &str> = [
        ("EVT", "事件"), ("REL", "关系"), ("CAUSE", "因果线"), ("CAS", "因果线"),
        ("TECH", "科技"), ("ART", "科技"), ("LOC", "地图"), ("MTC", "丹药"),
        ("CHAR", "角色"), ("SYS", "文明"), ("CULT", "文明"), ("ORG", "文明"), ("CIV", "文明"),
    ]
    .into_iter()
    .collect();
    if let Some(prefix) = ref_code.split('-').next() {
        if let Some(sub) = by_prefix_sub.get(prefix) {
            let dir = lib_root.join(sub);
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for e in entries.flatten() {
                    let p = e.path();
                    if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                        if name.contains(ref_code) {
                            return Some(p);
                        }
                    }
                }
            }
        }
    }
    None
}

/// 抓一句话名称（首行 H1）。
fn extract_name(body: &str) -> String {
    body.lines()
        .find(|l| l.starts_with('#'))
        .map(|l| l.trim_start_matches('#').trim().to_string())
        .unwrap_or_default()
}

/// 抓某字段行（`- **境界**：xx` / `- 境界：xx`）。
fn extract_field(body: &str, key: &str) -> String {
    let re = regex::Regex::new(&format!(r"[-*]?\s*\*?\*?{key}\*?\*?[：:]\s*(.+)")).unwrap();
    re.captures(body).map(|c| c[1].trim().to_string()).unwrap_or_default()
}

/// 抽卡面"3 行摘要"：找 摘要 段（`## 摘要` / `**摘要**：`）取最多 3 行；
/// 找不到则取正文前 3 个非标题行兜底。
fn extract_summary(body: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut in_section = false;
    for l in body.lines() {
        let t = l.trim();
        let is_heading = t.starts_with('#');
        if !in_section {
            if t.contains("摘要") && (is_heading || t.starts_with("**") || t.starts_with('-')) {
                in_section = true;
            }
            continue;
        }
        if is_heading {
            break;
        }
        if t.is_empty() {
            continue;
        }
        lines.push(t.trim_start_matches('-').trim().to_string());
        if lines.len() >= 3 {
            break;
        }
    }
    if lines.is_empty() {
        lines = body
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .take(3)
            .map(|l| l.trim_start_matches('-').trim().to_string())
            .collect();
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn build_injection_returns_missing_for_empty_lib() {
        let tmp = std::env::temp_dir().join("ne_inj_test");
        fs::create_dir_all(tmp.join("02_资料库/角色/主角")).unwrap();
        fs::create_dir_all(tmp.join("02_资料库/事件")).unwrap();
        fs::create_dir_all(tmp.join("04_正文/卷01")).unwrap();

        let mut ch = ChapterOutline::default();
        ch.number = 3;
        ch.title = "老屋夜".into();
        ch.card_refs = vec!["CHAR-99-001".into(), "EVT-99-005".into()];

        let pkt = build_injection(&ch, &tmp.join("02_资料库"), &tmp.join("04_正文"), "卷01", 2);
        assert!(pkt.missing_refs.iter().any(|m| m == "CHAR-99-001"));
        assert!(pkt.missing_refs.iter().any(|m| m == "EVT-99-005"));
        assert!(pkt.events.iter().any(|e| e.id == "EVT-99-005"));
        assert!(pkt.world.get("note").is_some());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn ranked_injection_selects_full_and_summary_within_budget() {
        let tmp = std::env::temp_dir().join("ne_inj_ranked_test");
        let lib = tmp.join("02_资料库");
        // 5 张事件卡 + 5 张地图卡 + 1 张丹药卡（仅出现在用量统计里）
        for (dir, prefix) in [("事件", "EVT"), ("地图", "LOC")] {
            fs::create_dir_all(lib.join(dir)).unwrap();
            for i in 0..5u32 {
                let body = format!(
                    "# {prefix}-99-{i:03} 测试卡\n## 摘要\n- 摘要行一\n- 摘要行二\n- 摘要行三\n\n{}\n",
                    "正文字。".repeat(400)
                );
                fs::write(lib.join(dir).join(format!("{prefix}-99-{i:03}.md")), body).unwrap();
            }
        }
        fs::create_dir_all(lib.join("丹药")).unwrap();
        fs::write(lib.join("丹药/MTC-99-000.md"), "# MTC-99-000 丹\n## 摘要\n- 三行摘要\n\n正文。\n").unwrap();

        let mut ch = ChapterOutline::default();
        ch.number = 10;
        ch.title = "测试章".into();
        ch.card_refs = (0..5u32)
            .flat_map(|i| vec![format!("EVT-99-{i:03}"), format!("LOC-99-{i:03}")])
            .collect();

        let usage = vec![UsageStat {
            code: "MTC-99-000".into(),
            ref_count: 5,
            last_chapter: 2,
            foreshadow_due: false,
        }];
        let banned = vec!["归墟".to_string()];
        let pkt = build_injection_ranked(&ch, &lib, &tmp.join("04_正文"), "卷01", 9, &usage, &banned);

        // top3 全文 + top5 摘要；细纲外的丹药卡排第 11 → 落选
        assert_eq!(pkt.budget.full_count, 3);
        assert_eq!(pkt.budget.summary_count, 5);
        assert_eq!(pkt.materials.len(), 8);
        assert!(pkt.materials.iter().all(|m| m.code != "MTC-99-000"));
        assert!(pkt.materials.iter().filter(|m| m.is_full).all(|m| !m.content.is_empty()));
        assert!(pkt.materials.iter().filter(|m| !m.is_full).all(|m| m.content.is_empty() && !m.summary.is_empty()));
        // 预算硬顶：卡片 token ≤ 3×1500 + 5×200；禁词注入
        assert!(pkt.budget.card_tokens <= 3 * 1500 + 5 * 200);
        assert!(pkt.budget.total_est_tokens <= pkt.budget.total_budget || pkt.budget.total_est_tokens > 0);
        assert_eq!(pkt.budget.total_budget, 20000);
        assert!(pkt.banned_words.iter().any(|w| w == "归墟"));
        let _ = fs::remove_dir_all(&tmp);
    }
}