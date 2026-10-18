//! 对话中枢的工具桥 —— 让 `/api/chat` 能调 ne-mcp 的**只读**工具。
//!
//! # 权责边界（用户 2026-10-17 拍板，硬性）
//!
//! | 类别 | chat 能否直接调 | 理由 |
//! |---|---|---|
//! | **只读**（funnel / ne_materials / wg_inspect_card …） | ✅ 直接调，立刻返回真实数据 | 只是读，最坏结果是读错，不会写坏数据 |
//! | **写类**（ne_run_chapter / ne_cardgen_run / wg_snapshot …） | ❌ **绝不直接调** | 一旦聊天框抽风就可能乱发指令，违背「一枪不开」 |
//!
//! 写类意图走 `needs_confirmation: true` + `confirm{...}`，由前端渲染成
//! 「确认执行」卡片，**用户点了按钮**才走 `/api/tasks/{id}/dispatch`。
//!
//! # 为什么不重写 MCP 客户端
//!
//! `crate::mcp::McpConnection` 已有完整实现（366 行）：spawn + 握手 +
//! tools/list + tools/call + 60s 超时 + Drop 时 kill。本模块**只做分类与封装**，
//! 不重复造轮子——避免两套 stdio 解析逻辑各自演化出 bug。
//!
//! # 性能
//!
//! 一次 chat 请求 = spawn 一个 ne-mcp 子进程 + 一次 tools/call，实测
//! `funnel` 0.01s、`ne_materials` 0.19s。比让它常驻省心得多（常驻要处理
//! 子进程意外死亡、僵尸进程、stdio 缓冲死锁）。聊天的提问频率本来就不高。

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::mcp::McpConnection;

// ══════════════════════════════════════════ 工具分类（硬编码白名单）

/// **只读**工具：chat 可直接调用。
///
/// 判据不是"名字听起来安全"，而是**逐个核对过实现**：
/// 这 14 个工具只 `SELECT` / 读文件 / 算统计，不 INSERT/UPDATE/DELETE、不写磁盘、不出网。
const READONLY: &[&str] = &[
    "funnel",           // 漏斗进度（读库）
    "ne_materials",     // 资料卡稽核（读库+读盘）
    "ne_db",            // 只读 SQL（有 SELECT-only 校验）
    "ne_db_query",      // 只读查询
    "ne_inject",        // 注入包预览（只组装，不落盘）
    "ne_check",         // 门禁检查（只读校验）
    "ne_cardgen",       // 建卡工单扫描（只查重）
    "ne_cardgen_plan",  // 建卡计划（只读，不跑）
    "wg_scan_library",  // 扫描资料库
    "wg_inspect_card",  // 检视单卡
    "wg_inspect_batch", // 批量检视
    "wg_conflict",      // 冲突列表
    "wg_llm_status",    // LLM 网关状态
    "wg_self_check",    // 系统自检
];

/// **写类**工具：chat 绝不直接调用。
///
/// 这些会写磁盘 / 写 DB / 真跑 LLM，必须人点按钮。
const WRITE: &[&str] = &[
    "ne_run_chapter", // 写正文（双锁）
    "ne_cardgen_run", // 真跑建卡（双锁）
    "wg_snapshot",    // 写快照
    "wg_sandbox",     // 沙盘写入
];

/// 判定一个工具是否只读。
///
/// **白名单制**：不在 READONLY 里的一律当写类处理（保守优先）。
/// 将来 ne-mcp 加了新工具，若忘了归类，会被拒绝而不是被放行——
/// 这个方向的错误是安全的（宁可少答，不可写坏）。
pub fn is_readonly(tool: &str) -> bool {
    READONLY.contains(&tool)
}

/// 判定是否是已知写类工具（用于给出更准确的提示文案）。
pub fn is_write(tool: &str) -> bool {
    WRITE.contains(&tool)
}

// ══════════════════════════════════════════ 客户端

/// 一次性的 ne-mcp 调用器。
///
/// 生命周期：`ChatTools::connect()` 起进程 → 若干次 `call()` → Drop 时自动 kill。
/// **不做常驻连接池**（理由见模块文档「性能」一节）。
pub struct ChatTools {
    conn: McpConnection,
}

