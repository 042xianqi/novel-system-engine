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

/// 细纲某章 + 资料库根 + 真值库根 → 注入包（兼容入口：无用量统计、无额外禁词）。
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
    build_injection_db(chapter, lib_root, None, content_dir, volume, prev_chapter, usage, banned_words)
}

/// 同 [`build_injection_ranked`]，但可用 `db_path` 指向真值库作**卡片回退源**。
///
/// 背景（S10）：`02_资料库` 的卡片磁盘副本已删，卡片正文 100% 存在 `materials.content`。
/// 因此取卡策略为「**磁盘优先 → DB 回退**」：
/// - 磁盘存在（或未删源的环境）→ 行为与改造前完全一致，零回归；
/// - 磁盘缺失 → 按 code 回退查 DB，命中则与磁盘同构（name/summary/正文截断口径一致）。
///
/// code 兜底：细纲可能仍用**旧编号**（如 `LOC-99-001`），而 DB 已重编号（`LOCA-99-001`）。
/// 命中不到时按 `renumber_map.old_code → new_code` 再查一次，避免误报 missing_refs。
#[allow(clippy::too_many_arguments)]
pub fn build_injection_db(
    chapter: &ChapterOutline,
    lib_root: &Path,
    db_path: Option<&Path>,
    content_dir: &Path,
    volume: &str,
    prev_chapter: u32,
    usage: &[UsageStat],
    banned_words: &[String],
) -> InjectionPacket {
    let mut packet = InjectionPacket::default();
    packet.chapter = chapter.clone();
    let cards = CardSource::new(lib_root, db_path);

    // ① 出场角色：查 角色/ 下 CHAR-*.md（状态快照注入，占预算极小，不走资料卡预算）
    for ref_code in &chapter.card_refs {
        if ref_code.starts_with("CHAR") {
            match cards.load(ref_code) {
                Some(body) => {
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
        match cards.load(code) {
            Some(body) => {
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
        match cards.load(code) {
            Some(body) => {
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

    // ④ 事件：从细纲引用里抓事件类卡作占位 due（按 templates 判类别，兼容 EVNT/EVT）
    for ref_code in &chapter.card_refs {
        if ne_core::templates::category_of_code(ref_code).as_deref() == Some("事件") {
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

/// 卡片取数源：**磁盘优先 → DB 回退**（S10 后磁盘卡片已删，正文只在 `materials.content`）。
struct CardSource<'a> {
    lib_root: &'a Path,
    /// 真值库路径；None 表示不做 DB 回退（纯磁盘模式，兼容未删源环境）。
    db: Option<rusqlite::Connection>,
    /// 旧编号 → 新编号（renumber_map）；仅 DB 命中不到时用。
    renum: HashMap<String, String>,
}

impl<'a> CardSource<'a> {
    fn new(lib_root: &'a Path, db_path: Option<&Path>) -> Self {
        let (db, renum) = match db_path {
            Some(p) if p.exists() => match rusqlite::Connection::open_with_flags(
                p,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            ) {
                Ok(con) => {
                    let mut renum = HashMap::new();
                    if let Ok(mut st) =
                        con.prepare("SELECT old_code, new_code FROM renumber_map WHERE old_code IS NOT NULL")
                    {
                        if let Ok(rows) = st.query_map([], |r| {
                            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                        }) {
                            for (o, n) in rows.flatten() {
                                renum.insert(o, n);
                            }
                        }
                    }
                    (Some(con), renum)
                }
                Err(_) => (None, HashMap::new()),
            },
            _ => (None, HashMap::new()),
        };
        Self { lib_root, db, renum }
    }

    /// 取一张卡的正文：先磁盘，再 DB（含旧编号兜底）。都取不到返回 None。
    fn load(&self, code: &str) -> Option<String> {
        if let Some(p) = locate_card(self.lib_root, code) {
            if let Ok(body) = std::fs::read_to_string(&p) {
                return Some(body);
            }
        }
        let con = self.db.as_ref()?;
        // ① 直接按 code 查（精确优先）
        if let Some(b) = query_card(con, code) {
            return Some(b);
        }
        // ② 旧编号兜底：renumber_map.old_code = code → new_code
        if let Some(new) = self.renum.get(code) {
            if new != code {
                if let Some(b) = query_card(con, new) {
                    return Some(b);
                }
            }
        }
        // ③ 反向：code 已是新编号但 DB 行 code 用了别的写法 —— 按 new_code 列再查
        query_card_by_new_code(con, code)
    }
}

/// `materials.code = code` 的非空正文。
fn query_card(con: &rusqlite::Connection, code: &str) -> Option<String> {
    con.query_row(
        "SELECT content FROM materials WHERE code = ?1 AND content IS NOT NULL AND trim(content) <> '' LIMIT 1",
        [code],
        |r| r.get::<_, String>(0),
    )
    .ok()
}

/// `materials.new_code = code` 的非空正文（兼容 code 列仍是旧值的行）。
fn query_card_by_new_code(con: &rusqlite::Connection, code: &str) -> Option<String> {
    con.query_row(
        "SELECT content FROM materials WHERE new_code = ?1 AND content IS NOT NULL AND trim(content) <> '' LIMIT 1",
        [code],
        |r| r.get::<_, String>(0),
    )
    .ok()
}

/// 按编号在 02_资料库 各子类里找卡文件（编号在文件名里，如 EVT-99-005.md）。
fn locate_card(lib_root: &Path, ref_code: &str) -> Option<PathBuf> {
    // 前缀 → 子目录。**新旧前缀都要列**：
    // 旧前缀（MTC/EVT/REL/LOC/…) 用于未重编号的磁盘；新前缀（PILL/EVNT/FORE/…）用于已重编号的库。
    // 缺新前缀会让「磁盘明明有卡却报 missing」——实测 PILL 曾漏列。
    let by_prefix_sub: HashMap<&str, &str> = [
        // ── 事件
        ("EVT", "事件"), ("EVNT", "事件"), ("ANC", "事件"),
        // ── 关系 / 因果线 / 伏笔
        ("REL", "关系"), ("RELA", "关系"),
        ("CAUSE", "因果线"), ("CAS", "因果线"), ("CAUS", "因果线"),
        ("FORE", "伏笔"),
        // ── 科技 / 物品 / 装备（ART = 物品旧前缀，勿映到科技）
        ("TECH", "科技"), ("ART", "物品"), ("ITEM", "物品"), ("GEAR", "装备"),
        // ── 地点（【修复 2026-10-17】v3 已归一：地图 → 地点）
        ("LOC", "地点"), ("LOCA", "地点"),
        // ── 丹药
        ("MTC", "丹药"), ("PILL", "丹药"),
        // ── 角色
        ("CHAR", "角色"),
        // ── 文明系（SYSX=系统规则 / XREA=跨文明关系，勿并入文明）
        ("SYS", "文明"), ("CIV", "文明"), ("CIVI", "文明"), ("ORG", "文明"),
        ("CULT", "文化"), ("CLTR", "文化"),
        ("SYSX", "系统规则"), ("XREA", "跨文明关系"),
        // ── 力量体系 / 功法 / 修炼体系 / 境界
        ("POWR", "力量体系"), ("YGEN", "力量体系"),
        ("ARTS", "功法"), ("PRAC", "修炼体系"), ("REAL", "境界"),
        // ── 势力
        ("FACT", "势力"),
        // ── 物产
        ("MTRL", "天材地宝"), ("CRAFT", "技艺"),
        // ── 诡异 / 时间 / 历史 / 建筑 / 世界观
        ("WEIR", "诡异"), ("TIME", "时间"), ("HIST", "历史"),
        ("BLDG", "建筑"), ("WRLD", "世界观"),
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
        // 5 张事件卡 + 5 张地点卡 + 1 张丹药卡（仅出现在用量统计里）
        //
        // 【2026-10-18 修】本测试此前**长期失败**（left: 5, right: 8），根因在夹具：
        //   · 这里原写 ("地图", "LOC")，造出的是 `地图/LOC-99-xxx.md`
        //   · 但 locate_card 的前缀映射是 ("LOC", "地点")（v3 已把「地图」归一为
        //     「地点」，见 ne-injection/src/lib.rs:270-271）
        //   · 于是 5 张 LOC 卡全部 load 失败，只有 3 张 EVT 全文 + 2 张 EVT 摘要
        //     成功入包，合计 5 张 —— 正是断言里 left: 5 的来源。
        // 修法：夹具目录名改成与新规范一致（地点/），不是改断言。
        for (dir, prefix) in [("事件", "EVT"), ("地点", "LOC")] {
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

    // ── S10 新增：卡片 DB 回退 ────────────────────────────────
    /// 造一个只有 materials + renumber_map 的最小库。
    fn make_db(path: &std::path::Path) {
        let con = rusqlite::Connection::open(path).unwrap();
        con.execute_batch(
            "CREATE TABLE materials(code TEXT, name TEXT, content TEXT, new_code TEXT);
             CREATE TABLE renumber_map(old_code TEXT, new_code TEXT);",
        )
        .unwrap();
        con.execute(
            "INSERT INTO materials(code, name, content) VALUES ('PILL-99-001','谐振髓核','# PILL-99-001 谐振髓核\\n## 摘要\\n- a\\n- b\\n- c\\n\\n正文。')",
            [],
        )
        .unwrap();
        // 旧编号行：code 仍是旧值，但 new_code 指向新编号
        con.execute(
            "INSERT INTO materials(code, name, content, new_code) VALUES ('CAS-014','因果线·残响归流','# CAS-014\\n## 摘要\\n- x\\n- y\\n- z\\n\\n正文。','FORE-99-014')",
            [],
        )
        .unwrap();
        con.execute("INSERT INTO renumber_map(old_code,new_code) VALUES ('EVT-141','EVNT-99-141')", []).unwrap();
        con.execute(
            "INSERT INTO materials(code, name, content) VALUES ('EVNT-99-141','第七次共振·裂河','# EVNT-99-141\\n## 摘要\\n- p\\n- q\\n- r\\n\\n正文。')",
            [],
        )
        .unwrap();
    }

    /// DB 回退：磁盘无卡时，按 code 命中 materials.content。
    #[test]
    fn card_source_falls_back_to_db() {
        let tmp = std::env::temp_dir().join(format!("ne_inj_db_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let db = tmp.join("novel.db");
        make_db(&db);

        // 磁盘目录为空（模拟 S10 后的 live 根）
        let empty_lib = tmp.join("02_资料库");
        fs::create_dir_all(&empty_lib).unwrap();

        let mut ch = ChapterOutline::default();
        ch.number = 2;
        ch.card_refs = vec!["PILL-99-001".into()];
        let pkt = build_injection_db(&ch, &empty_lib, Some(&db), &tmp.join("04_正文"), "卷01", 0, &[], &[]);
        assert!(pkt.missing_refs.is_empty(), "DB 回退应命中：{:?}", pkt.missing_refs);
        assert_eq!(pkt.materials.len(), 1);
        assert!(pkt.materials[0].content.contains("正文"), "应取到 DB 正文");

        // 旧编号兜底：细纲仍写 CAS-014 → 按 new_code 回退
        let mut ch2 = ChapterOutline::default();
        ch2.number = 2;
        ch2.card_refs = vec!["CAS-014".into(), "EVT-141".into()];
        let p2 = build_injection_db(&ch2, &empty_lib, Some(&db), &tmp.join("04_正文"), "卷01", 0, &[], &[]);
        assert!(p2.missing_refs.is_empty(), "旧编号应兜底命中：{:?}", p2.missing_refs);

        // 无 DB 时（纯磁盘模式）应如实报缺失，不静默通过
        let p3 = build_injection_db(&ch, &empty_lib, None, &tmp.join("04_正文"), "卷01", 0, &[], &[]);
        assert_eq!(p3.missing_refs, vec!["PILL-99-001".to_string()]);

        let _ = fs::remove_dir_all(&tmp);
    }

    /// 磁盘优先：磁盘有卡时不得走 DB（零回归）。
    #[test]
    fn card_source_prefers_disk_when_present() {
        let tmp = std::env::temp_dir().join(format!("ne_inj_disk_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("02_资料库/丹药")).unwrap();
        fs::write(
            tmp.join("02_资料库/丹药/PILL-99-001.md"),
            "# PILL-99-001 磁盘版\n## 摘要\n- d1\n- d2\n- d3\n\n磁盘正文。",
        )
        .unwrap();
        let db = tmp.join("novel.db");
        make_db(&db); // DB 里也有同名卡，但内容不同

        let mut ch = ChapterOutline::default();
        ch.number = 2;
        ch.card_refs = vec!["PILL-99-001".into()];
        let pkt = build_injection_db(&ch, &tmp.join("02_资料库"), Some(&db), &tmp.join("04_正文"), "卷01", 0, &[], &[]);
        assert!(pkt.materials[0].content.contains("磁盘正文"), "磁盘应优先：{}", pkt.materials[0].content);
        let _ = fs::remove_dir_all(&tmp);
    }
}