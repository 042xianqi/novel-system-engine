//! 治理层（gov）—— 独立外挂巡检工具集，**绝不改写生成/注入/记忆链路**。
//!
//! # 铁则
//! - 只读巡检：不修改/删除任何总纲、细纲、资料卡、正文。
//! - 新增表（risk_index / library_index）只通过 `governance_schema.sql` 幂等创建。
//! - 所有 `--semantic` 默认关闭；开启才调模型，且只产报告不落正文改动。
//! - 新增开关（如 `--check-compliance`）默认关闭。
//!
//! 子模块：`pipeline` —— 语义管线（探针→入队→(将来喂LLM)→结果→入库）内存占位。
//! 子模块：`pipeline_store` —— 可选持久任务层（novel.db.task_jobs；仅 --persist 启用）。
//! 子模块：`semantic_runtime` —— 语义推理运行时（契约/壳/沙盘替身；真实调用死 TODO）。
//! 子模块：`prompt_builder` —— 探针 JSON → 结构化 prompt 文本（规则层；无外呼、无副作用）。
//! 子模块：`report_builder` —— 审计报告 & 导出（只读 risk_index/task_jobs/基线 → 结构化产物）。
//! 子模块：`progress` —— 标准化进度输出（stderr；--quiet 静默）。
//! 子模块：`scan_snapshot` —— 增量扫描快照（07_勘误工坊/.scan_snapshot.json）。
//! 子模块：`inspect_batch` —— 批量检视入口（默认只预览/入队；绝不自动推理）。
pub mod pipeline;
pub mod pipeline_store;
pub mod progress;
pub mod prompt_builder;
pub mod report_builder;
pub mod scan_snapshot;
pub mod inspect_batch;
pub mod semantic_runtime;

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

// ────────────────────────────────── 术语

/// 层级法理优先级（数值越大越高）：L0 > L0.5 > L1 > L1x > 跨卷锚点。
pub const LAYER_PRIORITY: &[(&str, i32)] = &[
    ("L0", 100),
    ("L0.5", 90),
    ("L1", 80),
    ("L1x", 70),
    ("GUARD", 60),
    ("ANCHOR", 50),
    ("L2", 40),
    ("L3", 30),
    ("L4", 20),
];

pub fn layer_priority(layer: &str) -> i32 {
    LAYER_PRIORITY.iter().find(|(l, _)| *l == layer).map(|(_, p)| *p).unwrap_or(0)
}

/// LTM category → 治理层名。
pub fn ltm_category_to_layer(cat: &str, key: &str) -> &'static str {
    match cat {
        "顶层设定" => {
            if key.contains("脊梁") { "L0.5" } else { "L0" }
        }
        "分卷大纲" => {
            if key.contains("宏大对抗") { "L1x" }
            else if key.contains("护栏") { "GUARD" }
            else { "L1" }
        }
        "锚点元" => "ANCHOR",
        "世界观铁则" | "角色静态档案" => "GUARD",
        _ => "L4",
    }
}

// ────────────────────────────────── 通用模型

#[derive(Debug, Clone, Serialize)]
pub struct LayerEntry {
    pub layer: String,
    pub category: String,
    pub key: String,
    pub source: String,
    pub chars: usize,
    /// 条目内出现的 code 前缀集合（如 CHAR/LOCA/EVNT…）
    pub types: Vec<String>,
    /// 检查结论：OK | WARN | HIGH-RISK
    pub level: String,
    pub notes: Vec<String>,
}

/// 治理库打开：只读既有库 + 幂等附加治理表。
pub fn open_gov(lib_root: &Path) -> Result<Connection> {
    let conn = Connection::open(lib_root.join("novel.db"))?;
    conn.execute_batch(include_str!("../governance_schema.sql"))?;
    ensure_library_index_cols(&conn)?;
    ensure_risk_index_cols(&conn)?;
    Ok(conn)
}

/// 幂等补齐 risk_index 的语义治理列（is_semantic/law_basis）；已存在则跳过。
/// 区分【结构脏点（library-ctl，is_semantic=0）】与【世界观法理脏点（world-gov，is_semantic=1）】。
pub fn ensure_risk_index_cols(conn: &Connection) -> Result<()> {
    let mut have = std::collections::HashSet::new();
    {
        let mut st = conn.prepare("PRAGMA table_info(risk_index)")?;
        let it = st.query_map([], |r| r.get::<_, String>(1))?;
        for x in it.filter_map(|v| v.ok()) { have.insert(x); }
    }
    if !have.contains("is_semantic") {
        conn.execute("ALTER TABLE risk_index ADD COLUMN is_semantic INTEGER NOT NULL DEFAULT 0", [])?;
    }
    if !have.contains("law_basis") {
        conn.execute("ALTER TABLE risk_index ADD COLUMN law_basis TEXT DEFAULT ''", [])?;
    }
    Ok(())
}

/// 写入一条**语义类**风险（world-gov 专用；is_semantic=1 并携带法理依据原话）。
/// 幂等规则与 record_risk 一致；返回 true=新插入，false=已存在被幂等跳过。
pub fn record_risk_semantic(
    conn: &Connection,
    risk_type: &str,
    volume: Option<&str>,
    chapter: Option<u32>,
    source: &str,
    level: &str,
    message: &str,
    law_basis: &str,
) -> Result<bool> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT id FROM risk_index WHERE risk_type=?1 AND IFNULL(volume,'')=IFNULL(?2,'')
               AND IFNULL(chapter,0)=IFNULL(?3,0) AND message=?4 AND resolved=0 LIMIT 1",
            params![risk_type, volume, chapter, message],
            |r| r.get(0),
        )
        .optional()?;
    if exists.is_none() {
        conn.execute(
            "INSERT INTO risk_index(risk_type,volume,chapter,source,message,level,is_semantic,law_basis)
             VALUES(?1,?2,?3,?4,?5,?6,1,?7)",
            params![risk_type, volume, chapter, source, message, level, law_basis],
        )?;
        Ok(true)
    } else {
        Ok(false)
    }
}