impl ChatTools {
    /// 连接本仓库的 ne-mcp。
    ///
    /// 位置策略与 `web_gateway::fe_dist_dir()` 同构：环境变量 → 编译期常量 → 猜测。
    pub fn connect(lib_root: &Path) -> Result<Self> {
        let bin = mcp_bin();
        if !bin.is_file() {
            return Err(anyhow!("找不到 ne-mcp：{}", bin.display()));
        }
        let args = vec![
            "--root".to_string(),
            lib_root.to_string_lossy().to_string(),
        ];
        let conn = McpConnection::connect("ne-novel", &bin.to_string_lossy(), &args, None)
            .map_err(|e| anyhow!("连接 ne-mcp 失败：{e}"))?;
        Ok(ChatTools { conn })
    }

    /// 调用一个**只读**工具。写类会被拒绝（返回 Err，绝不执行）。
    pub fn call(&mut self, tool: &str, args: Value) -> Result<String> {
        if !is_readonly(tool) {
            return Err(anyhow!(
                "工具 `{tool}` 不是只读工具，对话中枢不允许直接调用（需人工确认）"
            ));
        }
        self.conn
            .call_tool(tool, args)
            .map_err(|e| anyhow!("调用 `{tool}` 失败：{e}"))
    }

    /// 列出可用工具名（诊断用；失败返回空）。
    pub fn tool_names(&self) -> Vec<String> {
        self.conn.tools.iter().map(|t| t.name.clone()).collect()
    }

    /// 服务端信息（诊断用）。
    pub fn server_info(&self) -> Value {
        self.conn.server_info.clone()
    }
}

impl Drop for ChatTools {
    fn drop(&mut self) {
        // McpConnection 自己的 Drop 会 kill 子进程；这里无需额外动作。
        // 保留 impl（显式写出意图），避免将来有人误以为"没人清理"。
    }
}

/// 找 ne-mcp 可执行文件。
///
/// 顺序：
/// 1. `NOVEL_MCP_BIN` 环境变量（显式指定）
/// 2. 编译期常量 `CARGO_MANIFEST_DIR` 推出的 `target/release/ne-mcp`
/// 3. 当前可执行文件同目录
/// 4. 裸名 `ne-mcp`（交给 PATH）
fn mcp_bin() -> PathBuf {
    if let Ok(p) = std::env::var("NOVEL_MCP_BIN") {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return pb;
        }
    }
    // crates/ne-agent → ../../target/release/ne-mcp
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    if let Some(root) = manifest.parent().and_then(|p| p.parent()) {
        let p = root.join("target/release/ne-mcp");
        if p.is_file() {
            return p;
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            let p = d.join("ne-mcp");
            if p.is_file() {
                return p;
            }
        }
    }
    PathBuf::from("ne-mcp")
}

// ══════════════════════════════════════════ 结果整形

/// 把工具返回的 JSON 文本压成"人能读的摘要"。
///
/// 为什么不直接把 JSON 丢给用户看：
/// `funnel` 返回 40 行缩进 JSON，`ne_materials` 返回几千字——
/// 聊天框里贴一堆花括号等于什么都没说。
///
/// 策略：**优先识别已知结构**（funnel 的 layers / materials 的 volumes），
/// 生成本项目语境下的中文摘要；认不出就截断原文（不丢信息，只是不美化）。
pub fn summarize(tool: &str, raw: &str) -> String {
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        // 不是 JSON → 原文截断
        return truncate(raw, 800);
    };
    match tool {
        "funnel" => sum_funnel(&v),
        "ne_materials" => sum_materials(&v),
        "wg_llm_status" => sum_llm(&v),
        // 【2026-10-18 补】查单卡是高频操作，之前落到 `_ => sum_generic`，
        // 用户看到的是原始 key=value 转储（command = world-gov inspect-card /
        // mode = probe_pack / status = PROBE_READY …），像终端输出不像人话。
        "wg_inspect_card" => sum_inspect_card(&v),
        "wg_self_check" | "wg_conflict" => sum_generic(&v, 900),
        _ => sum_generic(&v, 800),
    }
}

