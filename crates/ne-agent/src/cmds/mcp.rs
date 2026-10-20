//! MCP 与库索引运维子命令 —— 第二批平移（架构门禁：单文件 ≤ 700 行）。
//!
//! # 为什么这一批安全
//! 逐个函数扫描确认：二者的**唯一调用者是 `run()` 分发器**，不调用 main.rs 的
//! 任何私有助手，也不被 `run_world_gov` 使用。平移属纯机械操作。

use anyhow::Result;
use ne_agent::{RuntimeConfig, Vault};
use std::path::Path;

use crate::cli::{LibraryCtlCmd, McpCmd};

/// 管理外部 MCP 服务。
pub(crate) fn run_mcp(vault: &Vault, cmd: &McpCmd, json: bool) -> Result<()> {
    vault.init()?;
    let cfg = RuntimeConfig::load(vault.root())?;
    match cmd {
        McpCmd::List => {
            if cfg.mcp.servers.is_empty() {
                if json {
                    println!("[]");
                } else {
                    println!("（未配置 MCP 服务；在 runtime.yaml 的 mcp.servers 里添加）");
                }
                return Ok(());
            }
            if json {
                let v: Vec<serde_json::Value> = cfg
                    .mcp
                    .servers
                    .iter()
                    .map(|s| serde_json::json!({
                        "name": s.name, "command": s.command, "args": s.args, "enabled": s.enabled,
                    }))
                    .collect();
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                for s in &cfg.mcp.servers {
                    println!(
                        "  {:<14} {} {}  {}",
                        s.name,
                        s.command,
                        s.args.join(" "),
                        if s.enabled { "" } else { "（已禁用）" }
                    );
                }
            }
            Ok(())
        }
        McpCmd::Tools { name } => {
            let specs: Vec<ne_agent::config::McpServerCfg> = cfg
                .mcp
                .servers
                .iter()
                .filter(|s| s.enabled && name.as_ref().map(|n| &s.name == n).unwrap_or(true))
                .cloned()
                .collect();
            if specs.is_empty() {
                anyhow::bail!("没有匹配的（已启用）MCP 服务");
            }
            let mut reg = ne_agent::mcp::McpRegistry::new(specs);
            let tools = reg.discover_all();
            if json {
                let v: Vec<serde_json::Value> = tools
                    .iter()
                    .map(|(srv, t)| serde_json::json!({
                        "qualified": format!("mcp__{srv}__{}", t.name),
                        "server": srv, "name": t.name,
                        "description": t.description, "inputSchema": t.input_schema,
                    }))
                    .collect();
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else if tools.is_empty() {
                println!("（服务未提供任何工具，或全部连接失败）");
            } else {
                for (srv, t) in tools {
                    println!("  mcp__{srv}__{}  {}", t.name, t.description);
                }
            }
            Ok(())
        }
        McpCmd::Call { server, tool, args_json } => {
            let spec = cfg
                .mcp
                .servers
                .iter()
                .find(|s| s.name == *server && s.enabled)
                .ok_or_else(|| anyhow::anyhow!("未配置（或已禁用）的 MCP 服务：{server}"))?
                .clone();
            let args: serde_json::Value = serde_json::from_str(args_json)
                .map_err(|e| anyhow::anyhow!("args_json 不是合法 JSON：{e}"))?;
            let mut reg = ne_agent::mcp::McpRegistry::new(vec![spec]);
            let out = reg.call(server, tool, args)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({"result": out}))?);
            } else {
                println!("{out}");
            }
            Ok(())
        }
        McpCmd::ProbeAll { call_test } => {
            let specs: Vec<ne_agent::config::McpServerCfg> = cfg
                .mcp
                .servers
                .iter()
                .filter(|s| s.enabled)
                .cloned()
                .collect();
            if specs.is_empty() {
                if json {
                    println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                        "command": "mcp probe-all", "servers": [], "note": "未配置（或全部禁用）MCP 服务",
                    }))?);
                } else {
                    println!("（未配置 MCP 服务；在 runtime.yaml 的 mcp.servers 里添加）");
                }
                return Ok(());
            }
            let mut reports: Vec<serde_json::Value> = Vec::new();
            for spec in &specs {
                // 逐服务独立连接，一个失败不影响其它（可用性探测的语义）
                let mut reg = ne_agent::mcp::McpRegistry::new(vec![spec.clone()]);
                let tools = reg.discover_all();
                let server_tools: Vec<serde_json::Value> = tools
                    .iter()
                    .filter(|(srv, _)| srv == &spec.name)
                    .map(|(_, t)| serde_json::json!({"name": t.name, "description": t.description}))
                    .collect();
                let ok = !server_tools.is_empty();
                let mut calls: Vec<serde_json::Value> = Vec::new();
                if *call_test && ok {
                    for (_, t) in tools.iter().filter(|(srv, _)| srv == &spec.name) {
                        // 轻量探活：用空参数调用；执行期失败也算"可达"（关键在握手/往返通）
                        match reg.call(&spec.name, &t.name, serde_json::json!({})) {
                            Ok(_) => calls.push(serde_json::json!({"tool": t.name, "reachable": true})),
                            Err(e) => calls.push(serde_json::json!({"tool": t.name, "reachable": false, "error": e.to_string()})),
                        }
                    }
                }
                reports.push(serde_json::json!({
                    "server": spec.name, "command": spec.command, "args": spec.args,
                    "ok": ok, "tool_count": server_tools.len(), "tools": server_tools,
                    "call_test": if *call_test { serde_json::json!(calls) } else { serde_json::Value::Null },
                }));
            }
            let all_ok = reports.iter().all(|r| r["ok"] == true);
            if json {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                    "command": "mcp probe-all", "all_ok": all_ok,
                    "server_count": reports.len(), "servers": reports,
                }))?);
            } else {
                println!("== mcp probe-all（{} 个服务；{}）==", reports.len(),
                    if all_ok { "全部可达" } else { "存在不可达" });
                for r in &reports {
                    println!("  {} {} 工具 {} — {}",
                        if r["ok"] == true { "✅" } else { "❌" },
                        r["server"].as_str().unwrap_or(""),
                        r["tool_count"],
                        if r["ok"] == true { "握手 + 列工具成功" } else { "连接失败或无工具" });
                    if *call_test {
                        if let Some(cs) = r["call_test"].as_array() {
                            for c in cs {
                                println!("      {} {} {}", if c["reachable"] == true { "·" } else { "✗" },
                                    c["tool"].as_str().unwrap_or(""),
                                    c["error"].as_str().unwrap_or(""));
                            }
                        }
                    }
                }
            }
            Ok(())
        }
    }
}


