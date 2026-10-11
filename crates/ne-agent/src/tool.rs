//! 工具总线 —— **enum 工具** + 结构化结果 + 路径沙箱。
//!
//! # 为什么是 enum 而不是 trait 注册表
//!
//! 见 docs/ref_taixu_harness_notes.md（太墟用 `enum HarnessTool` + `match` 分派）。
//! 理由：
//!   * 静态、可穷举 —— `match` 漏分支编译期即报错
//!   * 零动态分发、零运行时注册 —— 没有"这个工具从哪来的"这种问题
//!   * schema 与执行体写在一起，改一处不会漏另一处
//!
//! 代价是要改工具就得重编译。对**单二进制**而言这是优点不是缺点。
//!
//! # 结果结构化（不抛异常）
//!
//! 工具失败**不返回 `Err`**，而是 [`ToolResult { ok: false, output }`](ToolResult)。
//! 原因（学太墟/oh-my-pi）：失败信息要**回注给模型**让它自己纠错，
//! 而不是炸掉整个循环。`Err` 只保留给"框架级故障"（如会话库打不开）。

use anyhow::Result;
use std::path::{Component, Path, PathBuf};

/// 工具权限档（M6 会接审批策略；M1 只做标注 + exec 类拒绝写盘外）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// 只读，无副作用
    Read,
    /// 写工作区内文件
    Write,
    /// 执行外部命令
    Exec,
}

/// 结构化工具结果。**失败也走这里**，不进 `Err`。
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub ok: bool,
    pub output: String,
}

impl ToolResult {
    pub fn ok(output: impl Into<String>) -> Self {
        ToolResult { ok: true, output: output.into() }
    }
    pub fn err(output: impl Into<String>) -> Self {
        ToolResult { ok: false, output: output.into() }
    }
    /// 给模型看的文本（含成功/失败标记，便于模型识别）。
    pub fn render(&self) -> String {
        let tag = if self.ok { "OK" } else { "ERROR" };
        format!("[{tag}] {}", self.output)
    }
}

/// 内置工具集合。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    /// 读文件
    Read,
    /// 写文件（覆盖）
    Write,
    /// 精确文本替换（要求 old 唯一匹配）
    Edit,
    /// 列目录
    Ls,
    /// 执行 shell 命令（危险）
    Base,
}

impl Tool {
    pub fn name(&self) -> &'static str {
        match self {
            Tool::Read => "read",
            Tool::Write => "write",
            Tool::Edit => "edit",
            Tool::Ls => "ls",
            Tool::Base => "base",
        }
    }

    pub fn tier(&self) -> Tier {
        match self {
            Tool::Read | Tool::Ls => Tier::Read,
            Tool::Write | Tool::Edit => Tier::Write,
            Tool::Base => Tier::Exec,
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            Tool::Read => "读取文件内容（相对工作区路径）",
            Tool::Write => "写入/覆盖文件（相对工作区路径）",
            Tool::Edit => "在文件中做精确文本替换；old 必须唯一匹配",
            Tool::Ls => "列出目录内容",
            Tool::Base => "在工作区内执行 shell 命令",
        }
    }

    pub fn from_name(s: &str) -> Option<Tool> {
        Some(match s {
            "read" => Tool::Read,
            "write" => Tool::Write,
            "edit" => Tool::Edit,
            "ls" => Tool::Ls,
            "base" => Tool::Base,
            _ => return None,
        })
    }

    pub fn all() -> &'static [Tool] {
        &[Tool::Read, Tool::Write, Tool::Edit, Tool::Ls, Tool::Base]
    }

    /// 参数中本工具需要的键名（用于参数校验与提示）。
    pub fn params(&self) -> &'static [&'static str] {
        match self {
            Tool::Read => &["path"],
            Tool::Write => &["path", "content"],
            Tool::Edit => &["path", "old", "new"],
            Tool::Ls => &["path"],
            Tool::Base => &["command"],
        }
    }
}

/// 执行上下文：工具只能在工作区内活动。
pub struct ToolCtx {
    pub root: PathBuf,
}