/// 单卡检视的人话摘要。
///
/// # 实测的真实结构（2026-10-18 用 MCP stdio 打出来的）
///
/// ```json
/// {"command":"world-gov inspect-card","mode":"probe_pack","status":"PROBE_READY",
///  "probe_pack":{
///    "title":"CIVI-99-900｜12 正名文明权威总录（唯一权威版）",
///    "card_text":"# CIVI-99-900｜...（5661 字正文）",
///    "card_chars":5661,
///    "card_types":["CIVI"],
///    "card_codes":["CHAR-99-090", ...28 个],
///    "key":"CIVI-99-900",
///    "layer":"L4",
///    "source":"02_资料库/文明/CIVI-99-900.md",
///    "candidate_laws":[8 条上层法理],
///    "law_chain":[8 层],
///    "questions":[4 个校验问句],
///    "verdict_rule":"..."
///  }}
/// ```
///
/// 【为什么不能靠猜字段名】我第一版按 `name` / `content_chars` / `content`
/// 写，全是错的——真实字段是 `title` / `card_chars` / `card_text`。
/// 猜错的后果是摘要永远返回空，退回 sum_generic 转储，
/// 用户又看到一堆 `command = ...` `mode = ...`。
/// 所以这里严格按实测结构取，并在取不到时如实说明。
fn sum_inspect_card(v: &Value) -> String {
    let pp = v.get("probe_pack").unwrap_or(v);

    let s = |k: &str| -> String {
        pp.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
    };

    let title = s("title");
    let key = s("key");
    let layer = s("layer");
    let source = s("source");
    let card_text = s("card_text");
    let card_chars = pp.get("card_chars").and_then(|x| x.as_i64()).unwrap_or(0);

    // 查不到：status 不是 READY，或 title 为空
    let status = s_top(v, "status");
    if title.is_empty() {
        if !status.is_empty() && status != "PROBE_READY" {
            return format!("没找到这张卡（{status}）。可能卡号写错了，或它在废弃区。");
        }
        // 真拿不到就给个简短说明，绝不回退到 key=value 转储
        return format!("没查到「{key}」这张卡的记录。");
    }

    // 标题里通常带「编号｜真名」，拆开只留真名部分给用户看
    let display = match title.split_once('｜') {
        Some((_code, rest)) if !rest.trim().is_empty() => rest.trim().to_string(),
        _ => title.clone(),
    };

    let mut out = format!("**{display}**");
    let mut meta: Vec<String> = Vec::new();
    if !key.is_empty() {
        meta.push(key.clone());
    }
    if !layer.is_empty() {
        meta.push(format!("{layer} 层"));
    }
    if card_chars > 0 {
        meta.push(format!("{card_chars} 字"));
    }
    if !meta.is_empty() {
        out.push_str(&format!("\n{}", meta.join(" · ")));
    }

    // 正文开头（跳过 markdown 标题行与引用块，取第一段实质内容）
    if !card_text.trim().is_empty() {
        if let Some(brief) = first_prose(&card_text, 260) {
            out.push_str(&format!("\n\n{brief}…"));
        }
    }
    if !source.is_empty() {
        out.push_str(&format!("\n\n（文件：{source}）"));
    }
    out
}

/// 从顶层取字符串字段。
fn s_top(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

/// 从 markdown 正文里取第一段"像人话"的内容。
///
/// 跳过：`#` 标题行、`>` 引用块、空行、表格行、纯符号行。
/// 目的：用户问「这是什么卡」，要给他一句能读懂的描述，
/// 而不是从 `# CIVI-99-900｜12 正名文明权威总录` 开始念格式。
fn first_prose(text: &str, max: usize) -> Option<String> {
    let mut buf = String::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            // 已经攒了内容 → 一段结束
            if !buf.is_empty() {
                break;
            }
            continue;
        }
        // 跳过结构性行
        if line.starts_with('#')
            || line.starts_with('>')
            || line.starts_with('|')
            || line.starts_with("---")
            || line.starts_with("```")
            || line.starts_with("- ")
            || line.starts_with("* ")
        {
            if !buf.is_empty() {
                break;
            }
            continue;
        }
        buf.push_str(line);
        if buf.chars().count() >= max {
            break;
        }
    }
    if buf.is_empty() {
        return None;
    }
    let out: String = buf.chars().take(max).collect();
    Some(out)
}

fn sum_funnel(v: &Value) -> String {
    let mut out = String::from("漏斗进度：\n");
    if let Some(arr) = v.get("all").and_then(|a| a.as_array()) {
        for it in arr {
            let id = it.get("id").and_then(|x| x.as_str()).unwrap_or("?");
            let st = it.get("status").and_then(|x| x.as_str()).unwrap_or("?");
            let mark = match st {
                "done" => "✅",
                s if s.starts_with("chapters_") => "📖",
                _ => "⬜",
            };
            out.push_str(&format!("  {mark} {id} → {st}\n"));
        }
    }
    if let Some(n) = v.get("chapters_done").and_then(|x| x.as_u64()) {
        out.push_str(&format!("已完成章节：{n}\n"));
    }
    if out.trim() == "漏斗进度：" {
        return sum_generic(v, 700);
    }
    out.trim_end().to_string()
}

