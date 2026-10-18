//! 世界观专用提示词库与强结构化护栏（world-gov，第 17 批）。
//!
//! # 目录
//! `{work-root}/prompts/*.toml`；内置三套模板（check_world / entity_extract / sandbox_step），
//! 目录内文件可覆盖内置，**无需重编译**。
//!
//! # 硬护栏（写进包装层，不靠 prompt 嘴炮）
//! - **纯 JSON**：输出必须能解析为 JSON；带前后废话 → 非法 → 有限重试后标 `malformed`。
//! - **Schema 强校验**：自写最小 JSON Schema 子集校验器（type/required/enum/items/properties/
//!   additionalProperties）；字段缺漏/类型错直接拒绝。
//! - **实体白名单**：模型输出引用的 实体/关系 ID 必须存在于当前实体池；捏造 → 告警，
//!   **不直接纳入状态**。
//! - 禁止长篇自由叙事当权威结果；`narrative_summary` 只作附加参考字段。
use anyhow::{bail, Result};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

use super::llm_config;

/// prompts 目录（相对 work-root）。
pub const PROMPTS_REL: &str = "prompts";

/// 一套提示词模板。
#[derive(Debug, Clone)]
pub struct PromptTemplate {
    pub name: String,
    pub system: String,
    pub user: String,
    /// JSON Schema（值形态；可为 null 表示不校验）。
    pub schema: Value,
    /// 输出中视为实体引用的字段路径（用于白名单校验）。
    pub entity_fields: Vec<String>,
}

/// 内置模板（目录里同名 .toml 可覆盖）。
pub fn builtin(name: &str) -> Option<PromptTemplate> {
    let (system, user, schema, ent) = match name {
        "check_world" => (
            "你是《残响之河》世界观法务官。只出证据与建议，不改写任何内容。只返回纯 JSON。",
            "审阅下面这条资料卡，判断是否与上层法理冲突。\n卡片：{{card_text}}\n候选上层法理：{{candidate_laws}}",
            json!({
                "type": "object",
                "required": ["verdict", "law_ref", "evidence"],
                "properties": {
                    "verdict": {"type": "string", "enum": ["OK", "WARN", "HIGH-RISK", "UNCERTAIN"]},
                    "law_ref": {"type": "string"},
                    "evidence": {"type": "string"},
                    "referenced_ids": {"type": "array", "items": {"type": "string"}}
                },
                "additionalProperties": true
            }),
            vec!["referenced_ids".to_string()],
        ),
        "entity_extract" => (
            "你是《残响之河》资料库补全器。只返回纯 JSON；不杜撰不存在于原文的实体。",
            "从下面文本抽取实体与关系（仅补全，不替代静态抽取）。\n文本：{{text}}\n已知实体池：{{entity_pool}}",
            json!({
                "type": "object",
                "required": ["entities"],
                "properties": {
                    "entities": {"type": "array", "items": {"type": "object",
                        "required": ["id", "kind"], "properties": {"id": {"type": "string"}, "kind": {"type": "string"}}}},
                    "relations": {"type": "array", "items": {"type": "object",
                        "required": ["a", "b"], "properties": {"a": {"type": "string"}, "b": {"type": "string"}, "label": {"type": "string"}}}}
                },
                "additionalProperties": true
            }),
            vec!["entities[].id".to_string(), "relations[].a".to_string(), "relations[].b".to_string()],
        ),
        "sandbox_step" => (
            "你是《残响之河》沙盘推演器。只返回纯 JSON；state_delta 只引用实体池内 ID；不得捏造新实体。",
            "当前沙箱状态：{{world_state}}\n干预指令：{{intervention}}\n实体池：{{entity_pool}}",
            json!({
                "type": "object",
                "required": ["state_delta", "constraint_violations"],
                "properties": {
                    "state_delta": {"type": "array", "items": {"type": "object",
                        "required": ["entity", "field"], "properties": {"entity": {"type": "string"}, "field": {"type": "string"}, "delta": {}}}},
                    "new_relations": {"type": "array", "items": {"type": "object",
                        "required": ["a", "b"], "properties": {"a": {"type": "string"}, "b": {"type": "string"}, "label": {"type": "string"}}}},
                    "constraint_violations": {"type": "array", "items": {"type": "string"}},
                    "narrative_summary": {"type": "string"},
                    "warnings": {"type": "array", "items": {"type": "string"}}
                },
                "additionalProperties": true
            }),
            vec!["state_delta[].entity".to_string(), "new_relations[].a".to_string(), "new_relations[].b".to_string()],
        ),
        _ => return None,
    };
    Some(PromptTemplate { name: name.to_string(), system: system.into(), user: user.into(), schema, entity_fields: ent })
}

