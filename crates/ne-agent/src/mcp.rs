//! MCP 客户端 —— 连接外部 MCP 服务（stdio + newline-delimited JSON-RPC 2.0）。
//!
//! # 为什么是「一个通用客户端」而不是「把 ne-mcp 的工具写死进来」
//!
//! 抄自太墟（`HarnessMessage.kt:24` 的 `MCP` 工具 + `HarnessToolRoundRunner.kt:351`）：
//! 太墟**不给每个 MCP 工具造一个 enum 变体**，而是：
//!   * 工具名走 `mcp__<server>__<tool>` 命名空间（模型直接可调，无需先"选服务"）
//!   * `mcp` 本身只是历史回放别名，模型误调它会拿到真实清单自我纠正
//!
//! 所以 ne-agent 也只在 [`crate::tool::Tool`] 里加**一个** `Mcp` 变体，
//! 具体服务/工具从 `runtime.yaml` 的 `mcp.servers` 读。
//!
//! # 为什么 ne-mcp 不进这个通道
//!
//! `ne-mcp` 的 6 个工具（funnel/cardgen/inject/check/db/db_query）全是**本地小说库操作**，
//! 而 M3 的 `novel` 任务已在**进程内直接调用**同样的能力（`novel_ctx` + `budget` + `gates`）。
//! 把本地能力绕道 MCP 子进程是纯粹的额外开销。MCP 通道留给**真正的外部服务**
//! （如太墟的 `mcp_websearch`）。
//!
//! # 协议
//!
//! 与 `ne-mcp` 同构：stdin/stdout 上 **newline-delimited JSON-RPC 2.0**。
//! 握手：`initialize` →（通知 `notifications/initialized`）→ `tools/list` / `tools/call`。

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

/// 协议版本（与 ne-mcp 一致）。
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// 一个已启动的 MCP 服务连接。
pub struct McpConnection {
    name: String,
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    pub server_info: Value,
    pub tools: Vec<McpTool>,
}

#[derive(Debug, Clone)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

impl McpConnection {
    /// 启动服务并完成握手。
    pub fn connect(name: &str, command: &str, args: &[String], cwd: Option<&std::path::Path>) -> Result<Self> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(d) = cwd {
            cmd.current_dir(d);
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("启动 MCP 服务 `{name}` 失败（{command}）"))?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("拿不到 stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("拿不到 stdout"))?;
        let mut conn = McpConnection {
            name: name.to_string(),
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
            server_info: Value::Null,
            tools: Vec::new(),
        };
        conn.initialize()?;
        conn.list_tools()?;
        Ok(conn)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// 发送请求并读响应（跳过通知等无 id 的消息）。
    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let mut line = serde_json::to_string(&req)?;
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .with_context(|| format!("向 MCP `{}` 写请求失败", self.name))?;
        self.stdin.flush()?;

        // 响应可能有通知混入 → 循环直到拿到对应 id
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if Instant::now() > deadline {
                return Err(anyhow!("MCP `{}` 响应超时（{method}）", self.name));
            }
            let mut buf = String::new();
            let n = self
                .stdout
                .read_line(&mut buf)
                .with_context(|| format!("读 MCP `{}` 响应失败", self.name))?;
            if n == 0 {
                return Err(anyhow!("MCP `{}` 已关闭（{method}）", self.name));
            }
            let buf = buf.trim();
            if buf.is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(buf) else {
                continue; // 非 JSON 行（如服务启动横幅）忽略
            };
            if v.get("id").and_then(|i| i.as_u64()) != Some(id) {
                continue; // 通知或他人响应
            }
            if let Some(err) = v.get("error") {
                return Err(anyhow!("MCP `{}` 返回错误：{err}", self.name));
            }
            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let req = json!({"jsonrpc": "2.0", "method": method, "params": params});
        let mut line = serde_json::to_string(&req)?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.flush()?;
        Ok(())
    }

    fn initialize(&mut self) -> Result<()> {
        let r = self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "ne-agent", "version": env!("CARGO_PKG_VERSION")},
            }),
        )?;
        self.server_info = r.get("serverInfo").cloned().unwrap_or(Value::Null);
        self.notify("notifications/initialized", json!({}))?;
        Ok(())
    }

    fn list_tools(&mut self) -> Result<()> {
        let r = self.request("tools/list", json!({}))?;
        let arr = r.get("tools").and_then(|t| t.as_array()).cloned().unwrap_or_default();
        self.tools = arr
            .iter()
            .filter_map(|t| {
                Some(McpTool {
                    name: t.get("name")?.as_str()?.to_string(),
                    description: t
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("")
                        .to_string(),
                    input_schema: t.get("inputSchema").cloned().unwrap_or(json!({"type":"object"})),
                })
            })
            .collect();
        Ok(())
    }

    /// 调用一个工具。
    pub fn call_tool(&mut self, tool: &str, arguments: Value) -> Result<String> {
        let r = self.request(
            "tools/call",
            json!({"name": tool, "arguments": arguments}),
        )?;
        // MCP 标准：result.content 是 [{type:"text", text:"..."}]
        if let Some(content) = r.get("content").and_then(|c| c.as_array()) {
            let mut out = String::new();
            for c in content {
                if let Some(t) = c.get("text").and_then(|t| t.as_str()) {
                    out.push_str(t);
                    out.push('\n');
                }
            }
            if !out.trim().is_empty() {
                return Ok(out.trim_end().to_string());
            }
        }
        Ok(serde_json::to_string_pretty(&r)?)
    }

    /// 工具全名（太墟命名法）：`mcp__<server>__<tool>`。
    pub fn qualified_tool_names(&self) -> Vec<String> {
        self.tools
            .iter()
            .map(|t| format!("mcp__{}__{}", self.name, t.name))
            .collect()
    }
}

