//! ne-mcp 的工具实现：把 ne-* 引擎能力暴露为 MCP 工具。
//!
//! 与 omp-rs-novel 的 5 个工具一一对应（funnel / ne_cardgen / ne_inject /
//! ne_check / ne_db），但以 MCP stdio 会话方式提供，且**直连用户真实 novel.db**。

use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};

/// 真实 LLM 外呼的**文件开关锁**：存在即解锁，`rm` 即上锁。
/// 比环境变量更适合"运行时手动授权"（env 需重启进程，file 即时生效）。
/// 路径放在 /sdcard（共享盘），确保 shell 侧 touch 与 bridge 拉起的 ne-mcp 都能看到——
/// /tmp 在 proot 与 bridge 可能不是同一实例，故不用 `/tmp`。
pub const ALLOW_FILE: &str = "/sdcard/wogov_real_llm.allow";

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
    // 光信注记会误报待建。对 novel.db 索引 + 磁盘卡文件做双向核对，
    // 每张工单标 exists_db / exists_disk，存在即为「注记过期」，无需再建。
    //
    // 【2026-10-17 修 P0 · db_hit 恒空】
    // 旧实现只读 `SELECT code, name FROM materials`。实测该表 name 列全部只存编号
    // （'CHAR-90-041' 而非真名），导致 `n.contains(&j.name)` 永远为 false，
    // db_hit 恒为空 → ne_cardgen 永远误报「库中没有这张卡」。
    // 改为三源合并（结果按 code 去重），任一命中即算「库中有卡」：
    //   ① library_index.title 的「｜」后真名（636/665 条带真名，最可靠）
    //   ② materials.name（修索引后已与 ① 对齐，保留作冗余）
    //   ③ library_index.title 里「（别名：X|Y）」的别名（解决「苍牙·烈骨」这类
    //      title 写职位、真名只在卡内 `* **本名**：` 字段的情况）
    let mut names_db: Vec<(String, String)> = Vec::new();
    let mut seen_name_keys: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();
    let mut push_name = |code: &str, name: &str| {
        let k = (code.to_string(), name.to_string());
        if seen_name_keys.insert(k) {
            names_db.push((code.to_string(), name.to_string()));
        }
    };
    if cfg.db_path().exists() {
        if let Ok(con) = rusqlite::Connection::open_with_flags(
            cfg.db_path(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ) {
            // ① + ③ library_index：key + title（title = '<code>｜<真名>' 或含别名）
            if let Ok(mut st) = con.prepare("SELECT key, title FROM library_index") {
                if let Ok(rows) = st.query_map([], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                }) {
                    for (key, title) in rows.flatten() {
                        let name = title
                            .split_once('｜')
                            .map(|(_, n)| n.to_string())
                            .unwrap_or_else(|| title.clone());
                        push_name(&key, &name);
                        // ③ 拆「（别名：A|B）」，把每个别名也登记为可命中项
                        if let Some(pos) = name.find("（别名：") {
                            let alias_part = &name[pos + "（别名：".len()..];
                            let alias_part = alias_part.trim_end_matches('）');
                            for a in alias_part.split('|') {
                                let a = a.trim();
                                if !a.is_empty() {
                                    push_name(&key, a);
                                }
                            }
                        }
                    }
                }
            }
            // ② materials.name（冗余源，修索引后已与①对齐）
            if let Ok(mut st) = con.prepare("SELECT code, name FROM materials") {
                if let Ok(rows) = st.query_map([], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                }) {
                    for (code, name) in rows.flatten() {
                        push_name(&code, &name);
                    }
                }
            }
        }
    }
    drop(push_name);
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