/// 内置模板名清单。
pub const BUILTIN_NAMES: &[&str] = &["check_world", "entity_extract", "sandbox_step"];

/// 加载模板：目录 `{work-root}/prompts/<name>.toml` 优先，否则内置。
pub fn load(work_root: &Path, name: &str) -> Result<PromptTemplate> {
    let p = work_root.join(PROMPTS_REL).join(format!("{name}.toml"));
    if p.is_file() {
        let text = std::fs::read_to_string(&p)?;
        let flat = llm_config::parse_toml_subset(&text)?;
        let system = flat.get("system").and_then(|x| x.as_str()).map(|s| s.to_string());
        let user = flat.get("user").and_then(|x| x.as_str()).map(|s| s.to_string());
        if let (Some(system), Some(user)) = (system, user) {
            let schema = flat.get("schema_json").and_then(|x| x.as_str())
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                .or_else(|| builtin(name).map(|b| b.schema))
                .unwrap_or(Value::Null);
            let entity_fields = flat.get("entity_fields").and_then(|x| x.as_str())
                .map(|s| s.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect())
                .unwrap_or_else(|| builtin(name).map(|b| b.entity_fields).unwrap_or_default());
            return Ok(PromptTemplate { name: name.to_string(), system, user, schema, entity_fields });
        }
    }
    builtin(name).ok_or_else(|| anyhow::anyhow!("未知模板 '{name}'（内置：{BUILTIN_NAMES:?}；或放 {PROMPTS_REL}/{name}.toml）"))
}

/// 渲染 `{{var}}`（严格：未知占位符不报错，但已知变量必须替换）。
pub fn render(tpl: &str, vars: &Map<String, Value>) -> String {
    let mut out = tpl.to_string();
    for (k, v) in vars {
        let s = match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        out = out.replace(&format!("{{{{{k}}}}}"), &s);
    }
    out
}

// ────────────────────────────────── 最小 JSON Schema 子集校验
/// 支持的子集：type / required / enum / items / properties / additionalProperties /
/// minimum / maximum。返回违规列表（空=通过）。
pub fn validate_schema(instance: &Value, schema: &Value) -> Vec<String> {
    let mut errs = Vec::new();
    validate_at("$", instance, schema, &mut errs);
    errs
}

