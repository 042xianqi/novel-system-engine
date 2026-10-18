//! 极简 JSON-RPC 2.0 over stdio —— 零外部依赖，对齐太墟内置 MCP 脚本的帧格式。
//!
//! 帧格式：每行一个 JSON 对象（newline-delimited JSON），不做 Content-Length 包装。

use std::io::{self, BufRead, Write};

pub fn write_msg<W: Write>(w: &mut W, v: &serde_json::Value) -> io::Result<()> {
    let s = serde_json::to_string(v)?;
    w.write_all(s.as_bytes())?;
    w.write_all(b"\n")?;
    w.flush()
}

pub fn result(id: &serde_json::Value, value: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "result": value})
}

pub fn error(id: &serde_json::Value, code: i64, message: &str) -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// 主循环：逐行读 JSON-RPC 请求，交给 handler 处理。
/// 非 JSON 行（如误入的日志）静默跳过 —— 太墟的 McpStdioTransport 也是这个容错策略。
pub fn serve<F>(mut handler: F) -> io::Result<()>
where
    F: FnMut(&serde_json::Value) -> Option<serde_json::Value>,
{
    let stdin = io::stdin();
    let mut out = io::stdout();
    let mut buf = String::new();
    let mut lock = stdin.lock();
    loop {
        buf.clear();
        match lock.read_line(&mut buf) {
            Ok(0) => break,             // EOF
            Ok(_) => {}
            Err(e) => {
                eprintln!("[ne-mcp] stdin read error: {e}");
                break;
            }
        }
        let line = buf.trim();
        if line.is_empty() {
            continue;
        }
        let req: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,         // 非 JSON 行跳过
        };
        if let Some(resp) = handler(&req) {
            if let Err(e) = write_msg(&mut out, &resp) {
                eprintln!("[ne-mcp] stdout write error: {e}");
                break;
            }
        }
    }
    Ok(())
}