impl ToolCtx {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        ToolCtx { root: root.into() }
    }

    fn root_canon(&self) -> Result<PathBuf> {
        std::fs::create_dir_all(&self.root)?;
        Ok(std::fs::canonicalize(&self.root)?)
    }

    /// 把用户给的相对路径解析成**工作区内**的绝对路径。
    ///
    /// 安全规则（沙箱的关键）：
    ///   * 拒绝绝对路径、拒绝 `..` 逃逸、拒绝符号链接指到区外
    ///   * 目标不必存在（写新文件），此时校验其**最近的已存在祖先**
    pub fn resolve(&self, rel: &str) -> Result<PathBuf, String> {
        if rel.trim().is_empty() {
            return Err("路径为空".into());
        }
        let p = Path::new(rel);
        if p.is_absolute() {
            return Err(format!("禁止绝对路径：{rel}"));
        }
        for c in p.components() {
            if matches!(c, Component::ParentDir) {
                return Err(format!("禁止 `..`（越出工作区）：{rel}"));
            }
        }
        let root = self.root_canon().map_err(|e| format!("工作区不可用：{e}"))?;
        let joined = root.join(p);

        // 已存在 → 直接规范化后判前缀
        if joined.exists() {
            let c = std::fs::canonicalize(&joined)
                .map_err(|e| format!("路径解析失败 {rel}：{e}"))?;
            return if c.starts_with(&root) {
                Ok(c)
            } else {
                Err(format!("越出工作区：{rel}"))
            };
        }

        // 不存在 → 校验最近的已存在祖先仍在区内
        let mut anc = joined.clone();
        while !anc.exists() {
            match anc.parent() {
                Some(par) => anc = par.to_path_buf(),
                None => return Err(format!("无法解析路径：{rel}")),
            }
        }
        let anc_c = std::fs::canonicalize(&anc).map_err(|e| format!("祖先解析失败：{e}"))?;
        if !anc_c.starts_with(&root) {
            return Err(format!("越出工作区：{rel}"));
        }
        Ok(joined)
    }
}

/// 执行一个工具。参数用 `&str` 取值（由 harness 从 JSON 取出后传入）。
///
/// 返回 `Ok(ToolResult)` 表示"工具跑了"（成功或失败都在 output 里）；
/// 只有框架级故障才返回 `Err`。
pub fn execute(ctx: &ToolCtx, tool: Tool, args: &ToolArgs) -> Result<ToolResult> {
    Ok(match tool {
        Tool::Read => read(ctx, args),
        Tool::Write => write(ctx, args),
        Tool::Edit => edit(ctx, args),
        Tool::Ls => ls(ctx, args),
        Tool::Base => base(ctx, args),
    })
}

/// 轻量参数袋（避免工具签名依赖具体 JSON 库）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolArgs {
    map: std::collections::BTreeMap<String, String>,
}

impl ToolArgs {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn set(mut self, k: &str, v: impl Into<String>) -> Self {
        self.map.insert(k.to_string(), v.into());
        self
    }
    pub fn get(&self, k: &str) -> Option<&str> {
        self.map.get(k).map(|s| s.as_str())
    }
    fn need(&self, k: &str) -> Result<&str, ToolResult> {
        self.get(k).filter(|s| !s.is_empty()).ok_or_else(|| {
            ToolResult::err(format!("缺少参数 `{k}`"))
        })
    }
}

