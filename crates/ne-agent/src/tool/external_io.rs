//! 外部 IO 工具 —— `mcp` / `web_fetch` / `web_search`。
//!
//! 从 tool.rs 拆出（架构门禁：单文件 ≤ 700 行）。

use super::*;

/// `mcp__<server>__<tool>`：路由到外部 MCP 服务。
///
/// 限定名由 harness 放在 [`MCP_TOOL_KEY`] 里传入；其余键即该工具的 arguments。
pub(super) fn mcp_call(ctx: &ToolCtx, a: &ToolArgs) -> ToolResult {
    let Some(reg) = ctx.mcp.as_ref() else {
        return ToolResult::err("未配置任何 MCP 服务（runtime.yaml 的 mcp.servers）");
    };
    let Some(qualified) = a.get(MCP_TOOL_KEY) else {
        return ToolResult::err("内部错误：缺少 MCP 限定工具名");
    };
    let Some((server, tool)) = crate::mcp::split_qualified(qualified) else {
        return ToolResult::err(format!(
            "MCP 工具名格式错误：`{qualified}`；应为 `mcp__<服务>__<工具>`"
        ));
    };
    // 除内部键外，其余都是该工具的实参
    let arguments = a.to_json_except(MCP_TOOL_KEY);
    match reg.borrow_mut().call(&server, &tool, arguments) {
        Ok(out) => ToolResult::ok(out),
        Err(e) => ToolResult::err(format!("MCP 调用失败（{server}/{tool}）：{e}")),
    }
}

/// `web_fetch`：抓网页 → 纯文本。
pub(super) fn web_fetch(a: &ToolArgs) -> ToolResult {
    let url = match a.need("url") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let max_chars = a
        .get("max_chars")
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(8000)
        .clamp(200, 60_000);
    match crate::web::fetch_text(url, max_chars) {
        Ok(t) => ToolResult::ok(t),
        Err(e) => ToolResult::err(format!("抓取失败：{e}")),
    }
}

/// `web_search`：多引擎搜索（失败自动回退）。
pub(super) fn web_search(a: &ToolArgs) -> ToolResult {
    let query = match a.need("query") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let engine = a.get("engine").unwrap_or("baidu");
    let limit = a
        .get("limit")
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(5)
        .clamp(1, 15);
    match crate::web::search(query, engine, limit) {
        Ok(hits) if hits.is_empty() => {
            ToolResult::err(format!("「{query}」无搜索结果"))
        }
        Ok(hits) => {
            let mut s = format!("搜索「{query}」（引擎 {engine}，{} 条）\n", hits.len());
            for (i, h) in hits.iter().enumerate() {
                s.push_str(&format!("\n{}. {}\n   {}", i + 1, h.title, h.url));
                if !h.snippet.is_empty() {
                    s.push_str(&format!("\n   {}", h.snippet));
                }
            }
            ToolResult::ok(s)
        }
        Err(e) => ToolResult::err(format!("搜索失败：{e}")),
    }
}