impl Drop for McpConnection {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// 从 `mcp__<server>__<tool>` 拆出 (server, tool)。
///
/// ⚠ 工具名本身可能含 `__`，所以**只切前两段**（太墟同法）。
pub fn split_qualified(name: &str) -> Option<(String, String)> {
    let rest = name.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server.to_string(), tool.to_string()))
}

/// MCP 服务注册表：惰性连接、按名缓存连接。
///
/// 由 `runtime.yaml` 的 `mcp.servers` 构造；**首次真正调用时才 spawn 服务**，
/// 避免每次 `ne-agent` 启动都拉起全部子进程。
pub struct McpRegistry {
    specs: Vec<crate::config::McpServerCfg>,
    live: std::collections::HashMap<String, McpConnection>,
}

impl McpRegistry {
    pub fn new(specs: Vec<crate::config::McpServerCfg>) -> Self {
        McpRegistry { specs, live: Default::default() }
    }

    /// 是否配置了任何（启用的）服务。
    pub fn is_empty(&self) -> bool {
        !self.specs.iter().any(|s| s.enabled)
    }

    pub fn server_names(&self) -> Vec<String> {
        self.specs
            .iter()
            .filter(|s| s.enabled)
            .map(|s| s.name.clone())
            .collect()
    }

    fn spec(&self, name: &str) -> Option<&crate::config::McpServerCfg> {
        self.specs.iter().find(|s| s.enabled && s.name == name)
    }

    /// 确保某服务的连接就绪（惰性 + 缓存）。
    fn ensure(&mut self, name: &str) -> Result<()> {
        if self.live.contains_key(name) {
            return Ok(());
        }
        let spec = self
            .spec(name)
            .ok_or_else(|| anyhow!("未配置（或已禁用）的 MCP 服务：`{name}`"))?;
        let cwd = spec.cwd.as_ref().map(std::path::PathBuf::from);
        let conn = McpConnection::connect(&spec.name, &spec.command, &spec.args, cwd.as_deref())
            .with_context(|| format!("连接 MCP 服务 `{name}` 失败"))?;
        self.live.insert(name.to_string(), conn);
        Ok(())
    }

    /// 调用某服务某工具。
    pub fn call(&mut self, server: &str, tool: &str, arguments: Value) -> Result<String> {
        self.ensure(server)?;
        let conn = self
            .live
            .get_mut(server)
            .ok_or_else(|| anyhow!("MCP 服务 `{server}` 未连接"))?;
        conn.call_tool(tool, arguments)
    }

    /// 全部服务的工具限定名清单（供 prompt 里告诉模型可用工具）。
    ///
    /// ⚠ 这个动作会**真的启动**所有服务（需要 tools/list）。为避免启动开销，
    /// 只在需要枚举时调用；失败的服务被跳过（不让一个坏服务拖垮整体）。
    pub fn discover_all(&mut self) -> Vec<(String, McpTool)> {
        let names: Vec<String> = self.server_names();
        let mut out = Vec::new();
        for n in names {
            if self.ensure(&n).is_err() {
                continue;
            }
            if let Some(conn) = self.live.get(&n) {
                for t in &conn.tools {
                    out.push((n.clone(), t.clone()));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_qualified_basic() {
        assert_eq!(
            split_qualified("mcp__websearch__web_search"),
            Some(("websearch".into(), "web_search".into()))
        );
    }

    #[test]
    fn split_qualified_keeps_inner_double_underscore() {
        // 工具名可含 __，只切前两段
        assert_eq!(
            split_qualified("mcp__srv__tool__with__underscores"),
            Some(("srv".into(), "tool__with__underscores".into()))
        );
    }

    #[test]
    fn split_qualified_rejects_malformed() {
        assert!(split_qualified("mcp__onlyserver").is_none());
        assert!(split_qualified("mcp____tool").is_none());
        assert!(split_qualified("plain_tool").is_none());
    }

    /// 用本地一个回显 MCP 服务验证握手 + tools/list + tools/call。
    ///
    /// 服务脚本在 tests/fake_mcp_server.py（零依赖 python）。
    #[test]
    fn connect_and_call_fake_server() {
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake_mcp_server.py");
        if !std::path::Path::new(script).exists() {
            return; // 脚本缺失则跳过（不制造假失败）
        }
        let mut c = match McpConnection::connect("fake", "python3", &[script.to_string()], None) {
            Ok(c) => c,
            Err(_) => return, // 无 python3 环境则跳过
        };
        assert_eq!(c.name(), "fake");
        assert!(!c.tools.is_empty(), "应列出工具");
        let names = c.qualified_tool_names();
        assert!(names.iter().any(|n| n == "mcp__fake__echo"), "{names:?}");

        let out = c.call_tool("echo", json!({"msg": "你好"})).unwrap();
        assert!(out.contains("你好"), "回显应含原文：{out}");
    }
}
