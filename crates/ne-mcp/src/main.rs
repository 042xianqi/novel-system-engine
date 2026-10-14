//! ne-mcp —— 残响之河 novel-engine 的 MCP stdio 服务（太墟外挂宿主）。
//!
//! 用法（由太墟以 McpCommandBuilder 的 commandLine 形式拉起）：
//!   stty raw -echo; exec 'ne-mcp' '--root' '/sdcard/.../系统/资料库'
//!
//! 设计要点：
//!   * 纯 stdio、newline-delimited JSON-RPC，与太墟内置 MCP 脚本同构。
//!   * 零外部网络依赖（不引 tokio/reqwest），静态可控、启动极快。
//!   * 直连用户真实 novel.db（ne_db::connect(root/novel.db)），无硬编码路径、无副作用。

mod jsonrpc;
mod tools;
mod wg_tools;

use tools::Cfg;

const PROTOCOL_VERSION: &str = "2025-06-18";
const SERVER_NAME: &str = "taixu-novel-engine";
// 从 Cargo 包版本取（与 workspace 统一版本号，避免两处漂移）
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

fn tool_specs() -> serde_json::Value {
    serde_json::json!({"tools": [
        {
            "name": "funnel",
            "description": "读 funnel.yaml，报当前待推进的 stage（L0-L5 哪层未 done）及全部 stage 状态。",
            "inputSchema": {"type":"object","properties":{}}
        },
        {
            "name": "ne_cardgen",
            "description": "抽某卷细纲的【待建卡】工单，按类别归堆（角色/势力/地点/…），给卡名+来源引用+素材。",
            "inputSchema": {"type":"object","properties":{
                "volume": {"type":"string","description":"卷号，如 01"}},
                "required":["volume"]}
        },
        {
            "name": "ne_inject",
            "description": "读某章细纲 → 生成注入包（出场角色/关联资料/前文末尾/伏笔/事件/缺失引用），两层选卡带预算。",
            "inputSchema": {"type":"object","properties":{
                "volume": {"type":"string","description":"卷号，如 01"},
                "chapter": {"type":"integer","description":"章号"},
                "allow_missing": {"type":"boolean","description":"有缺失引用时是否仍返回注入包，默认 false"}},
                "required":["volume","chapter"]}
        },
        {
            "name": "ne_check",
            "description": "对正文跑 16 道门禁（G01-G16，与 ne-engine CLI 同一实现）→ verdict(pass/fix/reject) + 逐门禁结论 + P0/P1 清单；附带六维 AI 味维度（不参与 verdict）。",
            "inputSchema": {"type":"object","properties":{
                "text": {"type":"string","description":"待质检正文"},
                "chapter": {"type":"integer","description":"章号，默认 0"},
                "volume": {"type":"string","description":"卷名，默认「卷01」，影响 G03/G11 等跨章门禁"}},
                "required":["text"]}
        },
        {
            "name": "ne_materials",
            "description": "L1→L4 资料对账：逐卷读 L2 分卷大纲，抽卡引用，对 novel.db + 02_资料库磁盘对账，按卷报已建/缺失卡与 DB-磁盘漂移。volume 留空扫全 10 卷。",
            "inputSchema": {"type":"object","properties":{
                "volume": {"type":"string","description":"卷号，如 01；留空=全 10 卷"}},
                "required":[]}
        },
        {
            "name": "ne_db",
            "description": "连 novel.db，报各表行数与卡用量 Top（直连真实资料库）。",
            "inputSchema": {"type":"object","properties":{}}
        },
        {
            "name": "ne_db_query",
            "description": "对 novel.db 执行只读 SQL（SELECT/PRAGMA/EXPLAIN），支持 FTS5 MATCH。写操作会被拒绝。",
            "inputSchema": {"type":"object","properties":{
                "sql": {"type":"string"},
                "limit": {"type":"integer","description":"最大返回行数，默认 100"}},
                "required":["sql"]}
        },
        {
            "name": "wg_scan_library",
            "description": "world-gov 全库语义脏点扫描（world-gov scan-library 的 MCP 封装）。产探针清单；--semantic 仅标注通路，不在此处外呼。默认只扫 L2/L3/L4。",
            "inputSchema": {"type":"object","properties":{
                "slice": {"type":"string","description":"分片 N/M（如 1/4）；留空=不分片"},
                "semantic": {"type":"boolean","description":"是否开启语义通路（默认 false，仅产零模型骨架）"},
                "dry_run": {"type":"boolean","description":"只算不落盘（默认 false）"},
                "all_layers": {"type":"boolean","description":"关闭层过滤，扫描全部条目（默认 false）"},
                "since": {"type":"string","description":"增量：Unix 时间戳或 last（默认全量）"}},
                "required":[]}
        },
        {
            "name": "wg_inspect_card",
            "description": "world-gov 单卡深度检视（world-gov inspect-card 的 MCP 封装）。默认产探针包；semantic=true 才组装题面，real_semantic=true 才走统一收口真实推理并落 conflict_records（仍需三重锁）。",
            "inputSchema": {"type":"object","properties":{
                "key": {"type":"string","description":"卡片 key（如 CHAR-90-038）"},
                "semantic": {"type":"boolean","description":"开启才组装语义题面（默认 false）"},
                "real_semantic": {"type":"boolean","description":"真实外呼（三重锁之命令行锁；默认 false）"},
                "dry_run": {"type":"boolean","description":"只渲染 prompt 不发送（默认 false）"}},
                "required":["key"]}
        },
        {
            "name": "wg_inspect_batch",
            "description": "world-gov 批量检视（world-gov inspect-batch 的 MCP 封装）。keys 显式列表（空=全库 L2/L3/L4），slice 分片 N/M。默认只预览；real_semantic=true 才逐卡真实推理（仍需三重锁）。",
            "inputSchema": {"type":"object","properties":{
                "keys": {"type":"array","items":{"type":"string"},"description":"要检视的 key 列表；空或缺省=全库"},
                "slice": {"type":"string","description":"分片 N/M（如 1/4）"},
                "all_layers": {"type":"boolean","description":"关闭层过滤（默认 false）"},
                "real_semantic": {"type":"boolean","description":"真实逐卡推理（三重锁之命令行锁；默认 false）"}},
                "required":[]}
        },
        {
            "name": "wg_snapshot",
            "description": "world-gov 逻辑快照管理（world-gov diff snap 的 MCP 封装）。只存索引+元信息哈希，不复制正文；与基准库隔离。",
            "inputSchema": {"type":"object","properties":{
                "action": {"type":"string","description":"create | list | show | delete"},
                "label": {"type":"string","description":"create 时的标签（可空）"},
                "snap_id": {"type":"string","description":"show/delete 需要的快照 id"}},
                "required":["action"]}
        },
        {
            "name": "wg_conflict",
            "description": "world-gov 冲突记录库查询（world-gov conflict 的 MCP 封装）。存 AI 观点，人是终审；只读。",
            "inputSchema": {"type":"object","properties":{
                "action": {"type":"string","description":"list | show | summary"},
                "status": {"type":"string","description":"list 过滤：pending|valid|false_alarm|ignored"},
                "key": {"type":"string","description":"list 过滤：probe_key"},
                "id": {"type":"integer","description":"show 需要的记录 id"}},
                "required":["action"]}
        },
        {
            "name": "wg_sandbox",
            "description": "world-gov 沙盘推演（world-gov sandbox 的 MCP 封装）。确定性多步推演、可重放；与基准资料库强隔离（不覆盖基准库）。",
            "inputSchema": {"type":"object","properties":{
                "action": {"type":"string","description":"run | list | show | spawn | verify | trace"},
                "scenario": {"type":"object","description":"run：场景对象（与 quick 二选一）"},
                "quick": {"type":"string","description":"run：快捷预设 warn|critical|tip|calm"},
                "actor": {"type":"string","description":"run：主体卡 key（给出受影响集）"},
                "save": {"type":"boolean","description":"是否落库（默认 false=只算不写）"},
                "run_id": {"type":"integer","description":"show/verify/trace 需要的推演记录 id"},
                "from_snap": {"type":"string","description":"spawn：源逻辑快照 id"}},
                "required":["action"]}
        },
        {
            "name": "wg_llm_status",
            "description": "world-gov LLM 状态与缓存（world-gov llm 的 MCP 封装）。全部只读、不外呼：三重锁状态/调用统计/推理缓存/后端清单/配置安全快照。",
            "inputSchema": {"type":"object","properties":{
                "action": {"type":"string","description":"gate | stats | cache | backends | config"}},
                "required":["action"]}
        },
        {
            "name": "wg_self_check",
            "description": "world-gov 健康自检（world-gov self-check 的 MCP 封装）。检查工作根/库 schema/目录/管线；fix=true 仅创建缺失目录（不改表、不删任务）。",
            "inputSchema": {"type":"object","properties":{
                "fix": {"type":"boolean","description":"安全自动修复：仅创建缺失目录（默认 false）"}},
                "required":[]}
        }
    ]})
}