pub(crate) fn run_library_ctl(lib_root: &Path, cmd: &LibraryCtlCmd, json: bool) -> Result<()> {
    let conn = ne_agent::governance::open_gov(lib_root)?;
    let out = match cmd {
        LibraryCtlCmd::Init => ne_agent::governance::library_init(&conn, lib_root)?,
        LibraryCtlCmd::Classify { out } => {
            let dir = out.clone().unwrap_or_else(|| lib_root.join("07_勘误工坊/export"));
            ne_agent::governance::library_classify(&conn, Some(&dir))?
        }
        LibraryCtlCmd::Audit { semantic: _ } => ne_agent::governance::library_audit(&conn, lib_root)?,
        LibraryCtlCmd::Export { out, layer } => {
            let dir = out.clone().unwrap_or_else(|| lib_root.join("07_勘误工坊/export"));
            ne_agent::governance::library_export_layer(&conn, lib_root, &dir, layer.as_deref())?
        }
        LibraryCtlCmd::Reindex { batch: _ } => ne_agent::governance::library_reindex(lib_root)?,
        LibraryCtlCmd::Stats => ne_agent::governance::library_stats(&conn)?,
    };
    if json { println!("{}", serde_json::to_string_pretty(&out)?); }
    else { println!("{}", serde_json::to_string_pretty(&out)?); }
    Ok(())
}