// ────────────────────────────────── 语义总门控（集中收口）

/// 编译期硬开关：**默认 false**。
/// ⚠️ 禁止直接改成 true 绕过运行时条件；三条件全真才允许真实模型请求。
pub const SEMANTIC_ENABLE_GATE: bool = false;

/// 门控拒绝时的统一提示文案。
pub const SEMANTIC_LOCKED_MSG: &str =
    "requested_but_locked（三条件未同时成立：SEMANTIC_ENABLE_GATE && --semantic && NOVEL_SEMANTIC_UNLOCK=yes）";

/// 门控纯逻辑（可测）：三条件全真才开门。
pub fn gate_logic(compile_gate: bool, cli_semantic: bool, env_unlock: bool) -> bool {
    compile_gate && cli_semantic && env_unlock
}

/// 门控集中收口：所有将来要调模型的点必须经过它，**禁止裸 call llm::call**。
pub fn get_semantic_gate(cli_semantic: bool) -> bool {
    let env_ok = std::env::var("NOVEL_SEMANTIC_UNLOCK").map(|v| v == "yes").unwrap_or(false);
    gate_logic(SEMANTIC_ENABLE_GATE, cli_semantic, env_ok)
}

/// 门控状态明细（gate-check 用）：打印三条件真值。
pub fn semantic_gate_status(cli_semantic: bool) -> Value {
    let env_raw = std::env::var("NOVEL_SEMANTIC_UNLOCK").unwrap_or_default();
    let env_ok = env_raw == "yes";
    let open = gate_logic(SEMANTIC_ENABLE_GATE, cli_semantic, env_ok);
    json!({
        "compile_gate": SEMANTIC_ENABLE_GATE,
        "cli_semantic": cli_semantic,
        "env_unlock": env_ok,
        "env_raw": if env_raw.is_empty() { "(未设置)".to_string() } else { env_raw },
        "open": open,
        "status": if open { "OPEN（允许真实模型请求）".to_string() } else { SEMANTIC_LOCKED_MSG.to_string() },
    })
}

/// 幂等补齐 library_index 的新列（title/tags/src_mtime）；已存在则跳过。
/// `src_mtime` = init 时快照的**文件 mtime（Unix 秒）**，供增量扫描 `--since` 使用；
/// 只读 index（不再 stat 磁盘），避免跨设备磁盘时间乱跳。旧库该列为 0 → 增量优雅降级全扫。
fn ensure_library_index_cols(conn: &Connection) -> Result<()> {
    let mut have = std::collections::HashSet::new();
    {
        let mut st = conn.prepare("PRAGMA table_info(library_index)")?;
        let it = st.query_map([], |r| r.get::<_, String>(1))?;
        for x in it.filter_map(|v| v.ok()) { have.insert(x); }
    }
    for (name, ty) in [
        ("title", "TEXT DEFAULT ''"),
        ("tags", "TEXT DEFAULT '[]'"),
        ("src_mtime", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        if !have.contains(name) {
            conn.execute(&format!("ALTER TABLE library_index ADD COLUMN {name} {ty}"), [])?;
        }
    }
    Ok(())
}

/// 层级识别（基于相对路径）。百万库关键函数。
pub fn classify_layer(rel: &str) -> &'static str {
    let r = rel.replace('\\', "/");
    if r.ends_with("03_叙事规划/00_整本概括.md") { return "L0"; }
    if r.contains("00_故事脊梁") { return "L0.5"; }
    if r.contains("00_叙事总纲") { return "L1"; }
    if r.contains("00_宏大对抗总规划") { return "L1x"; }
    if r.contains("护栏") { return "GUARD"; }
    if r.contains("跨卷线锚点表") { return "ANCHOR"; }
    if r.contains("_规范/全局铁则") { return "GUARD"; }
    if r.contains("03_叙事规划/细纲") { return "L3"; }
    if r.contains("03_叙事规划/分卷大纲") { return "L2"; }
    if r.contains("02_资料库/") { return "L4"; }
    // 工程辅助文件（非叙事素材）：prompts / reports / 工单 / README / PLAN / rules
    if r.contains("05_管线框架/") || r.contains("/工单_") || r.ends_with("/README.md")
        || r.ends_with("/PLAN_L5.md") || r.contains("/rules.md") {
        return "AUX";
    }
    "UNCLASSIFIED"
}

/// 提取 H1 标题（首行 `# ...`）。
pub fn extract_title(text: &str) -> String {
    for line in text.lines().take(20) {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("# ") { return rest.trim().to_string(); }
    }
    String::new()
}

/// 提取标签：YAML frontmatter 的 tags 字段 + 行内 `> 胶囊：` 首词。
pub fn extract_tags(text: &str) -> Vec<String> {
    let mut tags = Vec::new();
    // frontmatter
    if let Some(rest) = text.strip_prefix("---") {
        if let Some(end) = rest.find("\n---") {
            for line in rest[..end].lines() {
                let l = line.trim();
                if let Some(v) = l.strip_prefix("tags:") {
                    for part in v.trim_matches(['[', ']']).split(',') {
                        let t = part.trim_matches(|c: char| c=='\'' || c=='"' || c==' ').to_string();
                        if !t.is_empty() { tags.push(t); }
                    }
                }
            }
        }
    }
    tags.sort(); tags.dedup();
    tags
}

/// 从内存行读小文本（百万体量：按字节上限读，不整库入内存）。
pub fn read_head(path: &Path, max_bytes: usize) -> String {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else { return String::new(); };
    let mut buf = vec![0u8; max_bytes];
    let n = f.read(&mut buf).unwrap_or(0);
    buf.truncate(n);
    String::from_utf8_lossy(&buf).to_string()
}