fn text_content(s: String) -> serde_json::Value {
    serde_json::json!({"content":[{"type":"text","text": s}]})
}

/// 工具**执行**错误（区别于协议错误）。
///
/// MCP 规范：tools/call 的执行期失败应作为正常 result 返回，并带 `isError: true`，
/// 这样模型能看见失败原因并自行调整；而"未知工具/参数非法"属于协议错误，
/// 走 JSON-RPC `error`（-32602）。两者不可混用。
fn error_content(s: String) -> serde_json::Value {
    serde_json::json!({
        "content": [{"type": "text", "text": s}],
        "isError": true
    })
}

fn to_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    }
}

fn clamp(s: String, limit: usize) -> String {
    if s.chars().count() <= limit {
        s
    } else {
        let cut: String = s.chars().take(limit).collect();
        format!("{cut}\n\n... [已截断，原始长度 {} 字符]", s.chars().count())
    }
}

/// 参数校验（对齐 inputSchema 的 `required`）。
///
/// 返回 `Some(message)` 表示非法参数 —— 由 `tools/call` 升级为 JSON-RPC
/// `-32602 Invalid params`（MCP 规范要求），而不是把错误塞进 `content` 文本里
/// （后者会让客户端以为"调用成功、结果是一段错误文字"）。
///
/// 背景：旧版只校验 volume/text/sql，**漏了 `ne_inject.chapter`**，
/// 缺参时静默取 0 → 底层报"切不到第0章"，错误归类不对。
fn validate_args(name: &str, args: &serde_json::Value) -> Option<String> {
    // 从 tool_specs 的 required 里取，保证 schema 与校验同源，避免再次漂移。
    let specs = tool_specs();
    let spec = specs["tools"]
        .as_array()?
        .iter()
        .find(|t| t["name"].as_str() == Some(name))?;
    let required = spec["inputSchema"]["required"].as_array().cloned().unwrap_or_default();

    for r in required {
        let Some(key) = r.as_str() else { continue };
        let v = args.get(key);
        let bad = match v {
            None | Some(serde_json::Value::Null) => true,
            Some(serde_json::Value::String(s)) => s.trim().is_empty(),
            _ => false,
        };
        if bad {
            return Some(format!("缺少必填参数 `{key}`"));
        }
        // 类型校验：schema 声明 integer/boolean 的，收到错误类型也要拒。
        // 例外：string 字段容忍数字——部分 MCP 客户端（如 Operit）会把纯数字串
        // 智能转成整数，这是客户端行为、非调用方本意，不应据此判参数非法。
        if let (Some(want), Some(got)) = (spec["inputSchema"]["properties"][key]["type"].as_str(), v) {
            let type_ok = match want {
                "integer" => got.is_i64() || got.is_u64(),
                "boolean" => got.is_boolean(),
                "string" => got.is_string() || got.is_i64() || got.is_u64(),
                _ => true,
            };
            if !type_ok {
                return Some(format!("参数 `{key}` 类型应为 {want}，实收 {got}"));
            }
        }
    }

    // 数值越界（schema 未表达，但底层会出错，提前拦）
    if let Some(ch) = args.get("chapter").and_then(|v| v.as_i64()) {
        if ch < 0 {
            return Some(format!("参数 `chapter` 不能为负（收到 {ch}）"));
        }
    }
    None
}

