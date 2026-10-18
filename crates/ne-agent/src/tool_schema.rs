//! 工具参数 JSON Schema 校验（纯函数）—— 逐字对照太墟
//! `harness/validation/ToolSchemaValidator.kt`。
//!
//! 只实现工具定义实际用到的 JSON Schema 子集：`required / type / enum / minimum /
//! maximum / pattern / anyOf(required) / items / additionalProperties`。未知关键字一律忽略，
//! schema 为空/非对象时**宽容放行**（不阻断执行链路）。
//!
//! # 与太墟的差异（刻意的，见开工令）
//!
//! 太墟 `problemsFor(toolName, …)` 内部靠 `resolveSchema` 去**工具注册表**查 schema。
//! Rust 侧注册表解析属接线层，故本模块**只收 `schema: &Value`**，不传 toolName、
//! 不依赖 `protocol::builtin_tool_definitions`——测试全部用内联 schema。
use serde_json::{Map, Value};

/// 太墟 `MAX_PATTERN_LENGTH`：pattern 源码长度上限。
const MAX_PATTERN_LENGTH: usize = 256;
/// 太墟 `MAX_PATTERN_INPUT_LENGTH`：待匹配输入长度上限。
const MAX_PATTERN_INPUT_LENGTH: usize = 512;

/// 智能参数规整：解包 `params/arguments/input`（最多 8 层）、扁平键还原、内置别名映射。
/// `is_mcp_tool == true` 时前两项全跳过（MCP 参数名由远端 schema 定义）。
pub fn normalize_args(raw: &Value, apply_aliases: bool, is_mcp_tool: bool) -> Value {
    let obj = match raw.as_object() {
        Some(o) => o.clone(),
        None => return raw.clone(),
    };
    if is_mcp_tool {
        return Value::Object(obj);
    }
    // 1. 解包多层 params/arguments/input 包裹（最多 8 层，仅当对象恰有一个键且其值是对象）
    let mut base = obj;
    let mut wrap_depth = 0;
    while wrap_depth < 8 && base.len() == 1 {
        let key = base.keys().next().cloned().unwrap_or_default();
        let inner = match key.as_str() {
            "params" | "arguments" | "input" => base.get(&key).and_then(|v| v.as_object()).cloned(),
            _ => None,
        };
        match inner {
            Some(o) => {
                base = o;
                wrap_depth += 1;
            }
            None => break,
        }
    }
    // 2. 扁平键还原
    let unflattened = unflatten_object(&Value::Object(base));
    let mut map = match unflattened {
        Value::Object(m) => m,
        other => return other,
    };
    if !apply_aliases {
        return Value::Object(map);
    }
    // 3. 别名映射（目标键不存在时才补）
    add_alias(&mut map, "path", &["file_path", "filePath", "file", "target"]);
    add_alias(&mut map, "command", &["cmd", "script"]);
    add_alias(&mut map, "oldText", &["old_text", "original_text"]);
    add_alias(&mut map, "newText", &["new_text", "replacement"]);
    add_alias(
        &mut map,
        "timeout_seconds",
        &["timeout", "timeoutSeconds", "timeout_sec"],
    );
    Value::Object(map)
}

/// 目标键不存在时，从源别名里取第一个存在的值补上。
fn add_alias(map: &mut Map<String, Value>, target: &str, sources: &[&str]) {
    if map.contains_key(target) {
        return;
    }
    for s in sources {
        if let Some(v) = map.get(*s).cloned() {
            map.insert(target.to_string(), v);
            return;
        }
    }
}

