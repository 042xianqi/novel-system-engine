//! 文件系统与命令工具 —— `read` / `write` / `edit` / `ls` / `base`。
//!
//! 从 tool.rs 拆出（架构门禁：单文件 ≤ 700 行）。

use super::*;

pub(super) fn read(ctx: &ToolCtx, a: &ToolArgs) -> ToolResult {
    let rel = match a.need("path") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let p = match ctx.resolve(rel) {
        Ok(p) => p,
        Err(e) => return ToolResult::err(e),
    };
    if !p.is_file() {
        return ToolResult::err(format!("不是文件或不存在：{rel}"));
    }
    match std::fs::read_to_string(&p) {
        Ok(s) => {
            // 加行号，便于后续 edit 精确匹配
            let mut out = String::new();
            for (i, line) in s.lines().enumerate() {
                out.push_str(&format!("{:>5}| {}\n", i + 1, line));
            }
            ToolResult::ok(format!("{rel}（{} 行）\n{out}", s.lines().count()))
        }
        Err(e) => ToolResult::err(format!("读 {rel} 失败：{e}")),
    }
}

pub(super) fn write(ctx: &ToolCtx, a: &ToolArgs) -> ToolResult {
    let rel = match a.need("path") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let content = a.get("content").unwrap_or("");
    let p = match ctx.resolve(rel) {
        Ok(p) => p,
        Err(e) => return ToolResult::err(e),
    };
    if let Some(dir) = p.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            return ToolResult::err(format!("建目录失败 {rel}：{e}"));
        }
    }
    match std::fs::write(&p, content) {
        Ok(()) => ToolResult::ok(format!("已写入 {rel}（{} 字节）", content.len())),
        Err(e) => ToolResult::err(format!("写 {rel} 失败：{e}")),
    }
}

pub(super) fn edit(ctx: &ToolCtx, a: &ToolArgs) -> ToolResult {
    let rel = match a.need("path") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let old = match a.need("old") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let new = a.get("new").unwrap_or("");
    let p = match ctx.resolve(rel) {
        Ok(p) => p,
        Err(e) => return ToolResult::err(e),
    };
    let text = match std::fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) => return ToolResult::err(format!("读 {rel} 失败：{e}")),
    };
    let n = text.matches(old).count();
    match n {
        0 => ToolResult::err(format!(
            "{rel} 中未找到要替换的文本。请先 read 该文件确认内容后再 edit，不要盲目猜测。"
        )),
        1 => {
            let out = text.replacen(old, new, 1);
            match std::fs::write(&p, out) {
                Ok(()) => ToolResult::ok(format!("已修改 {rel}")),
                Err(e) => ToolResult::err(format!("写 {rel} 失败：{e}")),
            }
        }
        k => ToolResult::err(format!(
            "{rel} 中 `old` 匹配到 {k} 处，不唯一。请提供更长的上下文使其唯一。"
        )),
    }
}

pub(super) fn ls(ctx: &ToolCtx, a: &ToolArgs) -> ToolResult {
    let rel = a.get("path").unwrap_or(".");
    let p = match ctx.resolve(if rel.is_empty() { "." } else { rel }) {
        Ok(p) => p,
        Err(e) => return ToolResult::err(e),
    };
    if p.is_file() {
        return ToolResult::ok(format!("{rel} 是文件（{} 字节）", std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0)));
    }
    let rd = match std::fs::read_dir(&p) {
        Ok(r) => r,
        Err(e) => return ToolResult::err(format!("列目录失败 {rel}：{e}")),
    };
    let mut names = Vec::new();
    for e in rd.flatten() {
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        names.push(format!("{}{}", e.file_name().to_string_lossy(), if is_dir { "/" } else { "" }));
    }
    names.sort();
    ToolResult::ok(format!("{rel}/（{} 项）\n{}", names.len(), names.join("\n")))
}

pub(super) fn base(ctx: &ToolCtx, a: &ToolArgs) -> ToolResult {
    let cmd = match a.need("command") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let root = match ctx.root_canon() {
        Ok(r) => r,
        Err(e) => return ToolResult::err(format!("工作区不可用：{e}")),
    };
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(&root)
        .output();
    match out {
        Ok(o) => {
            let mut s = String::new();
            s.push_str(&String::from_utf8_lossy(&o.stdout));
            let err = String::from_utf8_lossy(&o.stderr);
            if !err.trim().is_empty() {
                s.push_str("\n[stderr]\n");
                s.push_str(&err);
            }
            let code = o.status.code().unwrap_or(-1);
            if o.status.success() {
                ToolResult::ok(if s.trim().is_empty() { format!("（无输出，退出码 {code}）") } else { s.trim_end().to_string() })
            } else {
                ToolResult::err(format!("退出码 {code}\n{}", s.trim_end()))
            }
        }
        Err(e) => ToolResult::err(format!("执行失败：{e}")),
    }
}