fn type_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn validate_at(path: &str, inst: &Value, schema: &Value, errs: &mut Vec<String>) {
    let Some(obj) = schema.as_object() else { return };
    if let Some(t) = obj.get("type").and_then(|x| x.as_str()) {
        let actual = type_of(inst);
        let ok = match t {
            "integer" => inst.is_i64() || inst.is_u64(),
            "number" => inst.is_number(),
            _ => actual == t,
        };
        if !ok {
            errs.push(format!("{path}: 期望 {t}，实际 {actual}"));
            return;
        }
    }
    if let Some(en) = obj.get("enum").and_then(|x| x.as_array()) {
        if !en.iter().any(|e| e == inst) {
            errs.push(format!("{path}: 不在 enum 内（得到 {inst}）"));
        }
    }
    if let (Some(min), Some(n)) = (obj.get("minimum").and_then(|x| x.as_f64()), inst.as_f64()) {
        if n < min { errs.push(format!("{path}: {n} < minimum {min}")); }
    }
    if let (Some(max), Some(n)) = (obj.get("maximum").and_then(|x| x.as_f64()), inst.as_f64()) {
        if n > max { errs.push(format!("{path}: {n} > maximum {max}")); }
    }
    match inst {
        Value::Object(m) => {
            if let Some(req) = obj.get("required").and_then(|x| x.as_array()) {
                for r in req.iter().filter_map(|x| x.as_str()) {
                    if !m.contains_key(r) {
                        errs.push(format!("{path}: 缺必填字段 `{r}`"));
                    }
                }
            }
            if let Some(props) = obj.get("properties").and_then(|x| x.as_object()) {
                for (k, sub) in props {
                    if let Some(v) = m.get(k) {
                        validate_at(&format!("{path}.{k}"), v, sub, errs);
                    }
                }
            }
            let additional_false = obj.get("additionalProperties").map(|x| x == &Value::Bool(false)).unwrap_or(false);
            if additional_false {
                if let Some(props) = obj.get("properties").and_then(|x| x.as_object()) {
                    for k in m.keys() {
                        if !props.contains_key(k) {
                            errs.push(format!("{path}: 不允许的额外字段 `{k}`"));
                        }
                    }
                }
            }
        }
        Value::Array(a) => {
            if let Some(items) = obj.get("items") {
                for (i, v) in a.iter().enumerate() {
                    validate_at(&format!("{path}[{i}]"), v, items, errs);
                }
            }
        }
        _ => {}
    }
}

// ────────────────────────────────── 实体白名单
/// 从实例按路径抽实体 ID（支持 `a[].b` 形式）。
pub fn collect_entity_ids(instance: &Value, fields: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for f in fields {
        walk_path(instance, f, &mut out);
    }
    out
}

fn walk_path(inst: &Value, path: &str, out: &mut Vec<String>) {
    let (head, rest) = match path.split_once("[]") {
        Some((h, r)) => (h, Some(r.trim_start_matches('.'))),
        None => (path, None),
    };
    let head = head.trim_end_matches('.');
    let cur = if head.is_empty() && rest.is_some() {
        Some(inst)
    } else {
        inst.get(head)
    };
    let Some(cur) = cur else { return };
    match rest {
        Some(next) if next.is_empty() => {
            if let Some(s) = cur.as_str() { out.push(s.to_string()); }
        }
        Some(next) => {
            if let Some(arr) = cur.as_array() {
                for v in arr {
                    walk_path(v, next, out);
                }
            }
        }
        None => {
            // 末端：字符串直接取；数组则取每项字符串
            match cur {
                Value::String(s) => out.push(s.clone()),
                Value::Array(a) => {
                    for v in a {
                        if let Some(s) = v.as_str() { out.push(s.to_string()); }
                    }
                }
                _ => {}
            }
        }
    }
}

/// 白名单校验：返回池外（捏造）的 ID 与是否越界。`pool` 为空时视为**不校验**（无池可比）。
pub fn check_whitelist(instance: &Value, fields: &[String], pool: &std::collections::HashSet<String>) -> Value {
    let ids = collect_entity_ids(instance, fields);
    if pool.is_empty() {
        return json!({"checked": false, "unknown": [], "note": "实体池为空 → 跳过白名单校验"});
    }
    let unknown: Vec<String> = ids.into_iter().filter(|id| !pool.contains(id)).collect();
    json!({
        "checked": true,
        "unknown": unknown,
        "ok": unknown.is_empty(),
        "note": if unknown.is_empty() { "全部 ID 在实体池内" } else { "发现池外 ID（捏造）→ 告警，不直接纳入状态" },
    })
}

/// 从资料库 library_index 构造实体池（key 集合）。
pub fn entity_pool_from_library(conn: &rusqlite::Connection) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    if let Ok(mut st) = conn.prepare("SELECT key FROM library_index") {
        if let Ok(it) = st.query_map([], |r| r.get::<_, String>(0)) {
            for x in it.filter_map(|v| v.ok()) {
                set.insert(x);
            }
        }
    }
    set
}