/// 将带 `__` 或合规点号路径的扁平键还原为嵌套对象；同名既有嵌套对象则递归深合并。
/// 冲突保留语义（对齐太墟）：中间段撞标量 → 丢弃该扁平键；叶层已有标量 → 丢弃冲突值。
fn unflatten_object(obj: &Value) -> Value {
    use std::collections::BTreeMap;
    enum Node {
        Scalar(Value),
        Map(BTreeMap<String, Node>),
    }
    fn to_value(n: Node) -> Value {
        match n {
            Node::Scalar(v) => v,
            Node::Map(m) => {
                let mut o = Map::new();
                for (k, v) in m {
                    o.insert(k, to_value(v));
                }
                Value::Object(o)
            }
        }
    }
    fn insert(root: &mut BTreeMap<String, Node>, path: &[String], value: Value) {
        let Some((head, rest)) = path.split_first() else {
            return;
        };
        if rest.is_empty() {
            // 叶层
            match root.get(head) {
                Some(Node::Scalar(_)) => {} // 已有标量 → 丢弃冲突值
                Some(Node::Map(_)) => {
                    let final_value = if value.is_object() {
                        unflatten_object(&value)
                    } else {
                        value
                    };
                    if let Value::Object(fv) = final_value {
                        if let Some(Node::Map(m)) = root.get_mut(head) {
                            for (k, v) in fv {
                                insert(m, &[k], v);
                            }
                        }
                    } else if let Some(Node::Map(_)) = root.get(head) {
                        root.insert(head.clone(), Node::Scalar(final_value));
                    }
                }
                None => {
                    let final_value = if value.is_object() {
                        unflatten_object(&value)
                    } else {
                        value
                    };
                    root.insert(head.clone(), Node::Scalar(final_value));
                }
            }
            return;
        }
        // 中间层：确保 head 是 Map，否则（撞标量 → 丢弃该扁平键）
        if !matches!(root.get(head), Some(Node::Map(_))) {
            if root.contains_key(head) {
                return;
            }
            root.insert(head.clone(), Node::Map(BTreeMap::new()));
        }
        if let Some(Node::Map(m)) = root.get_mut(head) {
            insert(m, rest, value);
        }
    }

    let Some(obj_map) = obj.as_object() else {
        return obj.clone();
    };
    let mut root: BTreeMap<String, Node> = BTreeMap::new();
    for (key, value) in obj_map {
        let segments = split_flattened_key(key);
        if segments.len() > 1 {
            insert(&mut root, &segments, value.clone());
        } else {
            let processed = if value.is_object() {
                unflatten_object(value)
            } else {
                value.clone()
            };
            match root.get(key) {
                Some(Node::Scalar(_)) => {}
                Some(Node::Map(_)) => {
                    if let Value::Object(fv) = &processed {
                        if let Some(Node::Map(m)) = root.get_mut(key) {
                            for (k, v) in fv {
                                insert(m, std::slice::from_ref(k), v.clone());
                            }
                        }
                    }
                    // 冲突保留：叶层已有对象而新值是标量 → 保留既有对象
                }
                None => {
                    // 对象值必须以 Map 形式存入，否则后续 options__x 扁平键会撞上它被丢弃
                    if let Value::Object(fv) = processed {
                        let mut m = BTreeMap::new();
                        for (k, v) in fv {
                            insert(&mut m, &[k], v);
                        }
                        root.insert(key.clone(), Node::Map(m));
                    } else {
                        root.insert(key.clone(), Node::Scalar(processed));
                    }
                }
            }
        }
    }
    to_value(Node::Map(root))
}

/// `__` 优先；否则 `.` 分隔且每段均为合法标识符才拆（点号在首/尾不拆）。
fn split_flattened_key(key: &str) -> Vec<String> {
    if key.contains("__") {
        let parts: Vec<String> = key
            .split("__")
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();
        if parts.len() > 1 {
            return parts;
        }
    }
    if key.contains('.') && !key.starts_with('.') && !key.ends_with('.') {
        let parts: Vec<String> = key.split('.').map(|s| s.to_string()).collect();
        let all_valid = parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_alphanumeric() || c == '_'));
        if all_valid && parts.len() > 1 {
            return parts;
        }
    }
    vec![key.to_string()]
}

/// 直接对给定 schema 校验（纯函数）：先 normalize（不套别名）再校验。
pub fn validate(schema: &Value, args: &Value) -> Vec<String> {
    let normalized = normalize_args(args, false, false);
    validate_object(schema, &normalized, "", &std::collections::HashSet::new())
}

/// 先 normalize 再对给定 schema 校验（schema 由调用方解析后传入）。
pub fn problems_for(schema: &Value, args: &Value) -> Vec<String> {
    let normalized = normalize_args(args, true, false);
    validate_object(schema, &normalized, "", &std::collections::HashSet::new())
}