// ─────────────────────────────────────────── 工具 16：ne_run_chapter（写类 · 路径A）
/// L5 章节生成派发（**写类工具**）。
///
/// # 设计口径（与 task_store 的"只登记不执行"相反）
/// 本工具是 MCP 侧的**同步执行入口**：接收参数 → 拉起 `python3 l5.py` → 回传 stdout/状态。
/// 用的是已跑通闭环的 Python 脚本，不做 Rust 重写（决策 B）。
///
/// # 双保险（对齐三重锁思路）
/// - `real=false`（默认）→ 走 `l5.py dryrun`，**0 真生成 0 写 04**，只出注入指标 + 14 门禁。
/// - `real=true` → 才走 `l5.py auto`（真出网）。但**再加一道文件开关锁**：
///   必须存在 `/tmp/wogov_real_llm.allow` 才放行，否则直接拒绝（不静默降级）。
///   运行时 `touch /tmp/wogov_real_llm.allow` 解锁，`rm` 即上锁——比环境变量更适合"手动授权"。
///
/// # 参数
/// - `volume`：卷号（"01" / "卷01" / 数字 1 均可，经 norm_volume 归一）
/// - `chapter`：章号（整数）
/// - `real`：是否真实调 LLM（默认 false）
/// - `timeout_sec`：子进程超时（默认 300，上限 900）
pub fn run_chapter(
    cfg: &Cfg,
    volume: &str,
    chapter: u32,
    real: bool,
    timeout_sec: u32,
) -> Result<serde_json::Value> {
    use std::process::Command;

    // 参数归一：空卷/空章直接拒
    let vol = if volume.trim().is_empty() {
        "卷01".to_string()
    } else {
        volume.trim().to_string()
    };
    let vol_arg = if vol.starts_with('卷') { vol.clone() } else { format!("卷{vol}") };
    if chapter == 0 {
        anyhow::bail!("chapter 必须 ≥ 1（收到 0）");
    }
    let t = timeout_sec.clamp(10, 900);

    // ── 真实外呼的文件开关锁（不绕过三重锁；比 env 更适合运行时手动授权）──
    let allow_file = std::path::Path::new(ALLOW_FILE);
    let env_allow = ne_agent::governance::llm_gate::env_allowed();
    let file_allow = allow_file.exists();
    if real && !file_allow {
        return Ok(serde_json::json!({
            "ok": false,
            "dispatched": false,
            "real": true,
            "blocked_reason": "Real LLM is locked. Touch /sdcard/wogov_real_llm.allow to unlock.",
            "hint": "先用 real=false 空跑验证链路；确认无误后手动 `touch /sdcard/wogov_real_llm.allow` 解锁，`rm` 即上锁。",
            "lock": {
                "file": ALLOW_FILE,
                "file_exists": false,
                "env_WOGOV_ALLOW_REAL_LLM": env_allow,
            },
        }));
    }

    // l5.py 位置（相对资料库根：05_管线框架/l5.py）
    let l5 = cfg.root.join("05_管线框架").join("l5.py");
    if !l5.exists() {
        anyhow::bail!("l5.py 不存在：{}（请确认资料库根）", l5.display());
    }

    let (subcmd, extra): (&str, Vec<String>) = if real {
        ("auto", vec![
            "--volume".into(), vol_arg.clone(),
            "--repair".into(), "2".into(),
        ])
    } else {
        ("dryrun", vec!["--volume".into(), vol_arg.clone()])
    };

    let mut cmd = Command::new("python3");
    cmd.arg(&l5).arg(subcmd).arg(chapter.to_string());
    for e in &extra {
        cmd.arg(e);
    }
    cmd.current_dir(cfg.root.join("05_管线框架"));

    let started = std::time::Instant::now();
    let out = cmd
        .output()
        .map_err(|e| anyhow::anyhow!("拉起 l5.py 失败：{e}（python3 是否在 PATH？）"))?;

    // 超时用简化处理：output() 是阻塞的，这里只报告耗时是否超阈值（真正的 kill 需 spawn+wait_timeout）
    let elapsed = started.elapsed();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let code = out.status.code().unwrap_or(-1);
    let timed_out = elapsed > std::time::Duration::from_secs(t as u64);

    // 截断超长输出（MCP 响应体不宜过大）
    let clip = |s: &str, n: usize| -> String {
        if s.chars().count() <= n { s.to_string() } else {
            let head: String = s.chars().take(n).collect();
            format!("{head}\n…（已截断，共 {} 字符）", s.chars().count())
        }
    };

    Ok(serde_json::json!({
        "ok": code == 0,
        "dispatched": true,
        "real": real,
        "volume": vol_arg,
        "chapter": chapter,
        "subcmd": subcmd,
        "exit_code": code,
        "elapsed_sec": elapsed.as_secs_f64().round(),
        "timeout_sec": t,
        "timed_out": timed_out,
        "lock": {
            "file": ALLOW_FILE,
            "file_exists": file_allow,
            "env_WOGOV_ALLOW_REAL_LLM": env_allow,
        },
        "wrote_content": real,
        "stdout_tail": clip(&stdout, 6000),
        "stderr_tail": if stderr.trim().is_empty() { String::new() } else { clip(&stderr, 2000) },
        "note": if real {
            "已真实调用 l5.py auto（生成+质检+修复+写入+入库）"
        } else {
            "空跑：l5.py dryrun（0 真生成 0 写 04_正文）"
        },
    }))
}