fn read(ctx: &ToolCtx, a: &ToolArgs) -> ToolResult {
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

fn write(ctx: &ToolCtx, a: &ToolArgs) -> ToolResult {
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

fn edit(ctx: &ToolCtx, a: &ToolArgs) -> ToolResult {
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

fn ls(ctx: &ToolCtx, a: &ToolArgs) -> ToolResult {
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

fn base(ctx: &ToolCtx, a: &ToolArgs) -> ToolResult {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(tag: &str) -> (ToolCtx, PathBuf) {
        let d = std::env::temp_dir().join(format!("ne-agent-tool-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        (ToolCtx::new(&d), d)
    }

    #[test]
    fn write_then_read_roundtrip() {
        let (c, d) = ctx("rw");
        let r = execute(&c, Tool::Write, &ToolArgs::new().set("path", "a.txt").set("content", "你好\n第二行"))
            .unwrap();
        assert!(r.ok, "{}", r.output);
        let r = execute(&c, Tool::Read, &ToolArgs::new().set("path", "a.txt")).unwrap();
        assert!(r.ok);
        assert!(r.output.contains("第二行"));
        assert!(r.output.contains("1| "), "read 应带行号");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn edit_requires_unique_match() {
        let (c, d) = ctx("edit");
        execute(&c, Tool::Write, &ToolArgs::new().set("path", "b.txt").set("content", "x\nx\n")).unwrap();
        // 多处匹配 → 明确报错
        let r = execute(&c, Tool::Edit, &ToolArgs::new().set("path", "b.txt").set("old", "x").set("new", "y")).unwrap();
        assert!(!r.ok);
        assert!(r.output.contains("不唯一"), "{}", r.output);
        // 唯一匹配 → 成功
        let r = execute(&c, Tool::Edit, &ToolArgs::new().set("path", "b.txt").set("old", "x\nx").set("new", "z")).unwrap();
        assert!(r.ok, "{}", r.output);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn edit_missing_text_gives_actionable_error() {
        let (c, d) = ctx("editmiss");
        execute(&c, Tool::Write, &ToolArgs::new().set("path", "c.txt").set("content", "abc")).unwrap();
        let r = execute(&c, Tool::Edit, &ToolArgs::new().set("path", "c.txt").set("old", "不存在").set("new", "x")).unwrap();
        assert!(!r.ok);
        assert!(r.output.contains("read"), "错误信息应提示先 read：{}", r.output);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn sandbox_rejects_escape() {
        let (c, d) = ctx("sandbox");
        for bad in ["../outside", "a/../../b", "/etc/passwd"] {
            let r = execute(&c, Tool::Read, &ToolArgs::new().set("path", bad)).unwrap();
            assert!(!r.ok, "{bad} 应被拒绝");
        }
        assert!(c.resolve("../x").is_err());
        assert!(c.resolve("/tmp/x").is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ls_lists_and_marks_dirs() {
        let (c, d) = ctx("ls");
        execute(&c, Tool::Write, &ToolArgs::new().set("path", "sub/f.txt").set("content", "1")).unwrap();
        // 根目录：只应看到 sub/
        let r = execute(&c, Tool::Ls, &ToolArgs::new().set("path", ".")).unwrap();
        assert!(r.ok);
        assert!(r.output.contains("sub/"), "{}", r.output);
        assert!(!r.output.contains("f.txt"), "根目录不该直接列出子目录里的文件：{}", r.output);
        // 子目录：应看到 f.txt
        let r2 = execute(&c, Tool::Ls, &ToolArgs::new().set("path", "sub")).unwrap();
        assert!(r2.ok);
        assert!(r2.output.contains("f.txt"), "{}", r2.output);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn render_handles_cjk_without_corruption() {
        // 回归：早期实现按字节索引 push char，中文被打碎成乱码
        let (c, d) = ctx("cjk");
        execute(&c, Tool::Write, &ToolArgs::new().set("path", "z.txt").set("content", "第一章 残响之河")).unwrap();
        let r = execute(&c, Tool::Read, &ToolArgs::new().set("path", "z.txt")).unwrap();
        assert!(r.ok);
        assert!(r.output.contains("残响之河"), "中文不得乱码：{}", r.output);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_param_reports_clearly() {
        let (c, _d) = ctx("miss");
        let r = execute(&c, Tool::Read, &ToolArgs::new()).unwrap();
        assert!(!r.ok);
        assert!(r.output.contains("path"));
    }

    #[test]
    fn registry_names_are_stable() {
        assert_eq!(Tool::from_name("read"), Some(Tool::Read));
        assert_eq!(Tool::from_name("nope"), None);
        let names: Vec<&str> = Tool::all().iter().map(|t| t.name()).collect();
        assert_eq!(names, vec!["read", "write", "edit", "ls", "base"]);
    }
}
