//! 状态表装载 —— `load_state_a.py` + `emuload.py` + `load_state_b.py`(续) 的 Rust 移植。
//!
//! **修复的历史 bug**：这些脚本全部硬编码
//! `ROOT = "/sdcard/projects/系统/资料库"`（`load_state_a.py:28`、`load_state_b.py:33`、
//! `emuload.py:31`、`tick.py:41`、`fill.py:20`）。真库实际在
//! `<资料库根>/`。
//! Rust 版路径由调用方注入。
//!
//! 本模块只做**解析 + 校验**（产出结构化行 / 报告），不写 DB。

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

// ═══════════════════════════ load_state_a.py 解析原语

/// `OTHER` 前缀（拒灌标记）。
pub fn is_other(s: &str) -> bool {
    s.starts_with("OTHER")
}

/// 清洗值（照抄 `clean`）。
pub fn clean(v: Option<&str>) -> Option<String> {
    let v = v?;
    let t = v.trim().trim_matches('*').trim();
    // 去前导 *>|- 与空白
    let t = t.trim_start_matches(|c: char| matches!(c, '*' | '>' | '|' | '-' | ' ') || c.is_whitespace());
    // 去尾部 * 与空白
    let t = t.trim_end_matches(|c: char| c == '*' || c.is_whitespace());
    if t.is_empty() { None } else { Some(t.to_string()) }
}

/// 卡编号（照抄 `card_code`）：H1 `CODE｜名` → `**档案编号**` → 文件名 stem。
/// `OTHER` 前缀 → `None`（拒灌）。
pub fn card_code(text: &str, file_stem: &str) -> Option<String> {
    let re_h1 = regex::Regex::new(r"^#\s+([A-Z]{2,6}-\d{2,4}(?:-\d{3})?)").unwrap();
    for line in text.lines() {
        let l = line.trim();
        if let Some(c) = re_h1.captures(l) {
            let code = c.get(1).unwrap().as_str().to_string();
            return if is_other(&code) { None } else { Some(code) };
        }
    }
    let re_arch = regex::Regex::new(r"\*\*档案编号\*\*[:：]\s*([A-Z]{2,6}-\d{2,4}(?:-\d{3})?)").unwrap();
    if let Some(c) = re_arch.captures(text) {
        let code = c.get(1).unwrap().as_str().to_string();
        return if is_other(&code) { None } else { Some(code) };
    }
    if is_other(file_stem) { None } else { Some(file_stem.to_string()) }
}

/// H1 名（照抄 `h1_name`）：`# CODE｜名` 或 `# 名`。
pub fn h1_name(text: &str) -> Option<String> {
    let first = text.lines().find(|l| l.starts_with('#'))?;
    let re = regex::Regex::new(r"^#\s+(?:[A-Z]{2,6}-\d{2,4}(?:-\d{3})?｜)?\s*(.+?)\s*$").unwrap();
    re.captures(first.trim())
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

/// `## 字段` H2 段正文（照抄 `section`）。
///
/// 终止条件（任一）：下一 H1~H3 / `**` 开头 / `>` 开头 / ```` ``` ```` / `===` / `- ` / `* `。
pub fn section(text: &str, fields: &[&str]) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let head = regex::Regex::new(&format!(
        r"^##\s+({})\s*(.*)$",
        fields.iter().map(|f| regex::escape(f)).collect::<Vec<_>>().join("|")
    ))
    .ok()?;
    let stop = regex::Regex::new(r"^#{1,3}\s").unwrap();
    let sep = regex::Regex::new(r"^\|[\s\-|]+\|$").unwrap();

    let mut found = false;
    let mut vals: Vec<String> = Vec::new();
    for ln in lines {
        if !found {
            if head.is_match(ln) {
                found = true;
            }
            continue;
        }
        let s0 = ln.trim();
        if s0.is_empty() {
            continue;
        }
        if stop.is_match(s0)
            || s0.starts_with("**")
            || s0.starts_with('>')
            || s0.starts_with("```")
            || s0.starts_with("===")
            || s0.starts_with("- ")
            || s0.starts_with("* ")
        {
            break;
        }
        let mut s = ln.trim().to_string();
        if s.starts_with('|') {
            if sep.is_match(&s) {
                continue;
            }
            let cells: Vec<&str> = s.trim_matches('|').split('|').map(|c| c.trim()).collect();
            s = cells.join(" | ");
        }
        if let Some(rest) = s.strip_prefix("- ") {
            s = rest.to_string();
        } else if let Some(rest) = s.strip_prefix('·') {
            s = rest.to_string();
        }
        vals.push(s);
    }
    if !found {
        return None;
    }
    let joined = vals.iter().filter(|v| !v.is_empty()).cloned().collect::<Vec<_>>().join(" ");
    clean(Some(joined.trim()))
}

