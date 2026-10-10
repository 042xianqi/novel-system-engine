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

use tools::Cfg;

const PROTOCOL_VERSION: &str = "2025-06-18";
const SERVER_NAME: &str = "taixu-novel-engine";
const SERVER_VERSION: &str = "0.1.0";

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
            "description": "对正文跑确定性质检（AI 味词/禁词/字数）→ verdict(pass/fix/reject) + findings + 路由。",
            "inputSchema": {"type":"object","properties":{
                "text": {"type":"string","description":"待质检正文"},
                "chapter": {"type":"integer","description":"章号，默认 0"}},
                "required":["text"]}
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
        // 类型校验：schema 声明 integer/boolean 的，收到错误类型也要拒
        if let (Some(want), Some(got)) = (spec["inputSchema"]["properties"][key]["type"].as_str(), v) {
            let type_ok = match want {
                "integer" => got.is_i64() || got.is_u64(),
                "boolean" => got.is_boolean(),
                "string" => got.is_string(),
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

fn tool_call(cfg: &Cfg, name: &str, args: &serde_json::Value) -> serde_json::Value {
    let s = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let i = |k: &str| args.get(k).and_then(|v| v.as_i64()).unwrap_or(0) as u32;
    let b = |k: &str| args.get(k).and_then(|v| v.as_bool()).unwrap_or(false);

    let r: anyhow::Result<serde_json::Value> = match name {
        "funnel" => tools::funnel(cfg),
        "ne_cardgen" => tools::cardgen(cfg, &s("volume")),
        "ne_inject" => tools::inject(cfg, &s("volume"), i("chapter"), b("allow_missing")),
        "ne_check" => tools::check(&s("text"), i("chapter")),
        "ne_db" => tools::db_status(cfg),
        "ne_db_query" => {
            let lim = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
            tools::db_query(cfg, &s("sql"), lim.max(1))
        }
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