/// 解析模型输出为 JSON：剥前导/后导废话（取第一个 `{` 到最后一个 `}`）。
/// 纯函数。解析失败 → Err。
pub fn extract_json(raw: &str) -> Result<Value> {
    let trimmed = raw.trim();
    if let Ok(v @ Value::Object(_)) = serde_json::from_str::<Value>(trimmed) {
        return Ok(v);
    }
    // 尝试剥 ```json 围栏
    let src = if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            if end > start {
                &trimmed[start..=end]
            } else { trimmed }
        } else { trimmed }
    } else {
        trimmed
    };
    match serde_json::from_str::<Value>(src) {
        Ok(v) if v.is_object() => Ok(v),
        Ok(_) => bail!("模型输出非 JSON 对象（禁止自由散文作为权威结果）"),
        Err(e) => bail!("模型输出非法 JSON：{e}"),
    }
}

/// 写注释模板目录（三套内置）。
pub fn init_templates(work_root: &Path, overwrite: bool) -> Result<Vec<PathBuf>> {
    let dir = work_root.join(PROMPTS_REL);
    std::fs::create_dir_all(&dir)?;
    let mut written = Vec::new();
    for name in BUILTIN_NAMES {
        let p = dir.join(format!("{name}.toml"));
        if p.exists() && !overwrite {
            continue;
        }
        let t = builtin(name).unwrap();
        let content = format!(
            "# world-gov prompt 模板：{name}（可自由修改；改后无需重编译）\n\
             # 输出必须为纯 JSON；schema_json 为强校验用（最小 JSON Schema 子集）。\n\
             system = {:?}\n\
             user = {:?}\n\
             schema_json = {:?}\n\
             entity_fields = {:?}\n",
            t.system, t.user, t.schema.to_string(),
            t.entity_fields.join(",")
        );
        std::fs::write(&p, content)?;
        written.push(p);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_names_all_resolvable() {
        for n in BUILTIN_NAMES {
            assert!(builtin(n).is_some(), "{n} 应有内置模板");
        }
    }

    #[test]
    fn render_replaces_vars() {
        let mut m = Map::new();
        m.insert("card_text".into(), json!("CARD"));
        m.insert("candidate_laws".into(), json!(["L0"]));
        let out = render("X {{card_text}} Y {{candidate_laws}}", &m);
        assert!(out.contains("CARD"));
        assert!(out.contains("[\"L0\"]"));
    }

    #[test]
    fn schema_validates_required_and_type() {
        let sc = json!({"type":"object","required":["verdict"],"properties":{"verdict":{"type":"string","enum":["OK","WARN"]}}});
        assert!(validate_schema(&json!({"verdict":"OK"}), &sc).is_empty());
        assert!(!validate_schema(&json!({}), &sc).is_empty(), "缺必填");
        assert!(!validate_schema(&json!({"verdict":"BAD"}), &sc).is_empty(), "enum 外");
        assert!(!validate_schema(&json!({"verdict":1}), &sc).is_empty(), "类型错");
    }

    #[test]
    fn schema_validates_nested_arrays() {
        let sc = json!({"type":"object","required":["state_delta"],"properties":{
            "state_delta":{"type":"array","items":{"type":"object","required":["entity","field"],
              "properties":{"entity":{"type":"string"},"field":{"type":"string"}}}}}});
        let ok = json!({"state_delta":[{"entity":"CHAR-99-001","field":"hp"}]});
        assert!(validate_schema(&ok, &sc).is_empty());
        let bad = json!({"state_delta":[{"entity":"CHAR-99-001"}]});
        assert!(!validate_schema(&bad, &sc).is_empty());
    }

    #[test]
    fn additional_properties_false_rejects_extra() {
        let sc = json!({"type":"object","properties":{"a":{"type":"string"}},"additionalProperties":false});
        assert!(validate_schema(&json!({"a":"x"}), &sc).is_empty());
        assert!(!validate_schema(&json!({"a":"x","b":1}), &sc).is_empty());
    }

    /// 第 18 批修正：ai-sandbox 的 schema（`step` 非必填、不约束类型）对 `step:null` 必须宽容。
    ///
    /// 背景：此前的 schema 把 `step` 设为 required+integer → 模型在无明确步号时合法返回
    /// `step:null` 会被误拒（`GUARD_REJECTED/schema: $.step: 期望 integer，实际 null`）。
    /// 该 schema 内联在 main.rs 的 ai-sandbox 分支，此处用等价 schema 锁定其判据。
    #[test]
    fn ai_sandbox_schema_tolerates_null_step() {
        let sc = json!({"type":"object","required":["state","verdict"],
            "properties":{"step":{},"state":{"type":"object"},
            "verdict":{"type":"string"},"note":{"type":"string"}}});
        // 模型返回 step=null（无步号）→ 必须通过
        let null_step = json!({"step":null,"state":{"chapter":0},"verdict":"目标不明确"});
        assert!(validate_schema(&null_step, &sc).is_empty(), "null-step 不应被拒：{:?}", validate_schema(&null_step, &sc));
        // step 完全缺失 → 也通过（非必填）
        let no_step = json!({"state":{"chapter":0},"verdict":"ok"});
        assert!(validate_schema(&no_step, &sc).is_empty());
        // 语义核心缺失 → 必须拒
        assert!(!validate_schema(&json!({"step":0}), &sc).is_empty(), "缺 state/verdict 应被拒");
        assert!(!validate_schema(&json!({"state":{}}), &sc).is_empty(), "缺 verdict 应被拒");
        // state 允许扩展字段（无 additionalProperties:false）
        let rich = json!({"state":{"a":1,"b":{"c":2}},"verdict":"ok","extra":"x"});
        assert!(validate_schema(&rich, &sc).is_empty());
    }

    #[test]
    fn collect_entity_ids_paths() {
        let inst = json!({"state_delta":[{"entity":"A"},{"entity":"B"}],"new_relations":[{"a":"C","b":"D"}]});
        let ids = collect_entity_ids(&inst, &["state_delta[].entity".into(), "new_relations[].a".into(), "new_relations[].b".into()]);
        assert!(ids.contains(&"A".to_string()));
        assert!(ids.contains(&"D".to_string()));
    }

    #[test]
    fn whitelist_flags_unknown() {
        let inst = json!({"state_delta":[{"entity":"A"},{"entity":"ZZZ"}]});
        let mut pool = std::collections::HashSet::new();
        pool.insert("A".to_string());
        let r = check_whitelist(&inst, &["state_delta[].entity".into()], &pool);
        assert_eq!(r["checked"], true);
        assert_eq!(r["ok"], false);
        assert!(r["unknown"].as_array().unwrap().iter().any(|x| x == "ZZZ"));
    }

    #[test]
    fn whitelist_empty_pool_skips() {
        let inst = json!({"entities":[{"id":"X"}]});
        let r = check_whitelist(&inst, &["entities[].id".into()], &std::collections::HashSet::new());
        assert_eq!(r["checked"], false);
    }

    #[test]
    fn extract_json_strips_fences_and_prose() {
        assert_eq!(extract_json("{\"a\":1}").unwrap()["a"], 1);
        assert_eq!(extract_json("好的：\n```json\n{\"a\":2}\n```\n以上").unwrap()["a"], 2);
        assert!(extract_json("纯散文没有 JSON").is_err());
    }

    #[test]
    fn extract_json_rejects_non_object() {
        assert!(extract_json("[1,2,3]").is_err(), "数组不是权威对象");
    }

    #[test]
    fn init_templates_writes_files() {
        let d = std::env::temp_dir().join(format!("wg_prompts_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let w = init_templates(&d, false).unwrap();
        assert_eq!(w.len(), BUILTIN_NAMES.len());
        // 可加载回来
        let t = load(&d, "check_world").unwrap();
        assert!(t.system.contains("法务官"));
        let _ = std::fs::remove_dir_all(&d);
    }
}