/// 全卡范围 `**字段**：值`（照抄 `bold_any`）。
pub fn bold_any(text: &str, fields: &[&str]) -> Option<String> {
    let pat = format!(
        r"^\s*(?:[-*]\s+)?\*{{1,2}}\s*({})\s*[：:]?\s*\*{{1,2}}\s*[：:]?\s*(.+)$",
        fields.iter().map(|f| regex::escape(f)).collect::<Vec<_>>().join("|")
    );
    let re = regex::Regex::new(&pat).ok()?;
    for ln in text.lines() {
        if let Some(c) = re.captures(ln) {
            return clean(c.get(2).map(|m| m.as_str()));
        }
    }
    None
}

/// markdown 表格 `| 字段 | 值 |`（照抄 `table_cell`）。
pub fn table_cell(text: &str, fields: &[&str]) -> Option<String> {
    for ln in text.lines() {
        if !ln.trim_start().starts_with('|') {
            continue;
        }
        let cells: Vec<String> = ln.trim().trim_matches('|').split('|').map(|c| c.trim().to_string()).collect();
        if cells.len() < 2 {
            continue;
        }
        let key: String = cells[0].chars().filter(|c| !c.is_whitespace() && *c != '*').collect();
        if fields.contains(&key.as_str()) {
            return clean(Some(&cells[1]));
        }
    }
    None
}

/// 按优先级取字段（照抄 `field`）：H2 段（可选）→ 加粗行 → 表格（可选）。
pub fn field(text: &str, fields: &[&str], use_table: bool, h2: bool) -> Option<String> {
    if h2 {
        if let Some(v) = section(text, fields) {
            return Some(v);
        }
    }
    if let Some(v) = bold_any(text, fields) {
        return Some(v);
    }
    if use_table {
        if let Some(v) = table_cell(text, fields) {
            return Some(v);
        }
    }
    None
}

// ═══════════════════════════ emuload.py 校验器

/// 合法 basis（照抄 BASIS_VALID）。
pub const BASIS_VALID: [&str; 5] = ["实值", "推断", "null默认值", "null", "默认中性"];
/// 合法 lambda 档（照抄 LAMBDA_TIERS）。
pub const LAMBDA_TIERS: [&str; 5] = ["0.95", "0.9", "0.85", "0.7", "mid"];
/// `mid` 别名（照抄 LAMBDA_MID_ALIAS）。
pub const LAMBDA_MID_ALIAS: &str = "0.85";
/// 派系立场枚举（照抄 FACTION_STANCE_ENUM）。
pub const FACTION_STANCE_ENUM: [&str; 4] = ["上升", "守成", "衰落", "中立"];

/// 10 维情绪（照抄 `validate_char_calib` 的检查维度）。
pub const EMOTION_DIMS: [&str; 10] =
    ["anger", "fear", "grief", "joy", "anxiety", "calm", "shame", "awe", "compassion", "hope"];

/// `mid` → `0.85`（照抄 `_resolve_lambda`）。
pub fn resolve_lambda(v: &str) -> String {
    if v == "mid" {
        LAMBDA_MID_ALIAS.to_string()
    } else {
        v.to_string()
    }
}

/// 校验结论。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibValidation {
    pub valid_codes: Vec<String>,
    pub report: Vec<String>,
}

