//! report_builder —— 审计报告 & 导出管线（world-gov）。
//!
//! # 定位（严格职责）
//! 只做**拼接导出**：把已经落库的 `risk_index` + `task_jobs` + 基线指纹 → 结构化审计报告。
//! **不是**自然语言总结，**不调用 LLM**，**不依赖 prompt_builder / semantic_runtime 做计算**。
//!
//! # 红线
//! - ❌ 不引入 http/llm；❌ 不新增数据库表；❌ 不后台任务；❌ 不自动写资料库正文。
//! - ✅ 产物严格落在 `LIB/07_勘误工坊/reports/`，自动建目录；文件名带时间戳 + run_id。
//! - ✅ 不自动导出：必须显式命令（`audit-export new`）触发。
//! - ✅ 只读 risk_index / task_jobs / 基线快照；不修改任何既有数据。
//!
//! # 与既有体系的关系
//! - `semantic-verify` / `queue step` 产生风险 → 落 `risk_index`（is_semantic=1）；
//! - 本层只把它们**读出来拼接**成可导出的审计产物，两种格式：json（机器可读）、minimal-md（极简可读）。

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use super::pipeline::SemanticJob;
use super::pipeline_store;

/// 报告输出子目录（相对库根）。
pub const REPORTS_SUBDIR: &str = "07_勘误工坊/reports";

/// 报告格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportFormat {
    Json,
    MinimalMd,
}

impl ReportFormat {
    pub fn ext(&self) -> &'static str {
        match self {
            ReportFormat::Json => "json",
            ReportFormat::MinimalMd => "md",
        }
    }
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "json" => Ok(ReportFormat::Json),
            "minimal-md" | "minimal_md" | "md" => Ok(ReportFormat::MinimalMd),
            other => anyhow::bail!("未知格式 '{other}'（仅支持 json | minimal-md）"),
        }
    }
}

// ────────────────────────────────── 结构契约

/// 报告元数据（字段非空由单测锁定）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportMeta {
    pub run_id: String,
    pub baseline_fingerprint: String,
    pub at: i64,
    pub layer_filter: String,
    pub all_layers: bool,
}

/// 报告条目（一条风险/发现）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportItem {
    pub risk_id: String,
    /// 关联任务 id（来自 risk_index.source 或过滤参数；可为空）
    pub job_id: Option<String>,
    /// 条目定位：volume/chapter 或 key
    pub item_loc: String,
    /// 法理依据原话（无依据留空）
    pub law_ref: String,
    pub verdict: String,
    pub confidence: f32,
    /// 溯源说明（law 哈希、来源命令、时间等）
    pub provenance_notes: String,
}

/// 一份完整审计报告。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditReport {
    pub meta: ReportMeta,
    /// 过滤条件回显（便于复现）
    pub filters: Value,
    /// 统计概览（非自然语言，纯计数）
    pub counts: Value,
    pub items: Vec<ReportItem>,
}

// ────────────────────────────────── 过滤条件

/// 导出过滤条件。
#[derive(Debug, Clone, Default)]
pub struct ExportFilter {
    pub only_semantic: bool,
    pub only_structural: bool,
    /// 只看语义依据文本哈希（从 message 里的 `law#<hash>` 比对）
    pub law_hash: Option<String>,
    /// 只看某任务产生的风险
    pub job_id: Option<String>,
}

impl ExportFilter {
    fn describe(&self) -> Value {
        json!({
            "only_semantic": self.only_semantic,
            "only_structural": self.only_structural,
            "law_hash": self.law_hash,
            "job_id": self.job_id,
        })
    }
}

// ────────────────────────────────── 工具

