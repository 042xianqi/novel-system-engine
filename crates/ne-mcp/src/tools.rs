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
    let mut by_cat: std::collections::BTreeMap<String, Vec<serde_json::Value>> =
        std::collections::BTreeMap::new();
    for j in &jobs {
        let cat = if j.category.is_empty() { "未分类".to_string() } else { j.category.clone() };
        by_cat.entry(cat).or_default().push(serde_json::json!({
            "name": j.name, "source_ref": j.source_ref, "material": j.material,
        }));
    }

    Ok(serde_json::json!({
        "outline": outline.display().to_string(),
        "volume": volume,
        "job_count": jobs.len(),
        "categories": by_cat.keys().cloned().collect::<Vec<_>>(),
        "by_category": by_cat,
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
/// 行格式：`- 名字（内文）`，只取内文含"无…卡/待建"的（与 ne-cardgen 同口径）。
pub fn extract_card_jobs(outline_text: &str) -> Vec<CardJob> {
    let re = regex::Regex::new(r"(?m)^>?\s*-\s+([^（\n]+)（(.*)）\s*$").unwrap();
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
    jobs
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

    let packet: InjectionPacket = ne_injection::build_injection_ranked(
        &ch,
        &cfg.lib_root(),
        &cfg.content_dir(),
        volume,
        prev,
        &usage,
        &banned,
    );

    // 缺失引用：如实列出（不杜撰）。allow_missing=false 时视为硬失败。
    let missing = packet.missing_refs.clone();
    if !missing.is_empty() && !allow_missing {
        return Ok(serde_json::json!({
            "ok": false,
            "reason": "存在库中缺失的引用（如需继续请传 allow_missing=true）",
            "chapter": chapter, "volume": volume,
            "missing_refs": missing,
        }));
    }

    Ok(serde_json::json!({
        "ok": true,
        "volume": volume, "chapter": chapter, "title": ch.title,
        "scene_type": format!("{:?}", ch.scene_type),
        "characters": ch.characters,
        "card_refs": ch.card_refs,
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
/// 对正文跑确定性质检（AI 味词 / 禁词 / 字数）→ verdict + findings + 路由。
pub fn check(text: &str, chapter: u32) -> Result<serde_json::Value> {
    let banned: Vec<String> = ne_qc::BANNED_WORDS.iter().map(|s| s.to_string()).collect();
    let r = ne_qc::validate_six(text, None, &banned);
    let route = ne_qc::route_on_verdict(&r);
    Ok(serde_json::json!({
        "verdict": format!("{:?}", r.verdict),
        "route": route,
        "stats": r.stats,
        "word_range": [ne_qc::MIN_WORDS, ne_qc::MAX_WORDS],
        "findings": r.findings.iter().map(|f| serde_json::json!({
            "dim": f.dim, "issue": f.issue, "severity": format!("{:?}", f.severity),
        })).collect::<Vec<_>>(),
        "gate14": ne_qc::gate14(text, chapter, ""),
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