/// 文件字符数（不整读内存：流式统计）。
pub fn file_chars(path: &Path) -> usize {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else { return 0; };
    let mut buf = [0u8; 64 * 1024];
    let mut s = String::new();
    loop {
        let n = f.read(&mut buf).unwrap_or(0);
        if n == 0 { break; }
        s.push_str(&String::from_utf8_lossy(&buf[..n]));
        if s.len() > 8 * 1024 * 1024 { break; } // 单文件上限保护
    }
    s.chars().count()
}

/// 写入一条风险（幂等：同 type+volume+chapter+message 未解决时不重复插入）。
pub fn record_risk(
    conn: &Connection,
    risk_type: &str,
    volume: Option<&str>,
    chapter: Option<u32>,
    source: &str,
    level: &str,
    message: &str,
) -> Result<()> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT id FROM risk_index WHERE risk_type=?1 AND IFNULL(volume,'')=IFNULL(?2,'')
               AND IFNULL(chapter,0)=IFNULL(?3,0) AND message=?4 AND resolved=0 LIMIT 1",
            params![risk_type, volume, chapter, message],
            |r| r.get(0),
        )
        .optional()?;
    if exists.is_none() {
        conn.execute(
            "INSERT INTO risk_index(risk_type,volume,chapter,source,message,level)
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![risk_type, volume, chapter, source, message, level],
        )?;
    }
    Ok(())
}

/// 从文本抽取全局 code（大写前缀-NN-NNN）。
pub fn extract_codes(text: &str) -> Vec<String> {
    // 算法：先定位 `-NN-NNN` 三段，再向前回吃连续大写字母作前缀。
    // 这样 `CHAR-90-038` 不会被截成 `HAR-90-038`。
    let mut out = BTreeSet::new();
    let chars: Vec<char> = text.chars().collect();
    let is_d = |c: char| c.is_ascii_digit();
    let is_up = |c: char| c.is_ascii_uppercase();
    let mut i = 0usize;
    while i + 6 < chars.len() {
        // 期望 chars[i] == '-'
        if chars[i] == '-'
            && is_d(chars[i + 1]) && is_d(chars[i + 2])
            && chars[i + 3] == '-'
            && is_d(chars[i + 4]) && is_d(chars[i + 5]) && is_d(chars[i + 6])
        {
            // 第 7 个字符必须不是数字（防止 -99-0001 误拼）
            let tail_digit = i + 7 < chars.len() && is_d(chars[i + 7]);
            if !tail_digit {
                // 向前回吃大写前缀
                let mut p = i;
                while p > 0 && is_up(chars[p - 1]) { p -= 1; }
                let plen = i - p;
                // 前缀边界：前一个字符不能是大写或数字
                let boundary_ok = p == 0 || !(is_up(chars[p - 1]) || is_d(chars[p - 1]));
                if plen >= 2 && plen <= 5 && boundary_ok {
                    let prefix: String = chars[p..i].iter().collect();
                    let d1: String = chars[i + 1..i + 3].iter().collect();
                    let d2: String = chars[i + 4..i + 7].iter().collect();
                    out.insert(format!("{prefix}-{d1}-{d2}"));
                    i += 7;
                    continue;
                }
            }
        }
        i += 1;
    }
    out.into_iter().collect()
}

/// 从 code 提取类型前缀（CHAR / LOCA / EVNT …）。
pub fn code_prefix(code: &str) -> String {
    code.split('-').next().unwrap_or("").to_string()
}

// ────────────────────────────────── 1. audit-layer-health

/// 枚举当前生效 L0~锚点（来自 long_term_memory）+ 可选语义。
pub fn layer_health(conn: &Connection) -> Result<Vec<LayerEntry>> {
    let mut stmt = conn.prepare(
        "SELECT category,key,content,priority,IFNULL(source,'') FROM long_term_memory
         ORDER BY priority, category, key",
    )?;
    let rows: Vec<(String, String, String, i64, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
        .filter_map(|x| x.ok())
        .collect();
    let mut out = Vec::new();
    for (cat, key, content, _pri, source) in rows {
        let layer = ltm_category_to_layer(&cat, &key).to_string();
        let chars = content.chars().count();
        let mut types: Vec<String> = extract_codes(&content).iter().map(|c| code_prefix(c)).collect();
        types.sort();
        types.dedup();
        let mut notes = Vec::new();
        let mut level = "OK".to_string();
        if content.trim().is_empty() {
            notes.push("空条目".into());
            level = "HIGH-RISK".into();
        }
        if chars > 8000 {
            notes.push(format!("超长条目（{chars} 字 > 8000）"));
            level = "WARN".into();
        }
        if key.trim().is_empty() {
            notes.push("缺 key".into());
            level = "HIGH-RISK".into();
        }
        if cat.trim().is_empty() {
            notes.push("缺 category".into());
            level = "HIGH-RISK".into();
        }
        if types.is_empty() && layer == "L4" {
            notes.push("缺 type（无可识别 code 前缀）".into());
            if level == "OK" { level = "WARN".into(); }
        }
        out.push(LayerEntry { layer, category: cat, key, source, chars, types, level, notes });
    }
    Ok(out)
}

// ────────────────────────────────── 2. library-ctl

/// 扫描 02_资料库 全部 md，提取 层级/分类/key/类型/脏标记 并写入 library_index。
/// **只建索引，不动原文件**。返回脏条目清单。
pub fn library_init(conn: &Connection, lib_root: &Path) -> Result<Value> {
    // 扫描资料库工程全部 md（覆盖 02_资料库/03_叙事规划/05_管线框架 等）
    let mut files = Vec::new();
    for sub in ["02_资料库", "03_叙事规划", "05_管线框架"] {
        collect_md(&lib_root.join(sub), &mut files)?;
    }
    let mut dirty = Vec::new();
    let mut by_layer: BTreeMap<String, (usize, usize)> = BTreeMap::new(); // layer -> (files, chars)
    let mut total_chars = 0usize;
    let mut unclassified = 0usize;
    conn.execute("DELETE FROM library_index", [])?;
    for f in &files {
        let rel = f.strip_prefix(lib_root).unwrap_or(f).to_string_lossy().to_string();
        let head = read_head(f, 4096);
        let chars = file_chars(f);
        total_chars += chars;
        let layer = classify_layer(&rel).to_string();
        let title = extract_title(&head);
        let tags = extract_tags(&head);
        let codes = extract_codes(&head);
        let mut types: Vec<String> = codes.iter().map(|c| code_prefix(c)).collect();
        types.sort(); types.dedup();
        let category = rel.split('/').nth(1).unwrap_or("").to_string();
        let key = f.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let e = by_layer.entry(layer.clone()).or_default();
        e.0 += 1; e.1 += chars;
        let mut notes: Vec<String> = Vec::new();
        if layer == "UNCLASSIFIED" { notes.push("未归类".into()); unclassified += 1; }
        if chars == 0 { notes.push("空文件".into()); }
        if title.is_empty() && layer != "L3" { notes.push("缺标题(H1)".into()); }
        let is_dirty = !notes.is_empty();
        // 文件 mtime（Unix 秒），供增量扫描 --since；只在 init 时写一次，消费侧只读 index
        let src_mtime: i64 = std::fs::metadata(f)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        conn.execute(
            "INSERT INTO library_index(layer,category,key,source,chars,types,title,tags,dirty,dirty_note,src_mtime)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![layer, category, key, rel, chars as i64,
                    serde_json::to_string(&types)?, title, serde_json::to_string(&tags)?,
                    if is_dirty {1} else {0}, notes.join("；"), src_mtime],
        )?;
        if is_dirty { dirty.push(json!({"file": rel, "layer": layer, "note": notes.join("；")})); }
    }
    let layers: Vec<Value> = by_layer.iter().map(|(k, (n, c))| json!({"layer": k, "files": n, "chars": c})).collect();
    Ok(json!({
        "total_files": files.len(),
        "total_chars": total_chars,
        "by_layer": layers,
        "unclassified": unclassified,
        "dirty_count": dirty.len(),
        "dirty_sample": dirty.iter().take(20).collect::<Vec<_>>(),
    }))
}