/// 校验角色标定 JSON（照抄 `validate_char_calib`）。
///
/// 口径：`basis ∈ BASIS_VALID`；`basis=实值/推断` 时 `value` 必须为数值且
/// emotion 维在 [0,100]、`suppress` 在 [0,1]、`P_base` 为数值、`lambda` 为字符串。
pub fn validate_char_calib(data: &Value, label: &str) -> CalibValidation {
    let mut report = Vec::new();
    let mut valid = Vec::new();

    // 顶层兼容 chars / characters
    let chars = data
        .get("characters")
        .or_else(|| data.get("chars"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    for c in &chars {
        let code = c.get("code").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if is_other(&code) {
            report.push(format!("[{label}] OTHER 前缀拒灌：{code}"));
            continue;
        }
        let mut bad: Vec<String> = Vec::new();
        let eb = c.get("emotion_base");

        for dim in EMOTION_DIMS {
            let Some(cell) = eb.and_then(|e| e.get(dim)) else {
                bad.push(format!("{dim}(missing)"));
                continue;
            };
            let b = cell.get("basis").and_then(|v| v.as_str()).unwrap_or("");
            if !BASIS_VALID.contains(&b) {
                bad.push(format!("{dim}(basis={b:?})"));
                continue;
            }
            let v = cell.get("value");
            if b == "实值" || b == "推断" {
                let ok = v.and_then(|x| x.as_f64()).map(|x| (0.0..=100.0).contains(&x)).unwrap_or(false);
                if !ok {
                    bad.push(format!("{dim}(value={:?} 越界)", v));
                }
            }
        }

        for fld in ["P_base", "suppress", "lambda"] {
            let Some(cell) = c.get(fld) else { continue };
            let b = cell.get("basis").and_then(|v| v.as_str()).unwrap_or("");
            if !BASIS_VALID.contains(&b) {
                bad.push(format!("{fld}(basis={b:?})"));
                continue;
            }
            let v = cell.get("value");
            if b == "实值" || b == "推断" {
                match fld {
                    "suppress" => {
                        let ok = v.and_then(|x| x.as_f64()).map(|x| (0.0..=1.0).contains(&x)).unwrap_or(false);
                        if !ok {
                            bad.push(format!("{fld}(value={v:?} 越界)"));
                        }
                    }
                    "P_base" => {
                        if v.and_then(|x| x.as_f64()).is_none() {
                            bad.push(format!("{fld}(value={v:?} 非数)"));
                        }
                    }
                    "lambda" => {
                        if v.and_then(|x| x.as_str()).is_none() {
                            bad.push(format!("{fld}(value={v:?} 非档字符串)"));
                        }
                    }
                    _ => {}
                }
            }
        }

        if !bad.is_empty() {
            report.push(format!("[{label}] {code} 拒灌：{bad:?}"));
            continue;
        }
        valid.push(code);
    }

    CalibValidation { valid_codes: valid, report }
}

/// 校验 `char_calib.json` 的顶层结构（照抄 `main` 里的字段断言）。
pub fn validate_calib_top(data: &Value) -> Result<()> {
    if data.get("characters").or_else(|| data.get("chars")).is_none() {
        return Err(anyhow!("char_calib 缺 characters/chars 顶层键"));
    }
    Ok(())
}

// ═══════════════════════════ tick.py 转换表

/// 事件状态迁移（照抄 `EVENT_TRANSITION`）。
pub fn event_transition(old: &str, resolve_ids: &[String], ev_id: &str) -> &'static str {
    if resolve_ids.iter().any(|r| r == ev_id) {
        return "resolved";
    }
    match old {
        "pending" => "pending",
        "active" => "active",
        _ => "pending",
    }
}

/// 动态表状态列（照抄 `DYN_STATE_COL`）。
pub fn dyn_state_col(table: &str) -> Option<&'static str> {
    match table {
        "anomaly_state" => Some("active"),
        "civ_relation_state" => Some("state"),
        "lore_state" => Some("status"),
        _ => None,
    }
}

/// 需要迁移的动态表（照抄 `DYN_TABLES`）。
pub const DYN_TABLES: [&str; 3] = ["anomaly_state", "civ_relation_state", "lore_state"];