/// 读取 volume：兼容 `"01"` / `"卷01"` 字符串，以及被客户端**智能转成整数**的情形。
///
/// 背景：Operit 的 `MCPToolParameter.smartConvert` 没有 `"string"` 分支，会把
/// 纯数字串 `"01"` 落进"数字猜测"→ 变成整数 `1`。若不处理，ne_cardgen/ne_inject
/// 会因 `1` 不是字符串而拿不到卷号。这里把整数补零回两位，对齐既有「卷NN」口径。
fn norm_volume(v: Option<&serde_json::Value>) -> String {
    match v {
        Some(serde_json::Value::String(s)) => s.trim().to_string(),
        Some(serde_json::Value::Number(n)) => match n.as_i64() {
            Some(i) if (0..100).contains(&i) => format!("{i:02}"),
            Some(i) => i.to_string(),
            None => n.to_string(),
        },
        _ => String::new(),
    }
}

fn tool_call(cfg: &Cfg, name: &str, args: &serde_json::Value) -> serde_json::Value {
    let s = |k: &str| match args.get(k) {
        Some(serde_json::Value::String(v)) => v.clone(),
        // 客户端可能把纯数字串转成数字，回退成字符串，避免被当成缺参。
        Some(serde_json::Value::Number(n)) => n.to_string(),
        _ => String::new(),
    };
    let i = |k: &str| args.get(k).and_then(|v| v.as_i64()).unwrap_or(0) as u32;
    let b = |k: &str| args.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
    let vol = norm_volume(args.get("volume"));

    let r: anyhow::Result<serde_json::Value> = match name {
        "funnel" => tools::funnel(cfg),
        "ne_cardgen" => tools::cardgen(cfg, &vol),
        "ne_inject" => tools::inject(cfg, &vol, i("chapter"), b("allow_missing")),
        "ne_check" => {
            let vol = if vol.trim().is_empty() { "卷01".to_string() } else { vol };
            tools::check(cfg, &s("text"), i("chapter"), &vol)
        }
        "ne_db" => tools::db_status(cfg),
        "ne_materials" => tools::materials_audit(cfg, &vol),
        "ne_db_query" => {
            let lim = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
            tools::db_query(cfg, &s("sql"), lim.max(1))
        }
        // ── world-gov 能力（第 18 批 · MCP 可调用接口集；底层复用 ne-agent 成熟逻辑）──
        "wg_scan_library" => wg_tools::scan_library(
            cfg,
            args.get("slice").and_then(|v| v.as_str()),
            b("semantic"), b("dry_run"), b("all_layers"),
            args.get("since").and_then(|v| v.as_str()),
        ),
        "wg_inspect_card" => wg_tools::inspect_card(cfg, &s("key"), b("semantic"), b("real_semantic"), b("dry_run")),
        "wg_inspect_batch" => {
            let keys: Option<Vec<String>> = args.get("keys").and_then(|v| v.as_array()).map(|a| {
                a.iter().filter_map(|x| x.as_str().map(String::from)).collect()
            });
            wg_tools::inspect_batch(
                cfg, keys.as_deref(),
                args.get("slice").and_then(|v| v.as_str()),
                b("all_layers"), b("real_semantic"),
            )
        }
        "wg_snapshot" => wg_tools::snapshot(
            cfg, &s("action"),
            args.get("label").and_then(|v| v.as_str()),
            args.get("snap_id").and_then(|v| v.as_str()),
        ),
        "wg_conflict" => wg_tools::conflict(
            cfg, &s("action"),
            args.get("status").and_then(|v| v.as_str()),
            args.get("key").and_then(|v| v.as_str()),
            args.get("id").and_then(|v| v.as_i64()),
        ),
        "wg_sandbox" => wg_tools::sandbox(
            cfg, &s("action"),
            args.get("quick").and_then(|v| v.as_str()),
            args.get("scenario"),
            args.get("actor").and_then(|v| v.as_str()),
            b("save"),
            args.get("run_id").and_then(|v| v.as_i64()),
            args.get("from_snap").and_then(|v| v.as_str()),
        ),
        "wg_llm_status" => wg_tools::llm_status(cfg, &s("action")),
        "wg_self_check" => wg_tools::self_check(cfg, b("fix")),
        other => return text_content(format!("未知工具：{other}")),
    };

    match r {
        Ok(v) => text_content(clamp(to_text(&v), 200_000)),
        Err(e) => error_content(format!("工具执行失败：{e}")),
    }
}