fn collect_md(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<()> {
    if !dir.exists() { return Ok(()); }
    for e in std::fs::read_dir(dir)? {
        let p = e?.path();
        if p.is_dir() { collect_md(&p, out)?; }
        else if p.extension().map(|x| x == "md").unwrap_or(false) { out.push(p); }
    }
    Ok(())
}

/// 资料库深度巡检（无模型）。
pub fn library_audit(conn: &Connection, lib_root: &Path) -> Result<Value> {
    // ① 同 category 疑似重复 key
    let mut dup = Vec::new();
    {
        let mut st = conn.prepare("SELECT category,key,COUNT(*) c FROM library_index WHERE layer='L4' GROUP BY category,key HAVING c>1")?;
        let it = st.query_map([], |r| Ok((r.get::<_,String>(0)?, r.get::<_,String>(1)?, r.get::<_,i64>(2)?)))?;
        for x in it.filter_map(|v| v.ok()) { dup.push(json!({"category": x.0, "key": x.1, "count": x.2})); }
    }
    // ② v1/v2/v3 多版本残留（文件名含 _vN）
    let mut versions = Vec::new();
    {
        let mut st = conn.prepare("SELECT key,source FROM library_index WHERE layer='L4' AND key LIKE '%_v%'")?;
        let it = st.query_map([], |r| Ok((r.get::<_,String>(0)?, r.get::<_,String>(1)?)))?;
        for x in it.filter_map(|v| v.ok()) { versions.push(json!({"key": x.0, "source": x.1})); }
    }
    // ③ 悬空引用：L3 细纲引用了 code，但 materials 表无此卡
    let detail = lib_root.join("03_叙事规划/细纲");
    let mut dangling = Vec::new();
    let mut l3_refs: BTreeSet<String> = BTreeSet::new();
    if let Ok(rd) = std::fs::read_dir(&detail) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().map(|x| x == "md").unwrap_or(false) {
                let txt = std::fs::read_to_string(&p).unwrap_or_default();
                for c in extract_codes(&txt) { l3_refs.insert(c); }
            }
        }
    }
    let mut st = conn.prepare("SELECT code FROM materials")?;
    let have: BTreeSet<String> = st.query_map([], |r| r.get::<_, String>(0))?
        .filter_map(|x| x.ok()).collect();
    for c in &l3_refs { if !have.contains(c) { dangling.push(c.clone()); } }
    // ④ 孤立卡片：materials 有卡，但无任何卷/章节引用（ref_by 为空且不在 l3_refs）
    let mut orphan = Vec::new();
    {
        let mut st = conn.prepare("SELECT code FROM materials")?;
        let codes: Vec<String> = st.query_map([], |r| r.get::<_, String>(0))?.filter_map(|x| x.ok()).collect();
        for c in codes {
            if !l3_refs.contains(&c) { orphan.push(c); }
        }
    }
    // ⑤ 非法标签/type/格式
    let mut bad = Vec::new();
    {
        let mut st = conn.prepare("SELECT source,types FROM library_index WHERE layer='L4'")?;
        let it = st.query_map([], |r| Ok((r.get::<_,String>(0)?, r.get::<_,String>(1)?)))?;
        for x in it.filter_map(|v| v.ok()) {
            let ts: Vec<String> = serde_json::from_str(&x.1).unwrap_or_default();
            if ts.is_empty() { bad.push(json!({"source": x.0, "note": "无 type" })); }
        }
    }
    // 入库风险
    for d in &dup { record_risk(conn, "library", None, None, "library-ctl audit", "WARN",
        &format!("疑似重复 key: {} {}", d["category"], d["key"]))?; }
    for c in &dangling { record_risk(conn, "library", None, None, "library-ctl audit", "HIGH-RISK",
        &format!("悬空引用：L3 引用 {c} 但 materials 无此卡"))?; }
    Ok(json!({
        "dup_key": dup,
        "version_residue": versions,
        "dangling_refs": dangling,
        "orphan_cards_count": orphan.len(),
        "orphan_cards_sample": orphan.iter().take(20).collect::<Vec<_>>(),
        "bad_entries": bad,
        "l3_refs_total": l3_refs.len(),
        "materials_total": have.len(),
    }))
}