// ─────────────────────────────────────────── 工具 17/18：建卡桥接（2026-10-17 P0）
//
// # 为什么不重写 Rust
// `crates/ne-cardgen` 的 `llm_fill_card_stub()` 至今是 TODO 占位（M2 骨架）。
// 补成能真出卡的实现 = 把 418 行 Python 重写一遍，还要重踩「规范 v1.0 四区块 /
// 26 类必填关联矩阵 / 编号分配 / 重名检测 / 三同步落库」的全部坑。
// 已跑通件在 `05_管线框架/cardgen.py`，这里只做**薄桥接**，与 `run_chapter` 同构。
//
// # 为什么必须异步
// `handle()` → `tool_call()` 是单线程同步的。`run_chapter` 用 `cmd.output()` 阻塞，
// 那是因为单章 dryrun 是秒级。建 10 张卡 = 10–30 分钟，若也阻塞：
//   · 这段时间 MCP 会话完全无法响应任何其他工具（funnel/ne_check/wg_* 全卡死）
//   · 客户端（Operit / 太墟）大概率先超时断开
// 故 `ne_cardgen_run` 默认 `Command::spawn()` 不 `wait()`，立即返回 task_id。

/// 建卡的**全局单飞锁**：存在即表示已有一个建卡进程在跑。
///
/// 防两件事：① 并发建卡撞号（`next_code()` 按 MAX+1 分配，两个进程会拿到同一个号）
/// ② LLM 接口 429 限流（本地网关并发能力有限）。
///
/// 用 `/sdcard` 而非 `/tmp`：proot 与 bridge 可能不是同一实例（与 ALLOW_FILE 同理）。
/// 锁文件里写子进程 pid；判活看 `/proc/<pid>`，进程死了锁自动失效（防僵尸锁）。
pub const CARDGEN_LOCK: &str = "/sdcard/wogov_cardgen.lock";

/// 读锁文件里的 pid。空/非法 → None。
fn read_lock_pid(p: &Path) -> Option<u32> {
    let s = fs::read_to_string(p).ok()?;
    s.trim().parse::<u32>().ok()
}

/// 进程是否还活着（Linux /proc 判活）。
fn proc_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// 尝试拿单飞锁。成功返回 Ok(())；已有活进程在跑返回 Err(现有 task/pid 描述)。
///
/// 僵尸锁自愈：锁文件存在但 pid 为空或进程已死 → 视为失效，删掉重拿。
fn acquire_cardgen_lock() -> Result<(), String> {
    let p = Path::new(CARDGEN_LOCK);
    if p.exists() {
        match read_lock_pid(p) {
            Some(pid) if proc_alive(pid) => {
                return Err(format!(
                    "已有建卡进程在跑（pid={pid}，锁 {CARDGEN_LOCK}）。\
                     同一时刻只允许 1 个建卡任务，避免撞号与 LLM 限流。\
                     等它跑完，或确认已死后 `rm {CARDGEN_LOCK}`。"
                ));
            }
            _ => {
                // 僵尸锁：进程已死或 pid 为空 → 清理后重拿
                let _ = fs::remove_file(p);
            }
        }
    }
    // create_new 是原子的：并发下只有一个能成功
    match fs::OpenOptions::new().write(true).create_new(true).open(p) {
        Ok(mut f) => {
            use std::io::Write;
            let _ = f.write_all(b"0"); // 先占位，spawn 拿到 pid 后再覆写
            Ok(())
        }
        Err(e) => Err(format!("拿建卡锁失败（{}）：{e}", CARDGEN_LOCK)),
    }
}

/// spawn 后把子进程 pid 写进锁文件（供后续判活）。
fn stamp_lock_pid(pid: u32) {
    let _ = fs::write(CARDGEN_LOCK, pid.to_string().as_bytes());
}