fn handle(cfg: &Cfg, req: &serde_json::Value) -> Option<serde_json::Value> {
    let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let id = req.get("id").cloned().unwrap_or(serde_json::Value::Null);

    match method {
        "initialize" => Some(jsonrpc::result(
            &id,
            serde_json::json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION},
            }),
        )),
        // 通知类无响应
        m if m.starts_with("notifications/") => None,
        "tools/list" => Some(jsonrpc::result(&id, tool_specs())),
        "tools/call" => {
            let params = req.get("params").cloned().unwrap_or(serde_json::json!({}));
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(serde_json::json!({}));
            // 未知工具 → -32602（不是"调用成功但内容是一段错误文字"）
            if !tool_specs()["tools"]
                .as_array()
                .map(|a| a.iter().any(|t| t["name"].as_str() == Some(name)))
                .unwrap_or(false)
            {
                return Some(jsonrpc::error(
                    &id,
                    -32602,
                    &format!("未知工具：{name}"),
                ));
            }
            // 参数校验 → -32602
            if let Some(msg) = validate_args(name, &args) {
                return Some(jsonrpc::error(&id, -32602, &msg));
            }
            let res = tool_call(cfg, name, &args);
            Some(jsonrpc::result(&id, res))
        }
        "ping" => Some(jsonrpc::result(&id, serde_json::json!({}))),
        "" => None,
        other => {
            if id.is_null() {
                None
            } else {
                Some(jsonrpc::error(&id, -32601, &format!("method not found: {other}")))
            }
        }
    }
}

