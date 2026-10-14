//! ne-mcp 的工具实现：把 ne-* 引擎能力暴露为 MCP 工具。
//!
//! 与 omp-rs-novel 的 5 个工具一一对应（funnel / ne_cardgen / ne_inject /
//! ne_check / ne_db），但以 MCP stdio 会话方式提供，且**直连用户真实 novel.db**。

use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};

use ne_core::{
    data_lib_root, extract_chapter, outline_dir, ChapterOutline, Funnel, InjectionPacket,
};

// ─────────────────────────────────────────── 配置
#[derive(Clone)]
pub struct Cfg {
    /// 资料库根：包含 02_资料库 / 03_叙事规划 / 04_正文 / novel.db 的目录
    pub root: PathBuf,
}

impl Cfg {
    pub fn lib_root(&self) -> PathBuf {
        data_lib_root(self.root.to_str().unwrap_or("."))
    }
    pub fn content_dir(&self) -> PathBuf {
        self.root.join("04_正文")
    }
    pub fn db_path(&self) -> PathBuf {
        self.root.join("novel.db")
    }
    pub fn funnel_path(&self) -> PathBuf {
        self.root.join("05_管线框架").join("funnel.yaml")
    }
}

// ─────────────────────────────────────────── 工具 1：funnel
/// 读 funnel.yaml，报当前待推进的 stage（哪层没 done）。
pub fn funnel(cfg: &Cfg) -> Result<serde_json::Value> {
    let p = cfg.funnel_path();
    let txt = fs::read_to_string(&p)
        .map_err(|e| anyhow::anyhow!("读 funnel.yaml 失败（{}）：{e}", p.display()))?;
    let f = Funnel::parse(&txt)?;
    let pending: Vec<serde_json::Value> = f
        .pending()
        .iter()
        .map(|s| {
            serde_json::json!({
                "id": s.id, "name": s.name, "status": s.status,
                "file": s.file, "dir": s.dir, "deps": s.deps,
            })
        })
        .collect();
    Ok(serde_json::json!({
        "funnel": p.display().to_string(),
        "total_stages": f.layers.len(),
        "pending_count": pending.len(),
        "pending": pending,
        "all": f.layers.iter().map(|s| serde_json::json!({"id": s.id, "status": s.status})).collect::<Vec<_>>(),
    }))
}