/// 对象级校验（对齐太墟 `validateObject`）。
fn validate_object(
    schema: &Value,
    obj: &Value,
    prefix: &str,
    inherited: &std::collections::HashSet<String>,
) -> Vec<String> {
    let mut problems = Vec::new();
    let Some(schema) = schema.as_object() else {
        return problems;
    };
    let Some(obj) = obj.as_object() else {
        return problems;
    };
    let obj_value = Value::Object(obj.clone());
    let alternatives: Vec<&Value> = schema
        .get("anyOf")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().collect())
        .unwrap_or_default();

    // allowedProperties = inherited + 本层 properties + anyOf 分支 properties
    let mut allowed: std::collections::HashSet<String> = inherited.clone();
    if let Some(props) = schema.get("properties").and_then(|v| v.as_object()) {
        allowed.extend(props.keys().cloned());
    }
    for alt in &alternatives {
        if let Some(props) = alt.get("properties").and_then(|v| v.as_object()) {
            allowed.extend(props.keys().cloned());
        }
    }

    // required
    if let Some(req) = schema.get("required").and_then(|v| v.as_array()) {
        for r in req {
            if let Some(name) = r.as_str() {
                if !obj.contains_key(name) {
                    problems.push(format!("缺少必填参数 {prefix}{name}"));
                }
            }
        }
    }

    // anyOf(required)：任一分支整体无问题即满足
    if !alternatives.is_empty() {
        let satisfied = alternatives
            .iter()
            .any(|alt| validate_object(alt, &obj_value, prefix, &allowed).is_empty());
        if !satisfied {
            let combos: Vec<String> = alternatives
                .iter()
                .filter_map(|alt| alt.get("required").and_then(|v| v.as_array()))
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str())
                        .collect::<Vec<_>>()
                        .join("/")
                })
                .filter(|s| !s.is_empty())
                .collect();
            if !combos.is_empty() {
                let hint = if combos.len() == 1 {
                    format!("至少提供 {}", combos[0])
                } else {
                    format!("{} 至少提供其中一组", combos.join(" 或 "))
                };
                problems.push(format!("参数组合不满足要求：{hint}"));
            }
        }
    }

    // 各属性
    if let Some(props) = schema.get("properties").and_then(|v| v.as_object()) {
        for (name, def) in props {
            let Some(value) = obj.get(name) else { continue };
            let child_prefix = format!("{prefix}{name}.");
            problems.extend(validate_value(def, value, &format!("{prefix}{name}"), &child_prefix));
        }
    }

    // 额外参数检测
    let allow_extra = schema
        .get("additionalProperties")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if let Some(props) = schema.get("properties").and_then(|v| v.as_object()) {
        let unknown: Vec<&String> = obj.keys().filter(|k| !allowed.contains(*k)).collect();
        if !unknown.is_empty() && !allow_extra {
            let at = if prefix.is_empty() {
                String::new()
            } else {
                format!("（位于 {prefix}层）")
            };
            let valid = if props.is_empty() {
                "无".to_string()
            } else {
                props.keys().cloned().collect::<Vec<_>>().join("、")
            };
            problems.push(format!(
                "不接受参数 {}{at}（该工具可用参数: {valid}）",
                unknown.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("、")
            ));
        }
    }
    problems
}