/// 释放锁（仅在 spawn 失败等需要回滚的场景显式调用；正常路径由 cardgen.py 结束后自然失效）。
fn release_cardgen_lock() {
    let _ = fs::remove_file(CARDGEN_LOCK);
}

/// 生成 webui_task 主键（与 ne-agent `task_store::new_id` 同构：纳秒 + 进程内自增）。
fn new_task_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("wt-{nanos:x}-{seq:x}")
}

/// 在 novel.db 里登记一条 `webui_task` 记录（state=pending/running）。
///
/// 表结构与 `ne-agent/governance/task_store.rs` 完全一致（CREATE IF NOT EXISTS 兜底，
/// 两处都可建表，谁先跑谁建）。写失败不让建卡失败——它只是"看板"，不是关键路径。
fn register_cardgen_task(
    cfg: &Cfg,
    task_id: &str,
    civ: &str,
    spec: &str,
    real: bool,
    state: &str,
) {
    let Ok(con) = rusqlite::Connection::open(cfg.db_path()) else {
        return;
    };
    let _ = con.execute(
        "CREATE TABLE IF NOT EXISTS webui_task (
            id          TEXT PRIMARY KEY,
            kind        TEXT NOT NULL,
            payload     TEXT NOT NULL DEFAULT '{}',
            state       TEXT NOT NULL DEFAULT 'pending',
            gate_l1     INTEGER NOT NULL DEFAULT 0,
            gate_l2     INTEGER NOT NULL DEFAULT 0,
            created_ts  INTEGER NOT NULL DEFAULT (strftime('%s','now')),
            updated_ts  INTEGER NOT NULL DEFAULT (strftime('%s','now')),
            result      TEXT NOT NULL DEFAULT ''
        )",
        [],
    );
    let payload = serde_json::json!({
        "civ": civ, "spec": spec, "real": real, "source": "ne-mcp",
    })
    .to_string();
    let _ = con.execute(
        "INSERT OR REPLACE INTO webui_task(id,kind,payload,state,gate_l1,gate_l2) \
         VALUES(?1,'cardgen',?2,?3,0,0)",
        rusqlite::params![task_id, payload, state],
    );
}

// ─────────────────────────────────────────── 工具 17：ne_cardgen_plan（只读）
/// 建卡预演：分配编号 + 查重 + 出计划。**不调 LLM、不落盘、不需要解锁。**
///
/// 底层直拉 `python3 cardgen.py plan --json`。它会插占位行再 rollback（这是
/// `next_code` 分配机制决定的），但 plan 结束时一定 rollback，DB 无净变化。
pub fn cardgen_plan(cfg: &Cfg, civ: &str, spec: &str) -> Result<serde_json::Value> {
    use std::process::Command;
    if civ.trim().is_empty() {
        anyhow::bail!("civ 不能为空（例：混沌帝国）");
    }
    if spec.trim().is_empty() {
        anyhow::bail!("spec 不能为空（例：\"文明:1,势力:3,地点:3,角色:3\"）");
    }
    let py = cfg.root.join("05_管线框架").join("cardgen.py");
    if !py.exists() {
        anyhow::bail!("cardgen.py 不存在：{}（请确认资料库根）", py.display());
    }
    let out = Command::new("python3")
        .arg(&py)
        .arg("plan")
        .arg("--civ")
        .arg(civ)
        .arg("--spec")
        .arg(spec)
        .arg("--json")
        .current_dir(cfg.root.join("05_管线框架"))
        .output()
        .map_err(|e| anyhow::anyhow!("拉起 cardgen.py 失败：{e}（python3 是否在 PATH？）"))?;

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let code = out.status.code().unwrap_or(-1);

    // `--json` 的 JSON 是最后一行以 `{` 开头的（前面可能有 🧹 自愈清理 提示）
    let last_json = stdout
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .and_then(|l| serde_json::from_str::<serde_json::Value>(l).ok());

    let clip = |s: &str, n: usize| -> String {
        if s.chars().count() <= n {
            s.to_string()
        } else {
            let head: String = s.chars().take(n).collect();
            format!("{head}\n…（已截断，共 {} 字符）", s.chars().count())
        }
    };

    Ok(match last_json {
        Some(plan) => serde_json::json!({
            "ok": code == 0,
            "dispatched": false,
            "real": false,
            "wrote": false,
            "exit_code": code,
            "plan": plan,
            "stderr_tail": if stderr.trim().is_empty() { String::new() } else { clip(&stderr, 1500) },
            "note": "只读预演：未调 LLM、未落盘、无需解锁。确认无误后用 ne_cardgen_run 真跑。",
        }),
        None => serde_json::json!({
            "ok": false,
            "dispatched": false,
            "real": false,
            "exit_code": code,
            "error": "cardgen.py 未返回可解析的 JSON（见 stdout/stderr）",
            "stdout_tail": clip(&stdout, 3000),
            "stderr_tail": clip(&stderr, 3000),
        }),
    })
}