fn now_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn fnv1a_hex(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// 生成 run_id：时间戳 + 内容指纹（不含随机源，可复现）。
pub fn make_run_id(at: i64, seed_extra: &str) -> String {
    let h = fnv1a_hex(seed_extra);
    format!("r{at}-{}", &h[..8])
}

/// 文件名安全转义：仅保留 ascii 字母数字，其余转 `_`；并拒绝任何路径分隔/`..`。
/// （防 `../` 逃逸：转义后不可能出现 `/` 或 `\`，且不以 `.` 开头。）
pub fn safe_token(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let cleaned = cleaned.trim_matches('_').to_string();
    if cleaned.is_empty() {
        "run".to_string()
    } else {
        cleaned
    }
}

/// 报告文件名（**纯函数**，便于单测）：`audit_<at>_<run_id>.<ext>`；全程经 `safe_token`。
pub fn report_file_name(at: i64, run_id: &str, fmt: ReportFormat) -> String {
    format!("audit_{}_{}.{}", at, safe_token(run_id), fmt.ext())
}

/// 从 message 里抽出 `law#<hash>`（emit_semantic_findings 写入的幂等键片段）。
pub fn extract_law_hash(message: &str) -> Option<String> {
    let idx = message.find("law#")?;
    let rest = &message[idx + 4..];
    let h: String = rest.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
    if h.is_empty() {
        None
    } else {
        Some(h)
    }
}

// ────────────────────────────────── 读取落库数据（只读）

/// 从 risk_index 读一行（含语义列）。
struct RiskRow {
    id: i64,
    risk_type: String,
    volume: Option<String>,
    chapter: Option<i64>,
    source: String,
    message: String,
    level: String,
    is_semantic: i64,
    law_basis: Option<String>,
}

fn read_risks(
    conn: &Connection,
    only_semantic: bool,
    only_structural: bool,
) -> Result<Vec<RiskRow>> {
    // 语义列可能尚未创建（新库）：先幂等补齐（只 ALTER 加列，不动数据）
    super::ensure_risk_index_cols(conn)?;
    let sql = "SELECT id,risk_type,volume,chapter,source,message,level,is_semantic,law_basis
               FROM risk_index WHERE resolved=0
               ORDER BY id";
    let mut stmt = conn.prepare(sql)?;
    let rows: Vec<RiskRow> = stmt
        .query_map([], |r| {
            Ok(RiskRow {
                id: r.get(0)?,
                risk_type: r.get(1)?,
                volume: r.get(2)?,
                chapter: r.get(3)?,
                source: r.get(4)?,
                message: r.get(5)?,
                level: r.get(6)?,
                is_semantic: r.get(7)?,
                law_basis: r.get(8)?,
            })
        })?
        .filter_map(|x| x.ok())
        .filter(|r| {
            if only_semantic {
                r.is_semantic == 1
            } else if only_structural {
                r.is_semantic == 0
            } else {
                true
            }
        })
        .collect();
    Ok(rows)
}

/// 把一行风险映射为 ReportItem（`law_hash` 过滤在此生效）。
fn risk_to_item(r: &RiskRow, filter: &ExportFilter) -> Option<ReportItem> {
    let law_hash = extract_law_hash(&r.message);
    if let Some(want) = filter.law_hash.as_deref() {
        if law_hash.as_deref() != Some(want) {
            return None;
        }
    }
    if let Some(job) = filter.job_id.as_deref() {
        // job 过滤：source 里包含该 job id 才算
        if !r.source.contains(job) {
            return None;
        }
    }
    let item_loc = match (&r.volume, r.chapter) {
        (Some(v), Some(c)) => format!("{v}/第{c}章"),
        (Some(v), None) => v.clone(),
        (None, _) => "(全库)".to_string(),
    };
    // confidence：语义项给出占位 0.0（真实置信度由推理层提供，此处不臆造）；
    // 无依据的项 confidence 恒 0，避免"看似有把握"。
    let provenance_notes = format!(
        "risk_type={} source={} risk_id={} law_hash={}",
        r.risk_type,
        r.source,
        r.id,
        law_hash.unwrap_or_else(|| "-".to_string())
    );
    Some(ReportItem {
        risk_id: format!("risk#{}", r.id),
        job_id: if filter.job_id.is_some() { filter.job_id.clone() } else { None },
        item_loc,
        law_ref: r.law_basis.clone().unwrap_or_default(),
        verdict: r.level.clone(),
        confidence: 0.0,
        provenance_notes,
    })
}

/// 读任务表（可选；表不存在时返回空、不报错、不建表）。
fn read_jobs(conn: &Connection, job_id: Option<&str>) -> Vec<SemanticJob> {
    if pipeline_store::ensure_task_table(conn).is_err() {
        return Vec::new();
    }
    match pipeline_store::job_list(conn, None) {
        Ok(v) => v
            .into_iter()
            .filter(|j| job_id.map(|want| j.id == want).unwrap_or(true))
            .collect(),
        Err(_) => Vec::new(),
    }
}

// ────────────────────────────────── 组装报告

/// 组装一份审计报告（**纯读**，不落盘；落盘由 `write_report` 负责）。
pub fn build_report(
    conn: &Connection,
    lib_root: &Path,
    layer_filter: &str,
    all_layers: bool,
    filter: &ExportFilter,
) -> Result<AuditReport> {
    let at = now_ts();
    let baseline = crate::world_gov::read_baseline_fingerprint(lib_root)
        .unwrap_or_else(|| "(未找到基线快照)".to_string());
    // 语义/结构计数（先算，供 run_id 与概览）
    super::ensure_risk_index_cols(conn)?;
    let semantic_n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM risk_index WHERE is_semantic=1 AND resolved=0",
            [],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    let structural_n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM risk_index WHERE is_semantic=0 AND resolved=0",
            [],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    let rows = read_risks(conn, filter.only_semantic, filter.only_structural)?;
    let items: Vec<ReportItem> = rows.iter().filter_map(|r| risk_to_item(r, filter)).collect();
    let jobs = read_jobs(conn, filter.job_id.as_deref());
    let seed = format!(
        "{baseline}|{layer_filter}|{all_layers}|sem={semantic_n}|struct={structural_n}|items={}",
        items.len()
    );
    let run_id = make_run_id(at, &seed);
    let meta = ReportMeta {
        run_id: run_id.clone(),
        baseline_fingerprint: baseline,
        at,
        layer_filter: layer_filter.to_string(),
        all_layers,
    };
    let counts = json!({
        "risks_semantic_total": semantic_n,
        "risks_structural_total": structural_n,
        "items_exported": items.len(),
        "jobs_seen": jobs.len(),
        "jobs_by_state": count_jobs_by_state(&jobs),
    });
    Ok(AuditReport { meta, filters: filter.describe(), counts, items })
}

fn count_jobs_by_state(jobs: &[SemanticJob]) -> Value {
    let mut m = serde_json::Map::new();
    for j in jobs {
        let k = j.state.as_str();
        let cur = m.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
        m.insert(k.to_string(), json!(cur + 1));
    }
    Value::Object(m)
}

/// 渲染 json（机器可读）。
pub fn render_json(rep: &AuditReport) -> Result<String> {
    Ok(serde_json::to_string_pretty(&json!({
        "schema": "world-gov.audit-report/v1",
        "meta": rep.meta,
        "filters": rep.filters,
        "counts": rep.counts,
        "items": rep.items,
        "note": "结构化审计产物（非自然语言总结）；不自动修改任何资料库内容。",
    }))?)
}

/// 渲染 minimal-md（极简可读，不渲染花哨样式）。
pub fn render_minimal_md(rep: &AuditReport) -> String {
    let mut s = String::new();
    s.push_str("# world-gov 审计报告（minimal）\n\n");
    s.push_str(&format!("- run_id: {}\n", rep.meta.run_id));
    s.push_str(&format!("- baseline_fingerprint: {}\n", rep.meta.baseline_fingerprint));
    s.push_str(&format!("- at: {} (unix)\n", rep.meta.at));
    s.push_str(&format!(
        "- layer_filter: {}   all_layers: {}\n",
        rep.meta.layer_filter, rep.meta.all_layers
    ));
    s.push_str(&format!("- filters: {}\n", rep.filters));
    s.push_str(&format!("- counts: {}\n\n", rep.counts));
    s.push_str(&format!("## items（{} 条）\n\n", rep.items.len()));
    for it in &rep.items {
        s.push_str(&format!(
            "- [{}] {} @{} verdict={} law_ref={} conf={:.2}\n",
            it.risk_id, it.item_loc, it.job_id.as_deref().unwrap_or("-"),
            it.verdict, if it.law_ref.is_empty() { "-" } else { &it.law_ref }, it.confidence
        ));
        s.push_str(&format!("  provenance: {}\n", it.provenance_notes));
    }
    s.push_str("\n> 本报告只出证据与计数，供人判读；不自动修改任何资料库内容。\n");
    s
}

/// 按格式渲染。
pub fn render(rep: &AuditReport, fmt: ReportFormat) -> Result<String> {
    match fmt {
        ReportFormat::Json => render_json(rep),
        ReportFormat::MinimalMd => Ok(render_minimal_md(rep)),
    }
}

// ────────────────────────────────── 落盘 / 列举 / 读取

/// 报告输出目录（自动建）。
pub fn reports_dir(lib_root: &Path) -> PathBuf {
    lib_root.join("07_勘误工坊").join("reports")
}

/// reports/ 目录的可读路径（供 CLI 展示）。
pub fn lf_dir_display(lib_root: &Path) -> String {
    reports_dir(lib_root).display().to_string()
}

/// 导出时的默认层过滤文案（与 scan 同口径：默认 L2/L3/L4；all=true 时关闭）。
pub fn scan_layer_filter_text(all_layers: bool) -> String {
    if all_layers {
        "off（--all-layers）".to_string()
    } else {
        crate::world_gov::SCAN_LAYERS_DEFAULT.join("/")
    }
}

/// 写盘：产物**只落** `reports_dir`。返回写入路径。
pub fn write_report(lib_root: &Path, rep: &AuditReport, fmt: ReportFormat) -> Result<PathBuf> {
    let dir = reports_dir(lib_root);
    std::fs::create_dir_all(&dir)?;
    let name = report_file_name(rep.meta.at, &rep.meta.run_id, fmt);
    // 双保险：文件名不得含路径分隔（safe_token 已保证），再断言一次
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        anyhow::bail!("文件名安全校验失败：{name}");
    }
    let path = dir.join(name);
    let body = render(rep, fmt)?;
    std::fs::write(&path, body)?;
    Ok(path)
}