/// 值级校验（对齐太墟 `validateValue`）。
fn validate_value(schema: &Value, value: &Value, label: &str, prefix: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let Some(schema) = schema.as_object() else {
        return problems;
    };
    let ty = schema.get("type").and_then(|v| v.as_str());

    if let Some(t) = ty {
        if !matches_type(t, value) {
            problems.push(format!("参数 {label} 类型错误：应为 {t}，实际是{}", type_name(value)));
            return problems; // 类型不符，后续约束无意义
        }
    }

    // enum
    if let Some(allowed) = schema.get("enum").and_then(|v| v.as_array()) {
        if !allowed.is_empty() {
            let content = value.as_str().map(|s| s.to_string()).unwrap_or_else(|| raw_content(value));
            let ok = allowed
                .iter()
                .any(|a| prim_content(a).as_deref() == Some(content.as_str()));
            if !ok {
                let joined = allowed
                    .iter()
                    .map(|a| prim_content(a).unwrap_or_default())
                    .collect::<Vec<_>>()
                    .join("/");
                problems.push(format!("参数 {label} 的值 \"{content}\" 不在允许范围内：{joined}"));
            }
        }
    }

    // minimum / maximum（数字或字符串包裹的数字）
    if ty == Some("integer") || ty == Some("number") {
        if let Some(number) = as_number(value) {
            let min = schema.get("minimum").and_then(as_number);
            let max = schema.get("maximum").and_then(as_number);
            match (min, max) {
                (Some(lo), Some(hi)) if number < lo || number > hi => problems.push(format!(
                    "参数 {label} 的值 {} 超出允许范围 [{}, {}]",
                    fmt_num(number),
                    fmt_num(lo),
                    fmt_num(hi)
                )),
                (Some(lo), _) if number < lo => problems.push(format!(
                    "参数 {label} 的值 {} 小于最小允许值 {}",
                    fmt_num(number),
                    fmt_num(lo)
                )),
                (_, Some(hi)) if number > hi => problems.push(format!(
                    "参数 {label} 的值 {} 大于最大允许值 {}",
                    fmt_num(number),
                    fmt_num(hi)
                )),
                _ => {}
            }
        }
    }

    // pattern（仅字符串；带安全阀）
    if value.is_string() {
        if let Some(pattern) = schema.get("pattern").and_then(|v| v.as_str()) {
            let input = value.as_str().unwrap_or("");
            let safe = pattern.len() <= MAX_PATTERN_LENGTH
                && input.chars().count() <= MAX_PATTERN_INPUT_LENGTH
                && !pattern.contains('(')
                && !pattern.contains(')')
                && !pattern.contains('|')
                && pattern.chars().filter(|c| matches!(c, '*' | '+' | '?' | '{')).count() <= 2;
            let matched = if safe {
                regex::Regex::new(pattern)
                    .map(|re| re.is_match(input))
                    .unwrap_or(true)
            } else {
                true
            };
            if !matched {
                problems.push(format!("参数 {label} 格式不符合要求（需匹配 {pattern}）"));
            }
        }
    }

    // items（数组元素）
    if let Some(arr) = value.as_array() {
        if let Some(item_schema) = schema.get("items") {
            let base = prefix.strip_suffix('.').unwrap_or(prefix);
            for (index, element) in arr.iter().enumerate() {
                let item_prefix = format!("{base}[{index}].");
                if element.is_object() {
                    problems.extend(validate_object(item_schema, element, &item_prefix, &std::collections::HashSet::new()));
                } else {
                    problems.extend(validate_value(item_schema, element, &format!("{base}[{index}]"), &item_prefix));
                }
            }
        }
    }
    problems
}

/// 基本类型判定（含字符串包裹的数字/布尔）。
fn matches_type(t: &str, value: &Value) -> bool {
    match t {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => match value {
            Value::Bool(_) => true,
            Value::String(s) => s.eq_ignore_ascii_case("true") || s.eq_ignore_ascii_case("false"),
            _ => false,
        },
        "integer" => as_number(value).map(|n| n.fract() == 0.0).unwrap_or(false),
        "number" => as_number(value).is_some(),
        "null" => value.is_null(),
        _ => true, // 未知类型声明宽容放行
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Object(_) => "对象",
        Value::Array(_) => "数组",
        Value::Null => "null",
        Value::Bool(_) => "布尔值",
        Value::Number(_) => "数字",
        Value::String(s) => {
            if s.parse::<f64>().is_ok() {
                "数字"
            } else if s.eq_ignore_ascii_case("true") || s.eq_ignore_ascii_case("false") {
                "布尔值"
            } else {
                "字符串"
            }
        }
    }
}

/// 取数字（数字原样；字符串尝试解析）。
fn as_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    }
}

/// JSON 原始内容（用于 enum 比较的非字符串值）。
fn raw_content(value: &Value) -> String {
    match value {
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_string(),
        Value::String(s) => s.clone(),
        _ => value.to_string(),
    }
}