// ─────────────────────────────────────────── 工具 18：ne_cardgen_run（写类）
/// 批量建卡：真实调 LLM 生成卡片正文并三同步落盘。
///
/// # 双锁（与 L5 同一把锁，口径一致）
/// - `real=false`（默认）：**不检查任何锁**，走 `--dry-run`，只出计划与 prompt 预览，
///   0 调 LLM、0 落盘。这让「零风险空跑」成为默认行为。
/// - `real=true`：必须存在 `/sdcard/wogov_real_llm.allow`，否则直接拒绝（不静默降级）。
///   Python 侧 `call_llm()` 会**再校验一次**——因为执行器可能绕过 MCP 直接拉 cardgen.py，
///   两层都拦才叫"不可绕过"。
///
/// # 全局单飞锁
/// 同一时刻只允许 1 个建卡进程（见 `CARDGEN_LOCK`）。第二个请求直接被拒。
///
/// # 异步
/// 默认 `spawn()` 后立即返回 `task_id`。子进程 stdout/stderr **必须重定向到日志文件**
/// —— 否则会写进 MCP 的 stdout，污染 newline-delimited JSON-RPC 协议流，直接搞崩会话。
#[allow(clippy::too_many_arguments)]
pub fn cardgen_run(
    cfg: &Cfg,
    civ: &str,
    spec: &str,
    real: bool,
    wait: bool,
    timeout_sec: u32,
) -> Result<serde_json::Value> {
    use std::process::{Command, Stdio};
    if civ.trim().is_empty() {
        anyhow::bail!("civ 不能为空（例：混沌帝国）");
    }
    if spec.trim().is_empty() {
        anyhow::bail!("spec 不能为空（例：\"文明:1,势力:3,地点:3,角色:3\"）");
    }
    let py = cfg.root.join("05_管线框架").join("cardgen.py");
    if !py.exists() {
        anyhow::bail!("cardgen.py 不存在：{}（请确认资料库根）", py.display());
    }
    let t = timeout_sec.clamp(10, 3600);

    // ── 锁：real=true 才查文件锁（与 run_chapter 同口径）──
    let allow_file = Path::new(ALLOW_FILE);
    let env_allow = ne_agent::governance::llm_gate::env_allowed();
    let file_allow = allow_file.exists();
    if real && !file_allow {
        return Ok(serde_json::json!({
            "ok": false,
            "dispatched": false,
            "real": true,
            "blocked_reason": "Real LLM is locked. Touch /sdcard/wogov_real_llm.allow to unlock.",
            "hint": "先用 real=false 空跑验证链路；确认无误后手动 `touch /sdcard/wogov_real_llm.allow` 解锁，`rm` 即上锁。",
            "lock": {
                "file": ALLOW_FILE,
                "file_exists": false,
                "env_WOGOV_ALLOW_REAL_LLM": env_allow,
            },
        }));
    }

    // ── 全局单飞锁 ──
    if let Err(msg) = acquire_cardgen_lock() {
        return Ok(serde_json::json!({
            "ok": false,
            "dispatched": false,
            "real": real,
            "blocked_reason": msg,
            "lock_file": CARDGEN_LOCK,
            "lock_pid": read_lock_pid(Path::new(CARDGEN_LOCK)),
        }));
    }

    // ── 登记任务 ──
    let task_id = new_task_id();
    register_cardgen_task(cfg, &task_id, civ, spec, real, "running");

    // ── 日志目录（子进程 stdout/stderr 必须重定向，绝不能进 MCP stdout）──
    let log_dir = cfg.root.join("07_勘误工坊").join("cardgen_logs");
    let _ = fs::create_dir_all(&log_dir);
    let out_path = log_dir.join(format!("{task_id}.out"));
    let err_path = log_dir.join(format!("{task_id}.err"));
    let out_f = std::fs::File::create(&out_path).ok();
    let err_f = std::fs::File::create(&err_path).ok();
    let (out_std, err_std) = match (out_f, err_f) {
        (Some(o), Some(e)) => (Stdio::from(o), Stdio::from(e)),
        _ => (Stdio::null(), Stdio::null()),
    };

    // ── 组装命令 ──
    let mut cmd = Command::new("python3");
    cmd.arg(&py)
        .arg("run")
        .arg("--civ")
        .arg(civ)
        .arg("--spec")
        .arg(spec)
        .arg("--task-id")
        .arg(&task_id)
        .arg("--json")
        .current_dir(cfg.root.join("05_管线框架"))
        .stdout(out_std)
        .stderr(err_std);
    if !real {
        cmd.arg("--dry-run");
    }

    if wait {
        // 同步模式：拦住等结果（仅建议单卡/小批量 + 小 timeout 使用）
        let started = std::time::Instant::now();
        let out = match cmd.output() {
            Ok(o) => o,
            Err(e) => {
                release_cardgen_lock();
                anyhow::bail!("拉起 cardgen.py 失败：{e}（python3 是否在 PATH？）");
            }
        };
        let elapsed = started.elapsed();
        let code = out.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        release_cardgen_lock();
        let clip = |s: &str, n: usize| -> String {
            if s.chars().count() <= n {
                s.to_string()
            } else {
                let head: String = s.chars().take(n).collect();
                format!("{head}\n…（已截断，共 {} 字符）", s.chars().count())
            }
        };
        return Ok(serde_json::json!({
            "ok": code == 0,
            "dispatched": true,
            "async": false,
            "real": real,
            "task_id": task_id,
            "dirty_run": !real,
            "exit_code": code,
            "elapsed_sec": elapsed.as_secs_f64().round(),
            "timeout_sec": t,
            "timed_out": elapsed > std::time::Duration::from_secs(t as u64),
            "lock": { "file": ALLOW_FILE, "file_exists": file_allow,
                      "env_WOGOV_ALLOW_REAL_LLM": env_allow },
            "stdout_tail": clip(&stdout, 6000),
            "note": if real { "已真实建卡（LLM 生成 + 三同步落盘）" } else { "空跑：--dry-run，0 调 LLM 0 落盘" },
        }));
    }

    // ── 异步模式（默认）：spawn 后立刻返回 ──
    match cmd.spawn() {
        Ok(child) => {
            let pid = child.id();
            stamp_lock_pid(pid);
            Ok(serde_json::json!({
                "ok": true,
                "dispatched": true,
                "async": true,
                "real": real,
                "task_id": task_id,
                "pid": pid,
                "dry_run": !real,
                "lock": {
                    "file": ALLOW_FILE,
                    "file_exists": file_allow,
                    "env_WOGOV_ALLOW_REAL_LLM": env_allow,
                    "singleton": CARDGEN_LOCK,
                    "pid": pid,
                },
                "logs": {
                    "stdout": out_path.display().to_string(),
                    "stderr": err_path.display().to_string(),
                },
                "poll": format!(
                    "ne_db_query SELECT state,result FROM webui_task WHERE id='{task_id}'  \
                     或 GET http://127.0.0.1:8270/api/tasks/{task_id}"
                ),
                "note": if real {
                    "子进程已 detach（不阻塞 MCP 主循环），正在真实建卡。进度写 webui_task.result；跑完记得 `rm /sdcard/wogov_real_llm.allow` 上锁。"
                } else {
                    "子进程已 detach，正在空跑（--dry-run，0 调 LLM 0 落盘）。进度写 webui_task.result。"
                },
            }))
        }
        Err(e) => {
            release_cardgen_lock();
            register_cardgen_task(cfg, &task_id, civ, spec, real, "failed");
            anyhow::bail!("spawn cardgen.py 失败：{e}（python3 是否在 PATH？）");
        }
    }
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