/// 已有正文章节号（读 `novel.db` 的 `chapters` 表），升序去重。
///
/// volume 兼容 `01` 与 `卷01` 两种写法。
pub fn generated_chapters(cfg: &Cfg, volume: &str) -> Vec<u32> {
    let con = match rusqlite::Connection::open_with_flags(
        cfg.db_path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let vol_exact = if volume.starts_with('卷') { volume.to_string() } else { format!("卷{volume}") };
    let mut st = match con.prepare(
        "SELECT chapter_no FROM chapters WHERE volume = ?1 OR volume = ?2 ORDER BY chapter_no",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let mut v: Vec<u32> = st
        .query_map(rusqlite::params![vol_exact.as_str(), volume], |r| r.get::<_, i64>(0))
        .map(|rows| rows.flatten().map(|n| n as u32).collect())
        .unwrap_or_default();
    v.sort_unstable();
    v.dedup();
    v
}

/// 章节规划：识别**编号缺口**，给出「补缺口 / 向后续写」两种建议。
///
/// ⚠ 不能简单 `max(chapter_no)+1`：若中间有缺口（实测 卷01 已有第1、3章，
/// **第2章缺**），直接续写会跳过第2章。故默认建议**先补缺口**。
pub fn chapter_plan(cfg: &Cfg, volume: &str) -> serde_json::Value {
    let have = generated_chapters(cfg, volume);
    let max = have.iter().max().copied().unwrap_or(0);
    // 首个缺口：1..=max+1 中第一个未出现的
    let first_gap = (1..=max + 1).find(|n| !have.contains(n));
    let suggest_append = max + 1;
    let has_gap = first_gap.map(|g| g <= max).unwrap_or(false);
    serde_json::json!({
        "volume": volume,
        "generated_chapters": have,
        "generated_count": have.len(),
        "has_gap": has_gap,
        "first_gap": if has_gap { first_gap } else { None },
        "suggest_fill_gap": if has_gap { first_gap } else { None },
        "suggest_append": suggest_append,
        "default_target": first_gap.unwrap_or(suggest_append),
        "note": if has_gap {
            format!("检测到第{}章缺失，默认优先补缺口（mode=fill_gap）；如需跳过缺口向后续写请显式传 chapter={suggest_append}", first_gap.unwrap())
        } else {
            format!("章节连续，建议续写第{suggest_append}章（mode=append）")
        },
    })
}

// ─────────────────────────────────────────── 工具 2：ne_cardgen
/// 抽某卷细纲的【待建卡】工单，按类别归堆。
///
/// 实现对齐 ne-cardgen::extract_card_jobs / group_by_category，
/// 但**去掉了 omp-rs-7k 依赖**（它只为 pick_key_for_job 的一行 lane_key 而引入
/// tokio full + reqwest，属重量级网络栈，与 MCP stdio 服务无关）。
pub fn cardgen(cfg: &Cfg, volume: &str) -> Result<serde_json::Value> {
    let outline = outline_file(cfg, volume)?;
    let text = fs::read_to_string(&outline)
        .map_err(|e| anyhow::anyhow!("读细纲失败（{}）：{e}", outline.display()))?;

    let jobs = extract_card_jobs(&text);
    // 查重（2026-10-13 修）：细纲头部的「无XX卡」注记会过期（卡建了注记没删），
    // 光信注记会误报待建。对 novel.db materials.name + 磁盘卡文件做双向核对，
    // 每张工单标 exists_db / exists_disk，存在即为「注记过期」，无需再建。
    let mut names_db: Vec<(String, String)> = Vec::new();
    if cfg.db_path().exists() {
        if let Ok(con) = rusqlite::Connection::open_with_flags(
            cfg.db_path(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ) {
            if let Ok(mut st) = con.prepare("SELECT code, name FROM materials") {
                if let Ok(rows) = st.query_map([], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                }) {
                    for r in rows.flatten() {
                        names_db.push(r);
                    }
                }
            }
        }
    }
    let lib = cfg.lib_root();
    let mut jobs_json: Vec<serde_json::Value> = Vec::with_capacity(jobs.len());
    for j in &jobs {
        let db_hit: Vec<String> = names_db
            .iter()
            .filter(|(_, n)| n.contains(&j.name) || j.name.contains(n.as_str()))
            .map(|(c, _)| c.clone())
            .collect();
        let disk_hit = {
            let re = regex::Regex::new(r"^([A-Z]+-\d+-\d+)\b").unwrap_or_else(|_| regex::Regex::new(r"$^").unwrap());
            let mut hit = String::new();
            if let Ok(rd) = fs::read_dir(&lib) {
                'outer: for cat in rd.flatten() {
                    let cp = cat.path();
                    if !cp.is_dir() { continue; }
                    if let Ok(files) = fs::read_dir(&cp) {
                        for f in files.flatten() {
                            let fp = f.path();
                            if fp.extension().map(|e| e == "md").unwrap_or(false) {
                                if let Ok(t) = fs::read_to_string(&fp) {
                                    let first = t.lines().next().unwrap_or("");
                                    if first.contains(&j.name) {
                                        let code = re
                                            .captures(&f.file_name().to_string_lossy())
                                            .and_then(|c| c.get(1))
                                            .map(|c| c.as_str().to_string())
                                            .unwrap_or_default();
                                        hit = if code.is_empty() { fp.display().to_string() } else { code };
                                        break 'outer;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            hit
        };
        jobs_json.push(serde_json::json!({
            "name": j.name, "source_ref": j.source_ref, "material": j.material,
            "exists_db": !db_hit.is_empty(), "db_codes": db_hit,
            "exists_disk": !disk_hit.is_empty(), "disk_code": disk_hit,
            "need_create": db_hit.is_empty() && disk_hit.is_empty(),
        }));
    }
    let real_jobs: Vec<&serde_json::Value> =
        jobs_json.iter().filter(|j| j["need_create"] == true).collect();
    let mut by_cat: std::collections::BTreeMap<String, Vec<serde_json::Value>> =
        std::collections::BTreeMap::new();
    for j in &real_jobs {
        let cat = j["material"]
            .as_str()
            .map(|m| if m.contains("角色") { "角色".to_string() }
                else if m.contains("地点") { "地点".to_string() }
                else { "未分类".to_string() })
            .unwrap_or_else(|| "未分类".to_string());
        by_cat.entry(cat).or_default().push((*j).clone());
    }
    Ok(serde_json::json!({
        "outline": outline.display().to_string(),
        "volume": volume,
        "job_count": jobs.len(),
        "need_create_count": real_jobs.len(),
        "stale_note_count": jobs.len() - real_jobs.len(),
        "note": "need_create=DB与磁盘均无卡才为true；exists_db/exists_disk=true 表示细纲注记过期（卡已建），无需再建",
        "categories": by_cat.keys().cloned().collect::<Vec<_>>(),
        "by_category": by_cat,
        "all_jobs": jobs_json,
    }))
}

/// 待建卡工单（等价 ne_cardgen::CardJob）。
#[derive(Debug, Clone, Default)]
pub struct CardJob {
    pub name: String,
    pub source_ref: String,
    pub category: String,
    pub material: String,
}

/// 从细纲文本抽【待建卡】清单。
/// 行格式：`> - 名字（内文）`，只取内文含"无…卡/待建"的（与 ne-cardgen 同口径）。
///
/// **必须带行首 `>` 前缀**（与 ne-cardgen::extract_card_jobs 对齐）：
/// 早先用 `^>?` 把细纲正文里的普通 `- xxx（yyy）` 行也扫进来，
/// 卷01 实测误抽 542 行、真卡仅 9 张（卡名被污染成 `**目标**：把白鸦` 之类）。
pub fn extract_card_jobs(outline_text: &str) -> Vec<CardJob> {
    let re = regex::Regex::new(r"(?m)^>\s*-\s+([^（\n]+)（(.*)）\s*$").unwrap();
    let mut jobs = Vec::new();
    for m in re.captures_iter(outline_text) {
        let name = m.get(1).map(|x| x.as_str().trim()).unwrap_or("");
        let inner = m.get(2).map(|x| x.as_str()).unwrap_or("");
        if !inner.contains("无") && !inner.contains("待建") {
            continue;
        }
        if name.is_empty() {
            continue;
        }
        let (source_ref, material, category) = parse_card_inner(inner);
        jobs.push(CardJob {
            name: name.to_string(),
            source_ref,
            material,
            category,
        });
    }
    // 兜底：本卷用**内联式**标注（`…实体名【…待建卡…】`）而没有 `>` 清单时启用。
    // 实测仅 卷02 是这种格式（全卷 0 行 `>` 清单）。有清单的卷不走这里，口径不变。
    if jobs.is_empty() {
        jobs = inline_card_jobs(outline_text);
    }
    jobs
}

/// 内联式【待建卡】兜底（仅当**无** `>` 清单时启用）。
///
/// 形态：`- **在场**：陈寂、林晚秋、加百列·黑刃【天启帝国核心设定§核心人物，待建卡】`
/// 名字 = 【 前最后一个强分隔段，去尾部括号、去开头虚词，再过**类别白名单**（宁缺毋滥）。
fn inline_card_jobs(outline_text: &str) -> Vec<CardJob> {
    let br = regex::Regex::new(r"【([^】]*待建卡[^】]*)】").unwrap();
    let sep = regex::Regex::new(r#"[、，,：:=（）()\[\]"“”'‘’「」\s]+"#).unwrap();
    let lead = regex::Regex::new(r"^[把将与和及的在向从对为被]+").unwrap();
    let trailer = regex::Regex::new(r"[）)]$").unwrap();
    let role = regex::Regex::new(
        r"(修士|长老|执事|祭司|主教|大祭司|教皇|密探|随员|近侍|骑士|守卒|货主|老主|巡守|录事|验录使|使者|侍从|守卫|军官|统领|大导师|大占星师|大航海士|团主|舵手|掌柜|伙计|大修士|船长)$",
    )
    .unwrap();
    let place = regex::Regex::new(
        r"(墙|街|区|殿|坛|塔|廊|院|海|城|村|镇|山|洞|谷|窟|港|桥|门|冢|渊|废墟|遗址|防线|边界|缓冲带|回廊|祭坛|圣柱)$",
    )
    .unwrap();
    let bad = regex::Regex::new(r#"[§#/+*"“”'‘’（）()\[\]]"#).unwrap();

    let mut out: Vec<CardJob> = Vec::new();
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for m in br.captures_iter(outline_text) {
        let inner = m.get(1).map(|x| x.as_str()).unwrap_or("");
        let whole = m.get(0).unwrap();
        let line_start = outline_text[..whole.start()].rfind('\n').map(|p| p + 1).unwrap_or(0);
        let before = &outline_text[line_start..whole.start()];
        let Some(last) = sep.split(before).filter(|s| !s.trim().is_empty()).last() else {
            continue;
        };
        let name = lead.replace(last.trim(), "").to_string();
        let name = trailer.replace(&name, "").trim().to_string();
        let n = name.chars().count();
        if n < 2 || n > 12 {
            continue;
        }
        if bad.is_match(&name) || name.starts_with('章') {
            continue;
        }
        // 正向白名单：必须是「带·的具名实体」或「…角色词」或「短…地点词」。
        if !(name.contains('·') || role.is_match(&name) || (place.is_match(&name) && n <= 6)) {
            continue;
        }
        if !seen.insert(name.clone()) {
            continue;
        }
        let (source_ref, material, category) = parse_card_inner(inner);
        out.push(CardJob {
            name,
            source_ref,
            material,
            category,
        });
    }
    out
}

/// 从卡面内文解析 (来源引用, 素材, 类别)。
fn parse_card_inner(inner: &str) -> (String, String, String) {
    // 类别：优先取明确的“X类”标记，其次按已知类别名兜底
    let cat_re = regex::Regex::new(r"(角色|势力|地点|物品|文化|技艺|丹药|科技|地图|事件|历史|时间|生物|组织|制度|民俗|语言|货币|历法|传说|修炼)类?").unwrap();
    let category = cat_re
        .captures(inner)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .unwrap_or_default();
    // 来源引用：形如 REL-99-004 / CHAR-01-003
    let ref_re = regex::Regex::new(r"\b((?:CHAR|EVT|LOC|REL|ART|ORG|TECH|SYS|CULT|MTC|CAS|TIME|ANC|YGEN|OTHER)-\d+(?:-\d+)?)").unwrap();
    let source_ref = ref_re
        .captures(inner)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .unwrap_or_default();
    // 素材：去掉“无…卡/待建”提示后的剩余描述
    let material = regex::Regex::new(r"无[^，,、；;\s]*卡|待建|无编号|无独立|无 ?LOC|无 ?EVT")
        .unwrap()
        .replace_all(inner, "")
        .trim_matches(|c: char| c.is_whitespace() || c == '，' || c == ',' || c == '、' || c == ';' || c == '；')
        .to_string();
    (source_ref, material, category)
}

// ─────────────────────────────────────────── 工具 3：ne_inject
/// 细纲某章 → 注入包（出场角色 / 关联资料 / 前文 / 伏笔 / 事件 / 缺失引用）。
pub fn inject(
    cfg: &Cfg,
    volume: &str,
    chapter: u32,
    allow_missing: bool,
) -> Result<serde_json::Value> {
    let outline = outline_file(cfg, volume)?;
    let text = fs::read_to_string(&outline)
        .map_err(|e| anyhow::anyhow!("读细纲失败（{}）：{e}", outline.display()))?;
    let ch: ChapterOutline = extract_chapter(&text, chapter).ok_or_else(|| {
        anyhow::anyhow!("细纲里切不到第{chapter}章（卷{volume}）：{}", outline.display())
    })?;

    // 两层选卡需要用量统计；库不存在则优雅降级（空统计）
    let usage = load_usage_stats(cfg).unwrap_or_default();
    let banned: Vec<String> = ne_qc::BANNED_WORDS.iter().map(|s| s.to_string()).collect();
    let prev = chapter.saturating_sub(1);

    let packet: InjectionPacket = ne_injection::build_injection_db(
        &ch,
        &cfg.lib_root(),
        Some(&cfg.db_path()),
        &cfg.content_dir(),
        volume,
        prev,
        &usage,
        &banned,
    );

    // 缺失引用：如实列出（不杜撰）。allow_missing=false 时视为硬失败。
    let missing = packet.missing_refs.clone();
    let card_source = if cfg.lib_root().exists() { "disk_first_db_fallback" } else { "db_only" };
    if !missing.is_empty() && !allow_missing {
        return Ok(serde_json::json!({
            "ok": false,
            "reason": "存在库中缺失的引用（如需继续请传 allow_missing=true）",
            "chapter": chapter, "volume": volume,
            "card_source": card_source,
            "missing_refs": missing,
            "chapter_plan": chapter_plan(cfg, volume),
        }));
    }

    Ok(serde_json::json!({
        "ok": true,
        "volume": volume, "chapter": chapter, "title": ch.title,
        "scene_type": format!("{:?}", ch.scene_type),
        "characters": ch.characters,
        "card_refs": ch.card_refs,
        "card_source": card_source,
        "missing_refs": missing,
        "chapter_plan": chapter_plan(cfg, volume),
        "packet": packet,
    }))
}

/// 读卡用量统计（card_usage 表）；库不存在返回 Err。
/// 注意：不用 ne_db::connect（它会跑 CREATE TABLE，且 usage_stats 的 foreshadow
/// 字段口径与真实库不一致会整体 Err）——这里只读直取，失败则退化为空统计。
fn load_usage_stats(cfg: &Cfg) -> Result<Vec<ne_core::UsageStat>> {
    let db = cfg.db_path();
    if !db.exists() {
        anyhow::bail!("novel.db 不存在：{}", db.display());
    }
    let con = open_ro(&db)?;
    Ok(raw_card_usage(&con).unwrap_or_default())
}

// ─────────────────────────────────────────── 工具 4：ne_check
/// 对正文跑 **16 道门禁**（`ne_engine::gates::run_all`，唯一权威实现）。
///
/// ⚠ 历史问题：本函数此前走 `ne_qc::validate_six` + `ne_qc::gate14`，
/// 与 `ne-engine check-file` 是**两条独立代码路径**，且 ne-qc 那套更宽松
/// （实测同一章：CLI 判 `fix`（2 处 P1），MCP 判 `Pass`）。
/// 现统一委托 ne-engine，保证 MCP 与 CLI 结果一致。
///
/// 附带保留六维 AI 味质检（`validate_six`）作为**附加维度**输出，
/// 不作为 verdict 依据 —— verdict 只认 16 门禁。
pub fn check(cfg: &Cfg, text: &str, chapter: u32, volume: &str) -> Result<serde_json::Value> {
    let conf = ne_engine::config::load(&cfg.root)?;
    let ctx = ne_engine::ctx::RepoCtx::open(&cfg.root)?;
    let r = ne_engine::gates::run_all(text, chapter, volume, &conf, &ctx);

    // 附加：六维 AI 味（仅提示，不影响 verdict）
    let banned: Vec<String> = ne_qc::BANNED_WORDS.iter().map(|s| s.to_string()).collect();
    let six = ne_qc::validate_six(text, None, &banned);

    let gates: serde_json::Map<String, serde_json::Value> = r
        .gates
        .iter()
        .map(|(k, g)| {
            (
                k.clone(),
                serde_json::json!({"pass": g.pass, "detail": g.detail}),
            )
        })
        .collect();

    Ok(serde_json::json!({
        "verdict": r.verdict,
        "chapter": chapter,
        "volume": volume,
        "stats": {"words": r.stats.words, "gates_run": r.stats.gates_run},
        "gates": gates,
        "p0": r.p0.iter().map(|f| serde_json::json!({
            "gate": f.gate, "issue": f.issue, "action": f.action,
        })).collect::<Vec<_>>(),
        "p1": r.p1.iter().map(|f| serde_json::json!({
            "gate": f.gate, "issue": f.issue, "action": f.action,
        })).collect::<Vec<_>>(),
        // 附加维度（不参与 verdict）
        "six_dim_ai_tone": {
            "verdict": format!("{:?}", six.verdict),
            "findings": six.findings.iter().map(|f| serde_json::json!({
                "dim": f.dim, "issue": f.issue, "severity": format!("{:?}", f.severity),
            })).collect::<Vec<_>>(),
        },
    }))
}

// ─────────────────────────────────────────── 工具 5：ne_db
/// 连 novel.db，报各表行数 + 卡用量统计。
pub fn db_status(cfg: &Cfg) -> Result<serde_json::Value> {
    let db = cfg.db_path();
    // 只读打开：本工具仅查询，绝不该改用户资料库。
    let con = open_ro(&db)?;
    let counts = ne_db::table_counts(&con).unwrap_or_default();
    // usage_stats 依赖 ne-db 自己的 foreshadow 口径（fs_id/plant_chapter/status='open'），
    // 用户真实库的 foreshadow 是另一套字段（id/planted_chapter/status='pending'），
    // 直接调用会整条 Err。这里降级为直读 card_usage，保证数据可用且不误报 0。
    let (usage, usage_note) = match ne_db::usage_stats(&con) {
        Ok(u) if !u.is_empty() => (u, None),
        _ => (
            raw_card_usage(&con).unwrap_or_default(),
            Some("ne-db 的 foreshadow 字段口径与真实库不一致，已降级为直读 card_usage"),
        ),
    };
    let mut out = serde_json::json!({
        "db": db.display().to_string(),
        "table_counts": counts.iter().map(|(t, n)| serde_json::json!({"table": t, "rows": n})).collect::<Vec<_>>(),
        "card_usage_top": usage.iter().take(15).map(|u| serde_json::json!({
            "code": u.code, "ref_count": u.ref_count, "last_chapter": u.last_chapter,
        })).collect::<Vec<_>>(),
        "card_usage_total": usage.len(),
    });
    if let Some(n) = usage_note {
        out["note"] = serde_json::json!(n);
    }
    Ok(out)
}

/// 直读 card_usage（不依赖 ne-db 的 foreshadow 口径）。
fn raw_card_usage(con: &rusqlite::Connection) -> Result<Vec<ne_core::UsageStat>> {
    let mut st = con.prepare("SELECT code, ref_count, last_chapter FROM card_usage")?;
    let rows = st.query_map([], |r| {
        Ok(ne_core::UsageStat {
            code: r.get::<_, String>(0)?,
            ref_count: r.get::<_, i64>(1).unwrap_or(0).max(0) as u32,
            last_chapter: r.get::<_, i64>(2).unwrap_or(0).max(0) as u32,
            foreshadow_due: false,
        })
    })?;
    Ok(rows.flatten().collect())
}

/// 只读打开数据库（禁止任何写入副作用）。
const SQLITE_OPEN_READONLY: i32 = 0x0000_0001;

fn open_ro(db: &Path) -> Result<rusqlite::Connection> {
    use rusqlite::OpenFlags;
    let flags = OpenFlags::from_bits_truncate(SQLITE_OPEN_READONLY);
    Ok(rusqlite::Connection::open_with_flags(db, flags)?)
}

/// 只读 SQL 查询（含 FTS5 检索）。写操作一律拒绝。
pub fn db_query(cfg: &Cfg, sql: &str, limit: usize) -> Result<serde_json::Value> {
    let s = sql.trim().trim_end_matches(';');
    let low = s.to_lowercase();
    let first = low.trim_start();
    let is_read = first.starts_with("select")
        || first.starts_with("with")
        || first.starts_with("pragma")
        || first.starts_with("explain");
    let forbidden = [
        "insert ", "update ", "delete ", "drop ", "alter ", "create ", "replace ",
        "attach ", "detach ", "vacuum ", "reindex ",
    ];
    if !is_read || forbidden.iter().any(|k| low.contains(k)) {
        anyhow::bail!("拒绝执行：本工具只允许只读 SELECT / PRAGMA / EXPLAIN 查询");
    }

    let db = cfg.db_path();
    let con = open_ro(&db)?;
    let mut stmt = con.prepare(s)?;
    let col_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let ncol = col_names.len();
    let mut rows_out: Vec<Vec<serde_json::Value>> = Vec::new();
    let rows = stmt.query_map([], |r| {
        let mut vals = Vec::with_capacity(ncol);
        for i in 0..ncol {
            let v: rusqlite::types::Value = r.get(i).unwrap_or(rusqlite::types::Value::Null);
            vals.push(match v {
                rusqlite::types::Value::Null => serde_json::Value::Null,
                rusqlite::types::Value::Integer(n) => serde_json::json!(n),
                rusqlite::types::Value::Real(f) => serde_json::json!(f),
                rusqlite::types::Value::Text(t) => serde_json::json!(t),
                rusqlite::types::Value::Blob(_) => serde_json::json!("<blob>"),
            });
        }
        Ok(vals)
    })?;
    for r in rows {
        if rows_out.len() >= limit {
            break;
        }
        rows_out.push(r?);
    }
    Ok(serde_json::json!({
        "columns": col_names,
        "rows": rows_out,
        "row_count": rows_out.len(),
        "limit": limit,
    }))
}

// ─────────────────────────────────────────── 辅助
fn outline_file(cfg: &Cfg, volume: &str) -> Result<PathBuf> {
    let dir = outline_dir(cfg.root.to_str().unwrap_or("."));
    // 细纲命名有多种历史口径，按序探测
    let candidates = [
        dir.join(format!("卷{volume}.md")),
        dir.join(format!("卷{volume}·细纲.md")),
        dir.join(format!("卷{}.md", volume.trim_start_matches('0'))),
    ];
    for c in candidates.iter() {
        if c.exists() {
            return Ok(c.clone());
        }
    }
    // 兜底：目录里找含 volume 的 .md
    if let Ok(rd) = fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name.ends_with(".md") && name.contains(volume.trim_start_matches('0')) && !volume.is_empty() {
                return Ok(p);
            }
        }
    }
    anyhow::bail!("找不到卷{volume}的细纲（目录：{}）", dir.display())
}

/// 确保 Path 可显示为 str（辅助）。
#[allow(dead_code)]
pub fn pstr(p: &Path) -> String {
    p.to_string_lossy().to_string()
}

// ─────────────────────────────────────────── 工具 7：ne_materials（L2→L4 资料对账）
/// L1→L4 资料整理：逐卷读 L2 分卷大纲，抽卡引用（[A-Z]+-\d+-\d+ 编号 + 中文卡名），
/// 对 novel.db materials 表对账，按卷报「已建实卡 / 缺失卡」，并汇总全库缺口。
///
/// 与 calc_materials.py 的区别：不依赖作废的「汐潮」旧口径硬编码，直接以
/// 现行 L2 分卷大纲（故事流口径，卷01=太玄帝国）为事实源。
pub fn materials_audit(cfg: &Cfg, volume: &str) -> Result<serde_json::Value> {
    use std::collections::BTreeMap;
    let db = cfg.db_path();
    if !db.exists() {
        anyhow::bail!("novel.db 不存在（{}）", db.display());
    }
    let con = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;

    // 编号 → (name, category, file_path, stale)，一次全量载入内存（635 行，可忽略）
    let mut codes: std::collections::HashMap<String, (String, String, String, bool)> =
        std::collections::HashMap::new();
    {
        let mut st = con.prepare("SELECT code, name, category, file_path, stale FROM materials")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4).unwrap_or(0) != 0,
            ))
        })?;
        for r in rows {
            let (code, name, cat, fp, stale) = r?;
            codes.insert(code, (name, cat, fp, stale));
        }
    }

    // 磁盘 02_资料库 实卡清单（编号 → 路径），供「DB 有但磁盘无」双向对账
    let lib = cfg.lib_root();
    let mut disk: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let code_re = regex::Regex::new(r"^([A-Z]+-\d+-\d+)\b")?;
    if let Ok(rd) = fs::read_dir(&lib) {
        for e in rd.flatten() {
            let cat_dir = e.path();
            if !cat_dir.is_dir() {
                continue;
            }
            if let Ok(sub) = fs::read_dir(&cat_dir) {
                for f in sub.flatten() {
                    let p = f.path();
                    if p.extension().and_then(|x| x.to_str()) != Some("md") {
                        continue;
                    }
                    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                    if let Some(c) = code_re.captures(stem).and_then(|c| c.get(1)) {
                        disk.entry(c.as_str().to_string())
                            .or_insert_with(|| p.display().to_string());
                    }
                    // 子目录再下一层（如 角色/终极篇/CHAR-99-001.md）
                    if p.is_dir() {
                        if let Ok(sub2) = fs::read_dir(&p) {
                            for f2 in sub2.flatten() {
                                let p2 = f2.path();
                                if p2.extension().and_then(|x| x.to_str()) != Some("md") {
                                    continue;
                                }
                                let stem2 = p2.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                                if let Some(c) = code_re.captures(stem2).and_then(|c| c.get(1)) {
                                    disk.entry(c.as_str().to_string())
                                        .or_insert_with(|| p2.display().to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // 决定扫哪些卷：传了 volume 就只对那一卷，否则 01..=10 全扫
    let vols: Vec<String> = if volume.trim().is_empty() {
        (1..=10).map(|i| format!("{i:02}")).collect()
    } else {
        vec![volume.to_string()]
    };

    // 引用抽取：编号（A-\d+-\d+）+ 兜底【待建卡】工单（复用 cardgen 口径）
    let ref_re = regex::Regex::new(r"([A-Z]{3,}[A-Z]*-\d+-\d+)")?;
    let code_class: BTreeMap<&str, &str> = BTreeMap::from([
        ("CHAR", "角色"), ("CIVI", "文明"), ("CIV", "文明"),
        ("LOCA", "地点"), ("LOC", "地点"),
        ("ORG", "势力"), ("EVNT", "事件"), ("FORE", "伏笔"),
        ("PILL", "丹药"), ("ITEM", "物品"), ("ART", "科技"),
        ("TECH", "科技"), ("TIME", "时间线"), ("REALM", "境界"),
        ("BEAS", "妖兽"), ("PLANT", "植物"), ("CULT", "风俗文化"),
    ]);
    let cat_of = |code: &str| -> String {
        let cls = code.split('-').next().unwrap_or("");
        code_class.get(cls).map(|s| s.to_string()).unwrap_or_else(|| "其他".to_string())
    };

    let mut per_volume: Vec<serde_json::Value> = Vec::new();
    let mut total_missing = 0usize;

    for v in &vols {
        // 分卷大纲文件：03_叙事规划/分卷大纲/卷NN·卷主体故事.md（兜底模糊匹配）
        let voldir = cfg.root.join("03_叙事规划").join("分卷大纲");
        let canonical = voldir.join(format!("卷{v}·卷主体故事.md"));
        let outline_path = if canonical.exists() {
            canonical
        } else if let Ok(rd) = fs::read_dir(&voldir) {
            let mut found = None;
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.ends_with(".md") && name.contains(v.trim_start_matches('0')) {
                    found = Some(e.path());
                    break;
                }
            }
            match found {
                Some(p) => p,
                None => anyhow::bail!("找不到卷{v}的分卷大纲（目录：{}）", voldir.display()),
            }
        } else {
            anyhow::bail!("读分卷大纲目录失败：{}", voldir.display());
        };
        let text = fs::read_to_string(&outline_path)
            .map_err(|e| anyhow::anyhow!("读分卷大纲失败（{}）：{e}", outline_path.display()))?;

        // 抽引用：编号去重保序
        let mut seen: Vec<String> = Vec::new();
        for c in ref_re.captures_iter(&text) {
            let code = c.get(1).map(|x| x.as_str().to_string()).unwrap_or_default();
            if !seen.contains(&code) {
                seen.push(code);
            }
        }

        let mut ok: Vec<serde_json::Value> = Vec::new();
        let mut missing: Vec<serde_json::Value> = Vec::new();
        for code in &seen {
            match codes.get(code) {
                Some((name, cat, fp, stale)) => ok.push(serde_json::json!({
                    "code": code, "name": name, "category": cat,
                    "db": true, "disk": disk.contains_key(code),
                    "stale": stale,
                })),
                None => missing.push(serde_json::json!({
                    "code": code, "category": cat_of(code), "db": false,
                    "disk": disk.contains_key(code),
                })),
            }
        }
        total_missing += missing.len();
        per_volume.push(serde_json::json!({
            "volume": format!("卷{v}"),
            "outline": outline_path.display().to_string(),
            "ref_count": seen.len(),
            "existing_count": ok.len(),
            "missing_count": missing.len(),
            "existing": ok,
            "missing": missing,
        }));
    }

    // DB/磁盘 双向漂移：DB 有但磁盘无文件（除 stale 外均视为漂移）
    let mut drift: Vec<serde_json::Value> = Vec::new();
    for (code, (name, cat, fp, stale)) in codes.iter() {
        if !disk.contains_key(code) && !stale {
            drift.push(serde_json::json!({"code": code, "name": name, "category": cat, "file_path": fp}));
    }
    }

    Ok(serde_json::json!({
        "volumes_scanned": vols.len(),
        "volumes": per_volume,
        "total_missing": total_missing,
        "db_disk_drift_count": drift.len(),
        "db_disk_drift_sample": drift.into_iter().take(20).collect::<Vec<_>>(),
        "note": "口径：L2 分卷大纲为引用事实源；missing=大纲引用但 DB 无卡；drift=DB 有卡但磁盘无文件",
    }))
}

#[cfg(test)]
mod cardgen_tests {
    use super::extract_card_jobs;

    #[test]
    fn list_style_requires_quote_prefix() {
        // 只有带 `>` 前缀的清单行算待建卡；正文普通列表行不算。
        let t = "> - 白鸦（REL-99-004 影阁内门，无角色卡）\n- **钩子**：引到白鸦（柒玖）【待建卡·REL-99-004】\n";
        let jobs = extract_card_jobs(t);
        let names: Vec<&str> = jobs.iter().map(|j| j.name.as_str()).collect();
        assert_eq!(names, vec!["白鸦"], "实际：{names:?}");
    }

    #[test]
    fn inline_style_fallback_only_when_no_list() {
        // 无 `>` 清单 → 启用内联兜底，只收「带·具名 / 角色词 / 短地点词」。
        let t = "- **在场**：陈寂、加百列·黑刃【天启帝国核心设定§核心人物，待建卡】、观测仪值守修士【待建卡】、仅登记【待建卡】\n";
        let jobs = extract_card_jobs(t);
        let names: Vec<&str> = jobs.iter().map(|j| j.name.as_str()).collect();
        assert!(names.contains(&"加百列·黑刃"), "缺具名实体：{names:?}");
        assert!(names.contains(&"观测仪值守修士"), "缺角色词实体：{names:?}");
        assert!(!names.contains(&"仅登记"), "噪声「仅登记」应被白名单过滤：{names:?}");
    }

    #[test]
    fn list_style_wins_over_inline() {
        // 有 `>` 清单时**不**走内联兜底（口径互斥，避免重复/污染）。
        let t = "> - 白鸦（REL-99-004，无角色卡）\n- **在场**：加百列·黑刃【天启帝国核心设定§核心人物，待建卡】\n";
        let jobs = extract_card_jobs(t);
        let names: Vec<&str> = jobs.iter().map(|j| j.name.as_str()).collect();
        assert_eq!(names, vec!["白鸦"], "实际：{names:?}");
    }
}