/// 列举已有报告（只列文件名 + 时间，不读内容）。
pub fn list_reports(lib_root: &Path) -> Result<Vec<Value>> {
    let dir = reports_dir(lib_root);
    let mut out = Vec::new();
    if !dir.is_dir() {
        return Ok(out);
    }
    for e in std::fs::read_dir(&dir)? {
        let e = e?;
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
        if !name.starts_with("audit_") {
            continue;
        }
        let mtime = std::fs::metadata(&p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        out.push(json!({"name": name, "mtime": mtime, "size": size}));
    }
    out.sort_by(|a, b| b["mtime"].as_i64().cmp(&a["mtime"].as_i64()));
    Ok(out)
}

/// 用 run_id 打开已有报告（内容）。返回 (path, content)。
/// 只接受安全扩展名（json/md），只用 `safe_token(run_id)` 比对文件名，**不接受任何路径分隔**。
pub fn read_report(lib_root: &Path, ext: &str, run_id: &str) -> Result<(PathBuf, String)> {
    // 扩展名白名单：防 `ext` 携带路径分隔/.. 越界
    let ext_ok = matches!(ext, "json" | "md");
    if !ext_ok {
        anyhow::bail!("非法扩展名 '{ext}'（仅允许 json | md）");
    }
    let dir = reports_dir(lib_root);
    let token = safe_token(run_id);
    let mut hit: Option<PathBuf> = None;
    if dir.is_dir() {
        for e in std::fs::read_dir(&dir)? {
            let e = e?;
            let name = e.file_name().to_string_lossy().to_string();
            // 文件名形如 audit_<at>_<token>.<ext>，且扩展名必须匹配
            if name.starts_with("audit_")
                && name.contains(&format!("_{token}."))
                && name.ends_with(&format!(".{ext}"))
            {
                hit = Some(e.path());
                break;
            }
        }
    }
    match hit {
        Some(p) => {
            let content = std::fs::read_to_string(&p)?;
            Ok((p, content))
        }
        None => anyhow::bail!("未找到 run_id='{run_id}' 的报告（用 audit-export list 查看可用 run_id）"),
    }
}

// ────────────────────────────────── 单测

#[cfg(test)]
mod tests {
    use super::*;
    use crate::governance::record_risk_semantic;

    fn mem_conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(include_str!("../governance_schema.sql")).unwrap();
        crate::governance::ensure_risk_index_cols(&c).unwrap();
        c
    }

    #[test]
    fn report_meta_fields_are_complete_and_nonempty() {
        let conn = mem_conn();
        let lib = std::env::temp_dir().join("wg_report_meta_lib");
        let rep = build_report(&conn, &lib, "L2/L3/L4", false, &ExportFilter::default()).unwrap();
        assert!(!rep.meta.run_id.is_empty(), "run_id 非空");
        assert!(!rep.meta.baseline_fingerprint.is_empty(), "基线指纹非空（缺失时给占位）");
        assert!(rep.meta.at > 0, "时间戳非空");
        assert_eq!(rep.meta.layer_filter, "L2/L3/L4");
        assert!(!rep.meta.all_layers);
        assert!(rep.counts["items_exported"].as_i64().is_some());
    }

    #[test]
    fn only_semantic_and_structural_filters() {
        let conn = mem_conn();
        // 语义一条（is_semantic=1）
        record_risk_semantic(&conn, "world_gov", None, None, "world-gov semantic-verify", "HIGH-RISK",
            "语义X :: law#deadbeef :: e", "上层原话").unwrap();
        // 结构一条（is_semantic=0）
        crate::governance::record_risk(&conn, "library", None, None, "library-ctl", "WARN", "结构脏点").unwrap();

        let lib = std::env::temp_dir().join("wg_report_filter_lib");
        let f_sem = ExportFilter { only_semantic: true, ..Default::default() };
        let r_sem = build_report(&conn, &lib, "L2/L3/L4", false, &f_sem).unwrap();
        assert_eq!(r_sem.items.len(), 1, "只语义 → 1 条");
        assert!(r_sem.items[0].provenance_notes.contains("law_hash=deadbeef"));

        let f_str = ExportFilter { only_structural: true, ..Default::default() };
        let r_str = build_report(&conn, &lib, "L2/L3/L4", false, &f_str).unwrap();
        assert_eq!(r_str.items.len(), 1, "只结构 → 1 条");
        assert!(r_str.items[0].law_ref.is_empty(), "结构项无 law_ref");
    }

    #[test]
    fn law_hash_filter_selects_exact_basis() {
        let conn = mem_conn();
        record_risk_semantic(&conn, "world_gov", None, None, "s1", "WARN", "语义A :: law#aaaa1111 :: e", "依据A").unwrap();
        record_risk_semantic(&conn, "world_gov", None, None, "s2", "WARN", "语义B :: law#bbbb2222 :: e", "依据B").unwrap();
        let lib = std::env::temp_dir().join("wg_report_lawhash_lib");
        let f = ExportFilter { law_hash: Some("aaaa1111".into()), ..Default::default() };
        let r = build_report(&conn, &lib, "L2/L3/L4", false, &f).unwrap();
        assert_eq!(r.items.len(), 1, "按 law_hash 精确命中 1 条");
        assert_eq!(r.items[0].law_ref, "依据A");
    }

    #[test]
    fn extract_law_hash_parses_and_rejects_absent() {
        assert_eq!(extract_law_hash("x :: law#deadbeef :: y"), Some("deadbeef".into()));
        assert_eq!(extract_law_hash("no hash here"), None);
        assert_eq!(extract_law_hash("law#"), None);
    }

    #[test]
    fn file_name_is_path_safe() {
        // 恶意 run_id 必须被安全转义，绝无 ../ 逃逸
        let name = report_file_name(1700000000, "../../etc/passwd", ReportFormat::Json);
        assert!(!name.contains(".."), "不得含 ..");
        assert!(!name.contains('/'), "不得含 /");
        assert!(!name.contains('\\'), "不得含 \\");
        assert!(name.starts_with("audit_1700000000_"));
        assert!(name.ends_with(".json"));
        // 全非法字符 → 回退 'run'
        assert_eq!(safe_token("../../"), "run");
    }

    #[test]
    fn write_creates_dir_and_persists_then_lists() {
        let conn = mem_conn();
        record_risk_semantic(&conn, "world_gov", None, None, "s", "HIGH-RISK", "语义 :: law#cafe1234 :: e", "L0原话").unwrap();
        let lib = std::env::temp_dir().join(format!("wg_report_write_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&lib);
        let rep = build_report(&conn, &lib, "L2/L3/L4", false, &ExportFilter::default()).unwrap();
        let p = write_report(&lib, &rep, ReportFormat::MinimalMd).unwrap();
        assert!(p.is_file(), "报告已落盘");
        assert!(p.starts_with(reports_dir(&lib)), "必须落在 reports/ 目录内");
        let listed = list_reports(&lib).unwrap();
        assert_eq!(listed.len(), 1, "列出 1 份");
        assert!(listed[0]["name"].as_str().unwrap().ends_with(".md"));
        // 读回
        let (_p2, content) = read_report(&lib, "md", &rep.meta.run_id).unwrap();
        assert!(content.contains("world-gov 审计报告"));
        assert!(content.contains("L0原话"));
        let _ = std::fs::remove_dir_all(&lib);
    }

    #[test]
    fn dry_run_writes_nothing() {
        // dry-run 语义：只 build_report（纯读），不调用 write_report → 磁盘无变化
        let conn = mem_conn();
        let lib = std::env::temp_dir().join(format!("wg_report_dry_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&lib);
        let _rep = build_report(&conn, &lib, "L2/L3/L4", false, &ExportFilter::default()).unwrap();
        assert!(!reports_dir(&lib).exists(), "dry-run 后不得创建 reports 目录");
    }

    #[test]
    fn render_minimal_md_is_plain_and_complete() {
        let conn = mem_conn();
        record_risk_semantic(&conn, "world_gov", Some("卷01"), Some(3), "s", "WARN", "语义 :: law#11223344 :: e", "依据原话").unwrap();
        let lib = std::env::temp_dir().join("wg_report_md_lib");
        let rep = build_report(&conn, &lib, "L2/L3/L4", false, &ExportFilter::default()).unwrap();
        let md = render_minimal_md(&rep);
        assert!(md.starts_with("# world-gov 审计报告"));
        assert!(md.contains("卷01/第3章"), "条目定位应含卷/章");
        assert!(md.contains("baseline_fingerprint"));
        assert!(md.contains("依据原话"));
        // 不应含 Markdown 花哨样式（表格/图片/HTML）
        assert!(!md.contains("<img"), "不渲染花哨样式");
        assert!(!md.contains("!["), "不渲染图片");
    }

    #[test]
    fn job_filter_limits_by_source() {
        let conn = mem_conn();
        record_risk_semantic(&conn, "world_gov", None, None, "job-abc123 scan", "WARN", "A :: law#1 :: e", "").unwrap();
        record_risk_semantic(&conn, "world_gov", None, None, "job-zzz999 scan", "WARN", "B :: law#2 :: e", "").unwrap();
        let lib = std::env::temp_dir().join("wg_report_job_lib");
        let f = ExportFilter { job_id: Some("job-abc123".into()), ..Default::default() };
        let r = build_report(&conn, &lib, "L2/L3/L4", false, &f).unwrap();
        assert_eq!(r.items.len(), 1, "只导出该 job 的风险");
        assert_eq!(r.items[0].job_id.as_deref(), Some("job-abc123"));
    }

    #[test]
    fn read_report_missing_run_id_errors_cleanly() {
        let lib = std::env::temp_dir().join("wg_report_missing_lib");
        let e = read_report(&lib, "md", "nope").unwrap_err();
        assert!(e.to_string().contains("未找到"));
    }

    #[test]
    fn read_report_rejects_unsafe_ext() {
        let lib = std::env::temp_dir().join("wg_report_ext_lib");
        let e = read_report(&lib, "../secret", "x").unwrap_err();
        assert!(e.to_string().contains("非法扩展名"));
        let e2 = read_report(&lib, "json/../x", "x").unwrap_err();
        assert!(e2.to_string().contains("非法扩展名"));
    }

    #[test]
    fn report_format_parse() {
        assert_eq!(ReportFormat::parse("json").unwrap(), ReportFormat::Json);
        assert_eq!(ReportFormat::parse("minimal-md").unwrap(), ReportFormat::MinimalMd);
        assert!(ReportFormat::parse("pdf").is_err());
    }
}