/// 章级 tick 的落库载荷（对应 `log_tick` 的 payload，去掉 DB）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TickPayload {
    pub chapter: u32,
    pub volume: String,
    pub domain: String,
    pub note: String,
    pub content_hash: Option<String>,
}

/// 构造 tick 载荷（照抄 `log_tick` 的 payload 组装）。
pub fn build_tick_payload(
    chapter: u32,
    volume: &str,
    domain: &str,
    note: &str,
    content_hash: Option<&str>,
) -> TickPayload {
    TickPayload {
        chapter,
        volume: volume.to_string(),
        domain: domain.to_string(),
        note: note.to_string(),
        content_hash: content_hash.map(String::from),
    }
}

/// 下一序号（照抄 `pipeline_backend._next_seq`）：从 payload 里取最大 seq + 1。
pub fn next_seq(existing: &[Map<String, Value>]) -> i64 {
    existing
        .iter()
        .filter_map(|m| m.get("seq").and_then(|v| v.as_i64()))
        .max()
        .unwrap_or(0)
        + 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn clean_strips_noise() {
        assert_eq!(clean(Some("**值**")), Some("值".into()));
        assert_eq!(clean(Some("- 值")), Some("值".into()));
        assert_eq!(clean(Some("   ")), None);
        assert_eq!(clean(None), None);
    }

    #[test]
    fn card_code_three_fallbacks() {
        assert_eq!(card_code("# CHAR-99-001｜陈寂\n", "x"), Some("CHAR-99-001".into()));
        assert_eq!(card_code("**档案编号**：POW-01-002\n", "x"), Some("POW-01-002".into()));
        assert_eq!(card_code("无编号\n", "ITEM-03"), Some("ITEM-03".into()));
        // ⚠ 对齐 Python 实测：`# OTHER-xx｜甲` 里 `OTHER-xx` 的 `xx` **不是数字**，
        // 故 H1 正则不匹配 → 落到文件名 stem 兜底（返回 "x"），而非 None。
        assert_eq!(card_code("# OTHER-xx｜甲\n", "x"), Some("x".into()));
        // 真正的 OTHER 拒灌需编号是数字形态：
        assert_eq!(card_code("# OTHER-12｜甲\n", "x"), None);
        assert_eq!(card_code("无\n", "OTHER-yy"), None, "stem 以 OTHER 开头 → 拒灌");
    }

    #[test]
    fn h1_name_forms() {
        // ⚠ 实测（Rust 与 Python **行为一致**）：正则里编号前缀组可选，
        // 而 `\s*` 优先吞掉后面内容再回溯，导致 `# CHAR-1｜陈寂` 返回 **整个**
        // `CHAR-1｜陈寂`（编号未被剥离）。这是上游既有行为，Rust 版如实照抄，
        // 不"顺手修正"——避免与 Python 产生静默分歧。
        assert_eq!(h1_name("# CHAR-1｜陈寂\n"), Some("CHAR-1｜陈寂".into()));
        assert_eq!(h1_name("# 独立标题\n"), Some("独立标题".into()));
        assert_eq!(h1_name("无 H1\n"), None);
    }

    #[test]
    fn section_extracts_body_and_stops() {
        let t = "# T\n## 境界分层（注）\n炼气期\n凝响境\n\n## 下一节\nxx\n";
        assert_eq!(section(t, &["境界分层"]).as_deref(), Some("炼气期 凝响境"));
    }

    #[test]
    fn section_stops_at_bold_line() {
        let t = "## 力量\n炼气期\n**备注**：xx\n";
        assert_eq!(section(t, &["力量"]).as_deref(), Some("炼气期"));
    }

    #[test]
    fn section_converts_table_rows() {
        let t = "## 列表\n| a | b |\n| --- | --- |\n| 1 | 2 |\n";
        assert_eq!(section(t, &["列表"]).as_deref(), Some("a | b 1 | 2"));
    }

    #[test]
    fn bold_any_first_hit() {
        let t = "**修为**：炼气期\n**修为**：金丹\n";
        assert_eq!(bold_any(t, &["修为"]).as_deref(), Some("炼气期"));
    }

    #[test]
    fn table_cell_bold_key() {
        let t = "| **境界** | 凝响境 |\n";
        assert_eq!(table_cell(t, &["境界"]).as_deref(), Some("凝响境"));
    }

    #[test]
    fn field_priority_h2_then_bold_then_table() {
        let t = "## 力量\n段值\n**力量**：加粗值\n| 力量 | 表值 |\n";
        // h2=true → 段优先
        assert_eq!(field(t, &["力量"], true, true).as_deref(), Some("段值"));
        // h2=false → 加粗优先
        assert_eq!(field(t, &["力量"], true, false).as_deref(), Some("加粗值"));
        // h2=false, 无加粗 → 表格
        let t2 = "| 力量 | 表值 |\n";
        assert_eq!(field(t2, &["力量"], true, false).as_deref(), Some("表值"));
    }

    #[test]
    fn validate_calib_accepts_good() {
        let mut eb = Map::new();
        for d in EMOTION_DIMS {
            eb.insert(d.into(), json!({"basis": "实值", "value": 50}));
        }
        let d = json!({"characters": [{
            "code": "CHAR-1",
            "emotion_base": eb,
            "P_base": {"basis": "实值", "value": 1.0},
            "suppress": {"basis": "实值", "value": 0.6},
            "lambda": {"basis": "实值", "value": "0.85"},
        }]});
        let r = validate_char_calib(&d, "k1");
        assert_eq!(r.valid_codes, vec!["CHAR-1"]);
        assert!(r.report.is_empty());
    }

    #[test]
    fn validate_calib_rejects_out_of_range_and_other() {
        let mut eb = Map::new();
        for (i, d) in EMOTION_DIMS.iter().enumerate() {
            eb.insert((*d).into(), json!({"basis": "实值", "value": if i == 0 { 150 } else { 50 }}));
        }
        let d = json!({"characters": [
            {"code": "CHAR-1", "emotion_base": eb},
            {"code": "OTHER-9", "emotion_base": {}},
        ]});
        let r = validate_char_calib(&d, "k1");
        assert!(r.valid_codes.is_empty());
        assert!(r.report.iter().any(|m| m.contains("越界")), "{:?}", r.report);
        assert!(r.report.iter().any(|m| m.contains("OTHER 前缀拒灌")), "{:?}", r.report);
    }

    #[test]
    fn validate_calib_rejects_bad_basis_and_missing_dim() {
        let mut eb = Map::new();
        eb.insert("anger".into(), json!({"basis": "瞎写", "value": 50}));
        // 其余 9 维缺失
        let d = json!({"chars": [{"code": "CHAR-2", "emotion_base": eb}]});
        let r = validate_char_calib(&d, "k2");
        let joined = r.report.join(" | ");
        assert!(joined.contains("anger(basis="), "应报 basis 非法：{joined}");
        assert!(joined.contains("瞎写"), "应含原始 basis 值：{joined}");
        assert!(joined.contains("fear(missing)"), "应报缺失维：{joined}");
        assert!(r.valid_codes.is_empty());
    }

    #[test]
    fn resolve_lambda_mid_alias() {
        assert_eq!(resolve_lambda("mid"), "0.85");
        assert_eq!(resolve_lambda("0.95"), "0.95");
    }

    #[test]
    fn event_transition_table() {
        let none: Vec<String> = vec![];
        assert_eq!(event_transition("pending", &none, "E-1"), "pending");
        assert_eq!(event_transition("active", &none, "E-1"), "active");
        assert_eq!(event_transition("pending", &["E-1".into()], "E-1"), "resolved");
        assert_eq!(event_transition("未知", &none, "E-1"), "pending");
    }

    #[test]
    fn next_seq_increments() {
        let rows = vec![
            json!({"seq": 3}).as_object().unwrap().clone(),
            json!({"seq": 7}).as_object().unwrap().clone(),
        ];
        assert_eq!(next_seq(&rows), 8);
        assert_eq!(next_seq(&[]), 1);
    }

    #[test]
    fn dyn_state_col_map() {
        assert_eq!(dyn_state_col("anomaly_state"), Some("active"));
        assert_eq!(dyn_state_col("lore_state"), Some("status"));
        assert_eq!(dyn_state_col("nope"), None);
    }
}