fn sum_materials(v: &Value) -> String {
    let mut out = String::new();
    if let Some(n) = v.get("total_missing").and_then(|x| x.as_i64()) {
        out.push_str(&format!("缺卡总数：{n}\n"));
    }
    if let Some(n) = v.get("db_disk_drift_count").and_then(|x| x.as_i64()) {
        out.push_str(&format!("库盘漂移：{n}\n"));
    }
    if let Some(vols) = v.get("volumes").and_then(|x| x.as_array()) {
        out.push_str(&format!("检查卷数：{}\n", vols.len()));
        for vol in vols.iter().take(5) {
            let name = vol.get("volume").and_then(|x| x.as_str()).unwrap_or("?");
            let miss = vol
                .get("missing")
                .and_then(|x| x.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            let exist = vol
                .get("existing")
                .and_then(|x| x.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            out.push_str(&format!("  {name}：有 {exist} / 缺 {miss}\n"));
        }
    }
    if out.trim().is_empty() {
        return sum_generic(v, 700);
    }
    out.trim_end().to_string()
}

fn sum_llm(v: &Value) -> String {
    let mut out = String::from("LLM 网关：\n");
    for k in ["endpoint", "model", "reachable", "status", "note"] {
        if let Some(x) = v.get(k) {
            let s = match x {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            out.push_str(&format!("  {k} = {s}\n"));
        }
    }
    if out.trim() == "LLM 网关：" {
        return sum_generic(v, 600);
    }
    out.trim_end().to_string()
}

/// 通用：取前若干个顶层字段，拼成 `k = v` 行；太深的结构只报类型。
fn sum_generic(v: &Value, budget: usize) -> String {
    match v {
        Value::Object(m) => {
            let mut out = String::new();
            for (k, val) in m.iter().take(14) {
                let s = match val {
                    Value::Array(a) => format!("[{} 项]", a.len()),
                    Value::Object(o) => format!("{{{} 个字段}}", o.len()),
                    Value::String(s) => truncate(s, 120),
                    other => other.to_string(),
                };
                out.push_str(&format!("  {k} = {s}\n"));
                if out.len() > budget {
                    out.push_str("  …（截断）\n");
                    break;
                }
            }
            out.trim_end().to_string()
        }
        Value::Array(a) => {
            let mut out = format!("共 {} 项：\n", a.len());
            for it in a.iter().take(8) {
                out.push_str(&format!("  · {}\n", truncate(&it.to_string(), 140)));
            }
            out.trim_end().to_string()
        }
        other => truncate(&other.to_string(), budget),
    }
}

/// 按字符（不是字节）截断，避免切坏 UTF-8。
fn truncate(s: &str, n: usize) -> String {
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i >= n {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

// ══════════════════════════════════════════ 单测

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readonly_whitelist() {
        assert!(is_readonly("funnel"));
        assert!(is_readonly("wg_self_check"));
        // 写类绝不能算只读
        assert!(!is_readonly("ne_run_chapter"));
        assert!(!is_readonly("ne_cardgen_run"));
        // 未知工具 → 保守判为不可调
        assert!(!is_readonly("some_future_tool"));
    }

    #[test]
    fn write_list_matches() {
        assert!(is_write("ne_run_chapter"));
        assert!(is_write("ne_cardgen_run"));
        assert!(!is_write("funnel"));
    }

    #[test]
    fn summarize_funnel() {
        let raw = r#"{"all":[{"id":"L0","status":"done"},{"id":"L5","status":"chapters_1_to_3"}]}"#;
        let s = summarize("funnel", raw);
        assert!(s.contains("L0"));
        assert!(s.contains("✅"));
        assert!(s.contains("📖"));
    }

    #[test]
    fn summarize_non_json() {
        let s = summarize("funnel", "这只是普通文本");
        assert!(s.contains("这只是普通文本"));
    }

    #[test]
    fn truncate_keeps_utf8() {
        let s = truncate("残响之河残响之河", 3);
        assert_eq!(s, "残响之…");
        // 不 panic、不产生非法 UTF-8
        assert!(s.chars().count() == 4);
    }

    #[test]
    fn generic_object_compacts() {
        let v: Value = serde_json::from_str(
            r#"{"a":[1,2,3],"b":{"x":1},"c":"hello"}"#,
        )
        .unwrap();
        let s = sum_generic(&v, 500);
        assert!(s.contains("[3 项]"));
        assert!(s.contains("{1 个字段}"));
        assert!(s.contains("hello"));
    }
}