fn arg_value(args: &[String], key: &str) -> Option<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == key {
            return it.next().cloned();
        }
        if let Some(rest) = a.strip_prefix(&format!("{key}=")) {
            return Some(rest.to_string());
        }
    }
    None
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let root = arg_value(&args, "--root")
        .or_else(|| std::env::var("NE_ROOT").ok())
        .unwrap_or_else(|| ".".to_string());

    let cfg = Cfg { root: std::path::PathBuf::from(&root) };
    eprintln!(
        "[ne-mcp] root={} lib={} db={}",
        cfg.root.display(),
        cfg.lib_root().display(),
        cfg.db_path().display()
    );

    if args.iter().any(|a| a == "--selftest") {
        let out = serde_json::json!({
            "root": cfg.root.display().to_string(),
            "lib_root": cfg.lib_root().display().to_string(),
            "content_dir": cfg.content_dir().display().to_string(),
            "db_path": cfg.db_path().display().to_string(),
            "db_exists": cfg.db_path().exists(),
            "funnel_exists": cfg.funnel_path().exists(),
            "tools": tool_specs()["tools"].as_array().map(|a| a.len()).unwrap_or(0),
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
        return;
    }

    if let Err(e) = jsonrpc::serve(|req| handle(&cfg, req)) {
        eprintln!("[ne-mcp] serve error: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod volume_tests {
    use super::norm_volume;
    use serde_json::json;

    #[test]
    fn volume_string_passthrough() {
        assert_eq!(norm_volume(Some(&json!("卷01"))), "卷01");
        assert_eq!(norm_volume(Some(&json!("01"))), "01");
        assert_eq!(norm_volume(Some(&json!(" 02 "))), "02");
    }

    #[test]
    fn volume_number_padded() {
        // Operit 的 smartConvert 会把 "01" 转成整数 1；应补零回 "01"。
        assert_eq!(norm_volume(Some(&json!(1))), "01");
        assert_eq!(norm_volume(Some(&json!(2))), "02");
        assert_eq!(norm_volume(Some(&json!(12))), "12");
        assert_eq!(norm_volume(Some(&json!(100))), "100");
    }

    #[test]
    fn volume_missing_is_empty() {
        assert_eq!(norm_volume(None), "");
        assert_eq!(norm_volume(Some(&serde_json::Value::Null)), "");
    }
}

#[cfg(test)]
mod wg_tool_tests {
    use super::{tool_specs, validate_args};
    use serde_json::json;

    /// 第 18 批：world-gov 8 个工具必须都在 tool_specs 里（MCP 可调用接口集）。
    #[test]
    fn wg_tools_registered() {
        let specs = tool_specs();
        let names: Vec<&str> = specs["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        for want in [
            "wg_scan_library", "wg_inspect_card", "wg_inspect_batch", "wg_snapshot",
            "wg_conflict", "wg_sandbox", "wg_llm_status", "wg_self_check",
        ] {
            assert!(names.contains(&want), "缺 MCP 工具：{want}（现有：{names:?}）");
        }
        // 原有 7 个只读/质检工具仍在（增量补齐，非替换）
        for keep in ["funnel", "ne_cardgen", "ne_inject", "ne_check", "ne_materials", "ne_db", "ne_db_query"] {
            assert!(names.contains(&keep), "原有工具被误删：{keep}");
        }
    }

    /// 每个 tool 的 inputSchema 必须是合法对象且 `required`（若有）是数组。
    #[test]
    fn wg_tool_schemas_wellformed() {
        for t in tool_specs()["tools"].as_array().unwrap() {
            let name = t["name"].as_str().unwrap();
            let schema = &t["inputSchema"];
            assert_eq!(schema["type"], "object", "{name} inputSchema.type 非 object");
            assert!(schema["properties"].is_object(), "{name} 缺 properties");
            if let Some(req) = schema.get("required") {
                assert!(req.is_array(), "{name} required 非数组");
            }
        }
    }

    /// 必填参数校验：缺 `key`/`action` 应被拒（对齐 inputSchema）。
    #[test]
    fn wg_required_args_enforced() {
        assert!(validate_args("wg_inspect_card", &json!({})).is_some(), "wg_inspect_card 缺 key 应被拒");
        assert!(validate_args("wg_inspect_card", &json!({"key": "CHAR-10-001"})).is_none());
        assert!(validate_args("wg_snapshot", &json!({})).is_some(), "wg_snapshot 缺 action 应被拒");
        assert!(validate_args("wg_llm_status", &json!({"action": "gate"})).is_none());
        // 无必填参数的探针类工具：空对象应放行
        assert!(validate_args("wg_scan_library", &json!({})).is_none());
        assert!(validate_args("wg_self_check", &json!({})).is_none());
    }

    /// wg_sandbox 的 action 集应覆盖规约要求的 spawn（--ai-sandbox 的 MCP 等价入口）。
    #[test]
    fn wg_sandbox_schema_lists_spawn() {
        let specs = tool_specs();
        let sandbox = specs["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "wg_sandbox")
            .expect("wg_sandbox 未注册");
        let desc = sandbox["inputSchema"]["properties"]["action"]["description"]
            .as_str()
            .unwrap_or("");
        for a in ["run", "list", "show", "spawn", "verify", "trace"] {
            assert!(desc.contains(a), "wg_sandbox action 描述缺 {a}：{desc}");
        }
    }
}