/// 分类建议（只读；不改原文件）。输出每条：当前层级|建议层级|建议tag|字数，可导 CSV/MD。
pub fn library_classify(conn: &Connection, out: Option<&Path>) -> Result<Value> {
    let mut st = conn.prepare(
        "SELECT source,layer,category,key,title,chars,tags,types FROM library_index ORDER BY layer,source")?;
    let rows: Vec<(String,String,String,String,String,i64,String,String)> = st
        .query_map([], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?)))?
        .filter_map(|x| x.ok()).collect();
    let mut items = Vec::new();
    let mut md = String::from("| 文件 | 当前层级 | 建议层级 | 建议tag | 字数 |
|---|---|---|---|---|
");
    let mut csv = String::from("source,current_layer,suggest_layer,suggest_tag,chars
");
    for (src, layer, cat, key, title, chars, tags_json, types_json) in rows {
        let types: Vec<String> = serde_json::from_str(&types_json).unwrap_or_default();
        // 建议层级：路径判定；L4 卡若有专有 type 保持 L4，否则若在 _规范 下建议 GUARD。
        let mut suggest = layer.clone();
        let mut reason = String::new();
        if layer == "UNCLASSIFIED" {
            if src.contains("_规范") { suggest = "GUARD".into(); reason = "_规范 目录".into(); }
            else if types.contains(&"CHAR".to_string()) || src.contains("角色") { suggest = "L4".into(); reason = "疑似角色卡".into(); }
            else { suggest = "UNCLASSIFIED".into(); reason = "无层级特征，需人工判定".into(); }
        }
        // 建议 tag：类型前缀 join
        let mut tag = types.join("|");
        if tag.is_empty() { tag = "(无)".into(); }
        let _ = (cat, key, title);
        md.push_str(&format!("| {} | {} | {} | {} | {} |\n", src, layer, suggest, tag, chars));
        csv.push_str(&format!("{},{},{},{},{}
", src, layer, suggest, tag, chars));
        if suggest != layer {
            items.push(json!({"source": src, "current": layer, "suggest": suggest, "reason": reason}));
        }
    }
    let mut written = Vec::new();
    if let Some(dir) = out {
        std::fs::create_dir_all(dir)?;
        let md_p = dir.join("classify_list.md");
        let csv_p = dir.join("classify_list.csv");
        std::fs::write(&md_p, md)?;
        std::fs::write(&csv_p, csv)?;
        written.push(json!({"markdown": md_p.to_string_lossy(), "csv": csv_p.to_string_lossy()}));
    }
    Ok(json!({"total": items.len() + 0, "suggest_changes": items, "written": written}))
}

/// 大盘统计：一眼掌握百万库全貌。
pub fn library_stats(conn: &Connection) -> Result<Value> {
    let mut by_layer: Vec<Value> = Vec::new();
    {
        let mut st = conn.prepare(
            "SELECT layer,COUNT(*),COALESCE(SUM(chars),0),COALESCE(SUM(dirty),0) FROM library_index GROUP BY layer ORDER BY layer")?;
        let it = st.query_map([], |r| Ok((r.get::<_,String>(0)?, r.get::<_,i64>(1)?, r.get::<_,i64>(2)?, r.get::<_,i64>(3)?)))?;
        for x in it.filter_map(|v| v.ok()) {
            by_layer.push(json!({"layer": x.0, "items": x.1, "chars": x.2, "dirty": x.3}));
        }
    }
    let total_items: i64 = conn.query_row("SELECT COUNT(*) FROM library_index", [], |r| r.get(0)).unwrap_or(0);
    let total_chars: i64 = conn.query_row("SELECT COALESCE(SUM(chars),0) FROM library_index", [], |r| r.get(0)).unwrap_or(0);
    let version_frag: i64 = conn.query_row(
        "SELECT COUNT(*) FROM library_index WHERE key LIKE '%_v%'", [], |r| r.get(0)).unwrap_or(0);
    let unclassified: i64 = conn.query_row(
        "SELECT COUNT(*) FROM library_index WHERE layer='UNCLASSIFIED'", [], |r| r.get(0)).unwrap_or(0);
    let dup: i64 = conn.query_row(
        "SELECT COALESCE(SUM(c-1),0) FROM (SELECT category,key,COUNT(*) c FROM library_index WHERE layer='L4' GROUP BY category,key HAVING c>1)", [], |r| r.get(0)).unwrap_or(0);
    Ok(json!({
        "total_items": total_items,
        "total_chars": total_chars,
        "by_layer": by_layer,
        "version_fragments": version_frag,
        "unclassified": unclassified,
        "dup_key_excess": dup,
    }))
}

/// 分层导出版（--layer），写文件不动库。支持 "L4" 或 "L0-L1x" 或省略。
pub fn library_export_layer(conn: &Connection, lib_root: &Path, out_dir: &Path, layer: Option<&str>) -> Result<Value> {
    std::fs::create_dir_all(out_dir)?;
    // 解析 layer 过滤
    let set: Option<std::collections::HashSet<String>> = layer.map(|l| {
        if let Some((a, b)) = l.split_once('-') {
            let order = ["L0","L0.5","L1","L1x","GUARD","ANCHOR","L2","L3","L4"];
            let ia = order.iter().position(|x| *x == a).unwrap_or(0);
            let ib = order.iter().position(|x| *x == b).unwrap_or(order.len()-1);
            order[ia.min(ib)..=ia.max(ib)].iter().map(|s| s.to_string()).collect()
        } else {
            let mut h = std::collections::HashSet::new();
            h.insert(l.to_string()); h
        }
    });
    // LTM 层
    let layers = layer_health(conn)?;
    let ltm: Vec<&LayerEntry> = layers.iter().filter(|e| set.as_ref().map(|s| s.contains(&e.layer)).unwrap_or(true)).collect();
    // L4 卡索引
    let mut l4 = Vec::new();
    if set.as_ref().map(|s| s.contains("L4")).unwrap_or(true) {
        if let Ok(mut st) = conn.prepare("SELECT category,key,source,chars,title,types,tags,dirty FROM library_index WHERE layer='L4' ORDER BY category,key") {
            if let Ok(it) = st.query_map([], |r| Ok(json!({
                "category": r.get::<_,Option<String>>(0)?,
                "key": r.get::<_,String>(1)?,
                "source": r.get::<_,Option<String>>(2)?,
                "chars": r.get::<_,i64>(3)?,
                "title": r.get::<_,Option<String>>(4)?,
                "types": r.get::<_,Option<String>>(5)?,
                "tags": r.get::<_,Option<String>>(6)?,
                "dirty": r.get::<_,i64>(7)?,
            }))) {
                for x in it.filter_map(|v| v.ok()) { l4.push(x); }
            }
        }
    }
    let tag = layer.unwrap_or("all").replace('/', "_");
    let json_p = out_dir.join(format!("export_{tag}.json"));
    std::fs::write(&json_p, serde_json::to_string_pretty(&json!({"ltm": ltm, "l4": l4}))?)?;
    let mut md = format!("# 分层导出（layer={}）

## LTM 层级条目 {} 条

", layer.unwrap_or("all"), ltm.len());
    for e in &ltm { md.push_str(&format!("- [{}] {} / {}（{} 字）
", e.layer, e.category, e.key, e.chars)); }
    md.push_str(&format!("
## L4 卡片 {} 条

", l4.len()));
    for c in l4.iter().take(5000) {
        md.push_str(&format!("- {}/{}{}（{} 字）
", c["category"].as_str().unwrap_or(""), c["key"].as_str().unwrap_or(""),
            if c["dirty"].as_i64().unwrap_or(0)==1 {" ⚠脏"} else {""}, c["chars"]));
    }
    let md_p = out_dir.join(format!("export_{tag}.md"));
    std::fs::write(&md_p, md)?;
    Ok(json!({"layer": layer.unwrap_or("all"), "ltm_entries": ltm.len(), "l4_cards": l4.len(),
              "json": json_p.to_string_lossy(), "markdown": md_p.to_string_lossy()}))
}

/// 导出全部生效 L0~L4 条目为 JSON + Markdown 包（写文件，不动库）。
pub fn library_export(conn: &Connection, lib_root: &Path, out_dir: &Path) -> Result<Value> {
    std::fs::create_dir_all(out_dir)?;
    let layers = layer_health(conn)?;
    let json_path = out_dir.join("L0-L4_export.json");
    std::fs::write(&json_path, serde_json::to_string_pretty(&layers)?)?;
    let mut md = String::from("# 当前生效 L0~L4 条目导出\n\n");
    for e in &layers {
        md.push_str(&format!("## [{}] {} / {}\n- 来源: {}\n- 字数: {}\n- 类型: {:?}\n- 结论: {}{}\n\n",
            e.layer, e.category, e.key, e.source, e.chars, e.types, e.level,
            if e.notes.is_empty() { String::new() } else { format!("（{}）", e.notes.join("；")) }));
    }
    // L4 索引
    md.push_str("## L4 卡片索引（library_index）\n\n");
    if let Ok(mut st) = conn.prepare("SELECT category,key,source,chars,dirty FROM library_index WHERE layer='L4' ORDER BY category,key") {
        if let Ok(it) = st.query_map([], |r| Ok((r.get::<_,Option<String>>(0)?, r.get::<_,String>(1)?, r.get::<_,Option<String>>(2)?, r.get::<_,i64>(3)?, r.get::<_,i64>(4)?))) {
            for x in it.filter_map(|v| v.ok()) {
                md.push_str(&format!("- {} / {}（{} 字）{}\n", x.0.unwrap_or_default(), x.1, x.3, if x.4==1 {" ⚠脏"} else {""}));
            }
        }
    }
    let md_path = out_dir.join("L0-L4_export.md");
    std::fs::write(&md_path, md)?;
    Ok(json!({"json": json_path.to_string_lossy(), "markdown": md_path.to_string_lossy(), "entries": layers.len()}))
}

/// 基于磁盘源重建 LTM 索引（**不覆盖库**，只出清单供人工核对/import_ltm 用）。
pub fn library_reindex(lib_root: &Path) -> Result<Value> {
    let pairs = [
        ("L0", "03_叙事规划/00_整本概括.md"),
        ("L0.5", "03_叙事规划/00_故事脊梁.md"),
        ("L1", "03_叙事规划/00_叙事总纲.md"),
        ("L1x", "03_叙事规划/00_宏大对抗总规划.md"),
        ("GUARD", "03_叙事规划/护栏·规划约束.md"),
    ];
    let mut found = Vec::new();
    for (layer, rel) in pairs {
        let p = lib_root.join(rel);
        let exists = p.exists();
        let chars = if exists { std::fs::read_to_string(&p).map(|t| t.chars().count()).unwrap_or(0) } else { 0 };
        found.push(json!({"layer": layer, "source": rel, "exists": exists, "chars": chars}));
    }
    Ok(json!({"disk_sources": found, "note": "仅清单；不覆盖库。如需同步 LTM 请手动跑 import_ltm"}))
}

// ────────────────────────────────── 4. audit-refs / audit-risk-list

/// 全卷引用图谱：【显式引用】= L3 细纲主动写的 code；【被动带入】= 注入包选中但未显式引用。
pub fn refs_graph(conn: &Connection, lib_root: &Path, volume: &str) -> Result<Value> {
    let outline = std::fs::read_to_string(lib_root.join("03_叙事规划/细纲").join(format!("{volume}.md"))).unwrap_or_default();
    // 逐章切分，取每章显式引用
    let mut per_chapter = Vec::new();
    let mut cur = 0u32;
    let mut buf = String::new();
    for line in outline.lines() {
        if let Some(rest) = line.trim().strip_prefix("### ") {
            if cur > 0 {
                let refs: Vec<String> = extract_codes(&buf);
                per_chapter.push(json!({"chapter": cur, "explicit_refs": refs}));
            }
            cur = rest.trim_start_matches('第').split(|c: char| !c.is_ascii_digit()).next().unwrap_or("0").parse().unwrap_or(0);
            buf.clear();
        } else { buf.push_str(line); buf.push('\n'); }
    }
    if cur > 0 { let refs: Vec<String> = extract_codes(&buf); per_chapter.push(json!({"chapter": cur, "explicit_refs": refs})); }
    let _ = conn;
    Ok(json!({"volume": volume, "chapters": per_chapter.len(), "per_chapter": per_chapter}))
}

/// 风险清单（支持只看未解决）；输出按 结构脏点(is_semantic=0) / 语义脏点(is_semantic=1) 分组。
pub fn risk_list(conn: &Connection, unresolved_only: bool) -> Result<Value> {
    let sql = if unresolved_only {
        "SELECT id,risk_type,IFNULL(volume,''),IFNULL(chapter,0),source,level,message,ts,resolved,IFNULL(is_semantic,0),IFNULL(law_basis,'') FROM risk_index WHERE resolved=0 ORDER BY level DESC, id DESC"
    } else {
        "SELECT id,risk_type,IFNULL(volume,''),IFNULL(chapter,0),source,level,message,ts,resolved,IFNULL(is_semantic,0),IFNULL(law_basis,'') FROM risk_index ORDER BY id DESC"
    };
    let mut st = conn.prepare(sql)?;
    let rows: Vec<Value> = st.query_map([], |r| Ok(json!({
        "id": r.get::<_,i64>(0)?, "risk_type": r.get::<_,String>(1)?,
        "volume": r.get::<_,String>(2)?, "chapter": r.get::<_,i64>(3)?,
        "source": r.get::<_,String>(4)?, "level": r.get::<_,String>(5)?,
        "message": r.get::<_,String>(6)?, "ts": r.get::<_,String>(7)?,
        "resolved": r.get::<_,i64>(8)?,
        "is_semantic": r.get::<_,i64>(9)?,
        "law_basis": r.get::<_,String>(10)?,
    }))).map(|it| it.filter_map(|x| x.ok()).collect()).unwrap_or_default();
    let counts: BTreeMap<String, i64> = {
        let mut m = BTreeMap::new();
        if let Ok(mut s2) = conn.prepare("SELECT level,COUNT(*) FROM risk_index WHERE resolved=0 GROUP BY level") {
            if let Ok(it) = s2.query_map([], |r| Ok((r.get::<_,String>(0)?, r.get::<_,i64>(1)?))) {
                for x in it.filter_map(|v| v.ok()) { m.insert(x.0, x.1); }
            }
        }
        m
    };
    let structural: Vec<&Value> = rows.iter().filter(|r| r["is_semantic"].as_i64().unwrap_or(0) == 0).collect();
    let semantic: Vec<&Value> = rows.iter().filter(|r| r["is_semantic"].as_i64().unwrap_or(0) == 1).collect();
    Ok(json!({
        "total": rows.len(),
        "unresolved_by_level": counts,
        "structural_count": structural.len(),
        "semantic_count": semantic.len(),
        "items": rows,
    }))
}

// ────────────────────────────────── 5. lib-checkpoint

/// 里程碑快照：checkpoint + 导出 L0~L4 清单 + 章节索引（**不自动执行**）。
pub fn lib_checkpoint(conn: &Connection, lib_root: &Path, out_root: &Path) -> Result<Value> {
    let ts = chrono_like_now();
    let dir = out_root.join(format!("checkpoint_{ts}"));
    std::fs::create_dir_all(&dir)?;
    // ① wal_checkpoint(TRUNCATE)
    let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get::<_, i64>(0));
    // ② 干净库快照
    let snap = dir.join("novel.db");
    conn.execute("VACUUM INTO ?1", params![snap.to_string_lossy()])?;
    // ③ L0~L4 清单
    let layers = layer_health(conn)?;
    std::fs::write(dir.join("layers.json"), serde_json::to_string_pretty(&layers)?)?;
    // ④ 章节索引
    let mut chapters = Vec::new();
    if let Ok(mut st) = conn.prepare("SELECT volume,chapter,status,IFNULL(chars,0) FROM chapters") {
        if let Ok(it) = st.query_map([], |r| Ok(json!({"volume":r.get::<_,String>(0)?,"chapter":r.get::<_,i64>(1)?,"status":r.get::<_,String>(2)?}))) {
            for x in it.filter_map(|v| v.ok()) { chapters.push(x); }
        }
    }
    std::fs::write(dir.join("chapters.json"), serde_json::to_string_pretty(&chapters)?)?;
    Ok(json!({"dir": dir.to_string_lossy(), "db_snapshot": snap.to_string_lossy(), "layers": layers.len(), "chapters": chapters.len()}))
}


/// 幂等补齐 chapter_meta 治理列（已存在则跳过）。只 ADD COLUMN，不改既有列。
pub fn ensure_chapter_meta_gov_cols(conn: &Connection) -> Result<()> {
    let mut have = std::collections::HashSet::new();
    {
        let mut st = conn.prepare("PRAGMA table_info(chapter_meta)")?;
        let it = st.query_map([], |r| r.get::<_, String>(1))?;
        for x in it.filter_map(|v| v.ok()) { have.insert(x); }
    }
    let cols = [
        ("compliance_level", "TEXT DEFAULT ''"),
        ("compliance_note", "TEXT DEFAULT ''"),
        ("compliance_check_ts", "TEXT DEFAULT ''"),
        ("refs_l3", "TEXT DEFAULT '[]'"),
        ("refs_l4", "TEXT DEFAULT '[]'"),
    ];
    for (name, ty) in cols {
        if !have.contains(name) {
            conn.execute(&format!("ALTER TABLE chapter_meta ADD COLUMN {name} {ty}"), [])?;
        }
    }
    Ok(())
}

/// 回写合规结论（只更新指定章的 3 列，不碰正文）。
pub fn set_compliance(
    conn: &Connection, volume: &str, chapter: u32, level: &str, note: &str,
) -> Result<()> {
    ensure_chapter_meta_gov_cols(conn)?;
    conn.execute(
        "UPDATE chapter_meta SET compliance_level=?1, compliance_note=?2,
           compliance_check_ts=datetime('now') WHERE volume=?3 AND chapter=?4",
        params![level, note, volume, chapter],
    )?;
    Ok(())
}

/// 回写本章 refs（显式 L3 引用集合、L4 key 集合）。
pub fn set_refs(conn: &Connection, volume: &str, chapter: u32, refs_l3: &[String], refs_l4: &[String]) -> Result<()> {
    ensure_chapter_meta_gov_cols(conn)?;
    conn.execute(
        "UPDATE chapter_meta SET refs_l3=?1, refs_l4=?2 WHERE volume=?3 AND chapter=?4",
        params![serde_json::to_string(refs_l3)?, serde_json::to_string(refs_l4)?, volume, chapter],
    )?;
    Ok(())
}

// ────────────────────────────────── 3. audit-chapter-compliance

/// 合规复核（--semantic 后才由调用方传 llm）；此处组装权威基准文本。
pub fn compliance_baseline(conn: &Connection, lib_root: &Path, volume: &str, chapter: u32) -> Result<(String, Vec<String>, Vec<String>)> {
    // 权威基准 = 全套 L0~L4 + 本卷锚点（截断防爆）
    let layers = layer_health(conn)?;
    let mut base = String::from("# 权威基准（L0≫L0.5≫L1≫L1x≫锚点≫L3≫L4）
");
    for e in &layers {
        base.push_str(&format!("
## [{}] {}
", e.layer, e.key));
        // 只取前 800 字作基准（避免超大）
        if let Ok(mut st) = conn.prepare("SELECT substr(content,1,800) FROM long_term_memory WHERE category=?1 AND key=?2") {
            if let Ok(v) = st.query_row(params![e.category, e.key], |r| r.get::<_, String>(0)) {
                base.push_str(&v); base.push('\n');
            }
        }
    }
    // 本卷 L3 细纲本章显式引用 + L4 卡片
    let outline = std::fs::read_to_string(lib_root.join("03_叙事规划/细纲").join(format!("{volume}.md"))).unwrap_or_default();
    // 简单切本章
    let mut cur = 0u32; let mut buf = String::new(); let mut this = String::new();
    for line in outline.lines() {
        if let Some(rest) = line.trim().strip_prefix("### ") {
            if cur == chapter { this = buf.clone(); }
            cur = rest.trim_start_matches('第').split(|c: char| !c.is_ascii_digit()).next().unwrap_or("0").parse().unwrap_or(0);
            buf.clear();
        } else { buf.push_str(line); buf.push('\n'); }
    }
    if cur == chapter { this = buf; }
    let refs_l3 = extract_codes(&this);
    let mut refs_l4 = Vec::new();
    for c in &refs_l3 {
        if let Ok(mut st) = conn.prepare("SELECT code FROM materials WHERE code=?1 LIMIT 1") {
            if st.query_row(params![c], |r| r.get::<_, String>(0)).is_ok() { refs_l4.push(c.clone()); }
        }
    }
    base.push_str(&format!("
## 本章 L3 细纲（第{chapter}章）
{}", this));
    Ok((base, refs_l3, refs_l4))
}

fn chrono_like_now() -> String {
    // 避免引入新依赖：用系统时间手拼。
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    format!("{}", d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// code 抽取必须完整：CHAR-90-038 不得被截成 HAR-90-038。
    #[test]
    fn extract_codes_keeps_full_prefix() {
        let t = "主角 CHAR-90-038 与 CIVI-99-014、FORE-99-020、ITEM-99-005，坐标 -10-20。";
        let c = extract_codes(t);
        assert!(c.contains(&"CHAR-90-038".to_string()), "got {c:?}");
        assert!(c.contains(&"CIVI-99-014".to_string()), "got {c:?}");
        assert!(c.contains(&"FORE-99-020".to_string()), "got {c:?}");
        assert!(c.contains(&"ITEM-99-005".to_string()), "got {c:?}");
        assert!(!c.iter().any(|x| x.starts_with("HAR-")), "不应有截断前缀: {c:?}");
    }

    /// 层级优先级：L0 > L0.5 > L1 > L1x > ANCHOR。
    #[test]
    fn layer_priority_supremacy() {
        assert!(layer_priority("L0") > layer_priority("L0.5"));
        assert!(layer_priority("L0.5") > layer_priority("L1"));
        assert!(layer_priority("L1") > layer_priority("L1x"));
        assert!(layer_priority("L1x") > layer_priority("ANCHOR"));
        assert!(layer_priority("ANCHOR") > layer_priority("L3"));
    }

    /// category→layer 映射。
    #[test]
    fn category_maps_to_layer() {
        assert_eq!(ltm_category_to_layer("顶层设定", "整本概括"), "L0");
        assert_eq!(ltm_category_to_layer("顶层设定", "故事脊梁"), "L0.5");
        assert_eq!(ltm_category_to_layer("分卷大纲", "叙事总纲"), "L1");
        assert_eq!(ltm_category_to_layer("分卷大纲", "宏大对抗总规划"), "L1x");
        assert_eq!(ltm_category_to_layer("锚点元", "跨卷线锚点表"), "ANCHOR");
    }
}
