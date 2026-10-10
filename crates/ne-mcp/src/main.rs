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

fn tool_call(cfg: &Cfg, name: &str, args: &serde_json::Value) -> serde_json::Value {
    let s = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let i = |k: &str| args.get(k).and_then(|v| v.as_i64()).unwrap_or(0) as u32;
    let b = |k: &str| args.get(k).and_then(|v| v.as_bool()).unwrap_or(false);

    let r: anyhow::Result<serde_json::Value> = match name {
        "funnel" => tools::funnel(cfg),
        "ne_cardgen" => {
            let vol = s("volume");
            if vol.is_empty() {
                return text_content("错误：volume 不能为空".into());
            }
            tools::cardgen(cfg, &vol)
        }
        "ne_inject" => {
            let vol = s("volume");
            if vol.is_empty() {
                return text_content("错误：volume 不能为空".into());
            }
            tools::inject(cfg, &vol, i("chapter"), b("allow_missing"))
        }
        "ne_check" => {
            let text = s("text");
            if text.is_empty() {
                return text_content("错误：text 不能为空".into());
            }
            tools::check(&text, i("chapter"))
        }
        "ne_db" => tools::db_status(cfg),
        "ne_db_query" => {
            let sql = s("sql");
            if sql.is_empty() {
                return text_content("错误：sql 不能为空".into());
            }
            let lim = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
            tools::db_query(cfg, &sql, lim.max(1))
        }
        other => return text_content(format!("未知工具：{other}")),
    };

    match r {
        Ok(v) => text_content(clamp(to_text(&v), 200_000)),
        Err(e) => text_content(format!("工具执行失败：{e}")),
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