/// 基本值内容（enum 比较用）。
fn prim_content(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null => Some("null".to_string()),
        _ => None,
    }
}

/// 数字格式化（整数不带小数点，对齐太墟 `formatNumber`）。
fn fmt_num(n: f64) -> String {
    if n.fract() == 0.0 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    // ── 校验 ──
    #[test]
    fn reports_missing_required_fields() {
        let sc = schema(r#"{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}"#);
        let p = validate(&sc, &json!({}));
        assert!(p.iter().any(|s| s.contains("path") && s.contains("缺少必填")));
        assert!(p.iter().any(|s| s.contains("content") && s.contains("缺少必填")));
    }

    #[test]
    fn reports_enum_violation() {
        let sc = schema(r#"{"type":"object","properties":{"action":{"type":"string","enum":["start","status","logs","list","stop"]}}}"#);
        let p = validate(&sc, &json!({"action":"restart"}));
        assert_eq!(p.len(), 1);
        assert!(p[0].contains("不在允许范围内"));
        assert!(p[0].contains("start/status/logs/list/stop"));
    }

    #[test]
    fn reports_integer_range_violation() {
        let sc = schema(r#"{"type":"object","properties":{"limit":{"type":"integer","minimum":1,"maximum":50}}}"#);
        let p = validate(&sc, &json!({"limit":99}));
        assert_eq!(p.len(), 1);
        assert!(p[0].contains("超出允许范围"));
    }

    #[test]
    fn reports_type_mismatch_with_actual_type_name() {
        let sc = schema(r#"{"type":"object","properties":{"path":{"type":"string"}}}"#);
        let p = validate(&sc, &json!({"path":42}));
        assert_eq!(p.len(), 1);
        assert!(p[0].contains("类型错误"));
        assert!(p[0].contains("数字"));
    }

    #[test]
    fn reports_pattern_violation() {
        let sc = schema(r#"{"type":"object","properties":{"id":{"type":"string","pattern":"^[a-z0-9-]+$"}}}"#);
        let p = validate(&sc, &json!({"id":"Bad Id!"}));
        assert!(p.iter().any(|s| s.contains("格式不符合要求") && s.contains("id")));
    }

    #[test]
    fn validates_array_items_against_item_schema() {
        let sc = schema(
            r#"{"type":"object","properties":{"steps":{"type":"array","items":{"type":"object","properties":{"id":{"type":"string"},"status":{"type":"string","enum":["pending","done"]}},"required":["id","status"]}}},"required":["steps"]}"#,
        );
        let args = json!({"steps":[
            {"id":"s1","status":"pending"},
            {"id":"s2","status":"banana"},
            {"id":"s3"}
        ]});
        let p = validate(&sc, &args);
        assert!(p.iter().any(|s| s.contains("steps[1].status") && s.contains("不在允许范围内")));
        assert!(p.iter().any(|s| s.contains("steps[2].status") && s.contains("缺少必填")));
    }

    #[test]
    fn accepts_valid_arguments() {
        let sc = schema(r#"{"type":"object","properties":{"path":{"type":"string"},"offset":{"type":"integer"},"limit":{"type":"integer"}},"required":["path"]}"#);
        assert!(validate(&sc, &json!({"path":"/workspace/a.kt","offset":1,"limit":50})).is_empty());
    }

    #[test]
    fn skips_validation_when_schema_empty_or_non_object() {
        assert!(validate(&json!({}), &json!({"anything":1})).is_empty());
        assert!(validate(&json!("not-an-object"), &json!({"anything":1})).is_empty());
        assert!(validate(&json!(null), &json!({"x":1})).is_empty());
    }

    #[test]
    fn extra_unknown_arguments_are_reported_with_valid_parameter_list() {
        let sc = schema(r#"{"type":"object","properties":{"path":{"type":"string"}}}"#);
        let p = validate(&sc, &json!({"path":"a.kt","url":"https://x"}));
        assert!(p.iter().any(|s| s.contains("不接受参数 url") && s.contains("可用参数")));
    }

    #[test]
    fn unknown_arguments_are_tolerated_when_additionalProperties_is_true() {
        let sc = schema(r#"{"type":"object","properties":{"a":{"type":"string"}},"additionalProperties":true}"#);
        assert!(validate(&sc, &json!({"a":"x","extra":"y"})).is_empty());
    }

    #[test]
    fn anyof_branch_properties_are_accepted_alongside_parent_properties() {
        let sc = schema(r#"{"type":"object","properties":{"common":{"type":"string"}},"anyOf":[{"required":["left"],"properties":{"left":{"type":"string"}}},{"required":["right"],"properties":{"right":{"type":"string"}}}]}"#);
        assert!(validate(&sc, &json!({"common":"ok","left":"yes"})).is_empty());
        assert!(validate(&sc, &json!({"common":"ok","right":"yes"})).is_empty());
    }

    #[test]
    fn string_encoded_numbers_and_booleans_pass_type_validation() {
        let sc = schema(r#"{"type":"object","properties":{"count":{"type":"integer","minimum":1,"maximum":100},"flag":{"type":"boolean"}}}"#);
        assert!(validate(&sc, &json!({"count":"50","flag":"true"})).is_empty());
    }

    // ── normalizeArgs ──
    #[test]
    fn normalize_args_unflattens_double_underscore_keys() {
        let raw = json!({
            "database__host":"127.0.0.1",
            "database__port":5432,
            "database__auth__user":"admin",
            "timeout":10
        });
        let n = normalize_args(&raw, true, false);
        assert_eq!(n["database"]["host"], "127.0.0.1");
        assert_eq!(n["database"]["port"], 5432);
        assert_eq!(n["database"]["auth"]["user"], "admin");
        // timeout 是别名 → timeout_seconds
        assert_eq!(n["timeout_seconds"], 10);
    }

    #[test]
    fn normalize_args_unflattens_dot_separated_keys() {
        let raw = json!({"config.network.port":8080,"config.name":"my-server"});
        let n = normalize_args(&raw, true, false);
        assert_eq!(n["config"]["name"], "my-server");
        assert_eq!(n["config"]["network"]["port"], 8080);
    }

    #[test]
    fn normalize_args_deep_merges_explicit_object_with_flattened_keys() {
        let raw = json!({"options":{"retries":3},"options__timeout":30});
        let n = normalize_args(&raw, true, false);
        assert_eq!(n["options"]["retries"], 3);
        assert_eq!(n["options"]["timeout"], 30);
    }

    #[test]
    fn normalize_args_unwraps_multiple_levels_of_arguments_wrapper() {
        let single = json!({"arguments":{"command":"ls -la"}});
        assert_eq!(normalize_args(&single, true, false)["command"], "ls -la");
        let double = json!({"arguments":{"arguments":{"command":"ls -la"}}});
        let nd = normalize_args(&double, true, false);
        assert_eq!(nd["command"], "ls -la");
        assert!(nd.get("arguments").is_none());
        let deep = json!({"arguments":{"arguments":{"arguments":{"arguments":{"command":"pwd"}}}}});
        assert_eq!(normalize_args(&deep, true, false)["command"], "pwd");
    }

    #[test]
    fn normalize_args_keeps_legit_single_key_object() {
        let read_args = json!({"path":"/workspace/a.kt"});
        assert_eq!(normalize_args(&read_args, true, false), read_args);
        let scalar_wrap = json!({"input":"raw text"});
        assert_eq!(normalize_args(&scalar_wrap, true, false), scalar_wrap);
    }

    #[test]
    fn normalize_args_skips_unwrap_and_flatten_for_mcp_tool() {
        let raw = json!({"params":{"a":1},"options__timeout":30});
        assert_eq!(normalize_args(&raw, true, true), raw);
    }

    #[test]
    fn validate_passes_nested_schema_when_model_emits_flattened_keys() {
        let sc = schema(r#"{"type":"object","properties":{"db":{"type":"object","properties":{"host":{"type":"string"}},"required":["host"]}},"required":["db"]}"#);
        let flat = json!({"db__host":"localhost"});
        let p = validate(&sc, &flat);
        assert!(p.is_empty(), "flattened args should satisfy nested schema: {p:?}");
    }
}