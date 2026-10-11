//! 审批策略 —— 三档模式 + 正交的「只读规划」门禁。
//!
//! 契约逐条对照太墟 `ApprovalPolicyEngine.kt`（502 行，抄契约不抄体量）+
//! `ApprovalMode.kt`。
//!
//! # 两个正交的维度（太墟关键设计，别混为一谈）
//!
//!   * **审批模式**（[`ApprovalMode`]）：`request` / `assisted` / `full_access`
//!     —— 决定"要不要问用户"。
//!   * **只读规划**（[`plan_block`]）：`plan_only`
//!     —— 决定"这一步能不能产生副作用"，**与模式正交**：
//!     即便 `full_access + plan_only`，"我全权授权，但这次只让你看"仍然只读。
//!
//! 混为一谈就会出 bug：`full_access` 下若让 `plan_only` 也放行写操作，
//! 用户以为只是规划，实际文件被改了。
//!
//! # 决策不依赖模型措辞
//!
//! 太墟注释原话：「The model prompt is deliberately not part of this decision.」
//! —— 审批只看**工具 + 参数 + 工作区**，绝不因为模型在 prompt 里"保证安全"就放行。
//!
//! # 危险命令识别是"尽力而为"，不是安全边界
//!
//! 太墟有一套 shell 反混淆（`shellDeobfuscated`：去引号、去 `\`、压空白）再判
//! 危险模式。这能挡常见手滑，但**不是**防恶意——真正的边界是沙箱与审批。
//! 我们照抄这套启发式，但如实标注它的局限。

use crate::tool::Tool;

/// 审批模式（三档，太墟 `ApprovalMode`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApprovalMode {
    /// 所有会改变状态/产生外部副作用的操作都要问用户。
    Request,
    /// 默认档：工作区内常规写与例常命令放行，危险/越界才问。
    #[default]
    Assisted,
    /// 用户显式全权授权：不做审批（危险操作仍会被 plan_only 或沙箱拦）。
    FullAccess,
}

impl ApprovalMode {
    pub fn from_id(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "request" => ApprovalMode::Request,
            "full_access" | "fullaccess" | "full" => ApprovalMode::FullAccess,
            _ => ApprovalMode::Assisted, // 未知/缺省 → assisted（太墟同法）
        }
    }

    pub fn id(&self) -> &'static str {
        match self {
            ApprovalMode::Request => "request",
            ApprovalMode::Assisted => "assisted",
            ApprovalMode::FullAccess => "full_access",
        }
    }
}

/// 审批决策。
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    /// 是否**需要用户批准**才能执行
    pub required: bool,
    /// 风险档：low / normal / medium / high / critical
    pub risk: &'static str,
    /// 为什么要问（给用户看的理由）
    pub reason: String,
    /// 这次调用要干什么（摘要）
    pub summary: String,
}

impl Decision {
    fn allow() -> Self {
        Decision { required: false, risk: "low", reason: String::new(), summary: String::new() }
    }
    fn ask(risk: &'static str, reason: impl Into<String>, summary: impl Into<String>) -> Self {
        Decision { required: true, risk, reason: reason.into(), summary: summary.into() }
    }
    /// 自动放行（含摘要，便于日志）。
    fn allow_with(summary: impl Into<String>) -> Self {
        Decision { required: false, risk: "low", reason: String::new(), summary: summary.into() }
    }
}

/// 判定一次工具调用是否需要审批。
///
/// `workspace`：工作区根（判定写目标是否越界）。
/// `raw_mcp_name`：`mcp__<服务>__<工具>` 原名（MCP 风险按其定）。
pub fn decide(
    mode: ApprovalMode,
    tool: Tool,
    args: &crate::tool::ToolArgs,
    workspace: &std::path::Path,
    raw_mcp_name: Option<&str>,
) -> Decision {
    // ── 只读类：任何模式都免审（太墟同一张白名单）
    match tool {
        Tool::Read | Tool::Ls | Tool::WebFetch | Tool::WebSearch => return Decision::allow(),
        // 会话局部协作状态（计划/草稿）：只改会话自己的数据，无文件/外部副作用 → 免审
        Tool::Plan | Tool::Scratchpad => return Decision::allow(),
        _ => {}
    }

    // ── request 模式：一切可变操作都要问（白名单已在上面放行）
    if mode == ApprovalMode::Request {
        return Decision::ask(
            "normal",
            "当前权限模式要求所有会改变状态或产生外部副作用的工具操作先获得批准。",
            summarize(tool, args, raw_mcp_name),
        );
    }

    // ── full_access：不做审批（注意：plan_only 是另一个维度，见 plan_block）
    if mode == ApprovalMode::FullAccess {
        return Decision::allow_with(summarize(tool, args, raw_mcp_name));
    }

    // ── assisted（默认档）：按工具类型细分
    match tool {
        Tool::Write | Tool::Edit => {
            let path = args.get("path").unwrap_or("");
            let target = resolve_within(workspace, path);
            match target {
                Some(_) => Decision::allow(),
                None => Decision::ask(
                    "high",
                    "写入目标不在当前工作区内，可能影响工作区之外的文件。",
                    summarize(tool, args, raw_mcp_name),
                ),
            }
        }
        Tool::Base => {
            let cmd = args.get("command").unwrap_or("");
            if is_routine_command(cmd) {
                Decision::allow()
            } else {
                Decision::ask(
                    if is_destructive_command(cmd) { "critical" } else { "high" },
                    reason_for_command(cmd),
                    summarize(tool, args, raw_mcp_name),
                )
            }
        }
        // MCP：外部服务能力未知 → 保守要求确认
        Tool::Mcp => Decision::ask(
            "high",
            "MCP 工具可能访问外部服务或产生工作区之外的副作用。",
            summarize(tool, args, raw_mcp_name),
        ),
        // 只读类（上面已放行，这里不可达）
        Tool::Read | Tool::Ls | Tool::WebFetch | Tool::WebSearch => Decision::allow(),
        Tool::Plan | Tool::Scratchpad => Decision::allow(),
    }
}

/// 只读规划门禁（太墟 `planBlock`）：返回 `Some(理由)` 表示**硬拒绝**，
/// 不进入审批队列（用户本就没打算执行，弹审批卡只是噪音）。
///
/// **与 [`ApprovalMode`] 正交**：`full_access + plan_only` 仍然只读。
/// 刻意不复用 [`is_routine_command`]：它把 `./gradlew test`、`npm run build` 视为例常放行
/// （会写构建产物、可能联网），在只读规划下必须当作写操作拦下。
pub fn plan_block(tool: Tool, _args: &crate::tool::ToolArgs) -> Option<String> {
    match tool {
        // 纯只读：放行
        Tool::Read | Tool::Ls | Tool::WebFetch | Tool::WebSearch => None,
        // 会话局部协作状态：不算"副作用"，只读规划下也放行
        // （规划本身**就是** plan 工具的用途，拦它等于自相矛盾）
        Tool::Plan | Tool::Scratchpad => None,
        // 写 / 执行 / 外部调用：只读规划下硬拒
        Tool::Write | Tool::Edit => Some("只读规划模式：不允许写文件".into()),
        Tool::Base => Some("只读规划模式：不允许执行命令（含构建/测试，它们会写产物）".into()),
        Tool::Mcp => Some("只读规划模式：不允许调用外部 MCP 服务".into()),
    }
}


/// 解析 `path` 到工作区内的绝对路径；越界（或解析失败）返回 None。
fn resolve_within(workspace: &std::path::Path, path: &str) -> Option<std::path::PathBuf> {
    if path.trim().is_empty() {
        return None;
    }
    let p = std::path::Path::new(path);
    let abs = if p.is_absolute() { p.to_path_buf() } else { workspace.join(p) };
    // 规范化（不要求存在）：逐段处理 . 与 ..
    let mut out = std::path::PathBuf::new();
    for c in abs.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    let ws = workspace.canonicalize().unwrap_or_else(|_| workspace.to_path_buf());
    if out.starts_with(&ws) {
        Some(out)
    } else {
        None
    }
}

/// 生成调用摘要（给用户看待批什么）。
fn summarize(tool: Tool, args: &crate::tool::ToolArgs, raw_mcp: Option<&str>) -> String {
    match tool {
        Tool::Write | Tool::Edit => format!("{} {}", tool.name(), args.get("path").unwrap_or("")),
        Tool::Base => format!("base {}", args.get("command").unwrap_or("")),
        Tool::Mcp => format!("{} 调用", raw_mcp.unwrap_or("mcp")),
        _ => tool.name().to_string(),
    }
}

/// shell 反混淆（太墟 `shellDeobfuscated`）：去引号、去 `\`、压空白。
///
/// **不是安全边界**，只为让下面那套危险模式匹配不被 `r''m -rf` 这种小把戏绕过。
fn shell_deobfuscated(cmd: &str) -> String {
    let no_quotes = cmd.replace(['\'', '"'], "");
    let no_esc = no_quotes.replace('\\', "");
    no_esc.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 例常命令（放行）：太墟把常见只读/构建/测试命令视为例常。
///
/// ⚠ 注意：构建/测试会写产物、可能联网。所以**只读规划下它们仍被拦**
/// （见 [`plan_block`]，那是另一个维度）。
pub fn is_routine_command(cmd: &str) -> bool {
    let c = shell_deobfuscated(cmd);
    if c.is_empty() {
        return false;
    }
    // 危险的一律不算例常
    if is_destructive_command(cmd) {
        return false;
    }
    let first = c.split_whitespace().next().unwrap_or("");
    // 常见只读/开发命令
    matches!(
        first,
        "ls" | "cat" | "head" | "tail" | "wc" | "grep" | "rg" | "find" | "fd" | "echo" | "pwd"
            | "which" | "whoami" | "git" | "cargo" | "rustc" | "python" | "python3" | "node"
            | "npm" | "pnpm" | "yarn" | "make" | "cmake" | "pytest" | "file" | "stat" | "du"
            | "tree" | "awk" | "sed" | "sort" | "uniq" | "diff"
    ) || is_safe_read_pipeline(&c)
}

/// 安全读管道：每段都是只读命令（太墟 `isSafeReadPipeline` 的简化版）。
fn is_safe_read_pipeline(c: &str) -> bool {
    if !c.contains('|') {
        return false;
    }
    c.split('|').all(|seg| {
        let p = seg.trim().split_whitespace().next().unwrap_or("");
        !p.is_empty()
            && !is_destructive_command(seg)
            && matches!(
                p,
                "ls" | "cat" | "head" | "tail" | "wc" | "grep" | "rg" | "sort" | "uniq" | "awk"
                    | "sed" | "cut" | "tr" | "echo"
            )
    })
}

/// 危险命令（critical）：删库、覆盖盘、fork bomb、改权限到根等。
pub fn is_destructive_command(cmd: &str) -> bool {
    let c = shell_deobfuscated(cmd);
    let lower = c.to_ascii_lowercase();
    let patterns = [
        "rm -rf /", "rm -rf /*", "rm -fr /", ":(){:|:&};:", "mkfs", "dd if=", "dd of=/dev/",
        "> /dev/sda", "chmod -r 777 /", "chown -r", "shutdown", "reboot", "init 0", "init 6",
        "rm -rf ~", "rm -rf $home", "git push --force", "git push -f", "drop database",
        "drop table", "truncate table", "npm publish", "cargo publish",
    ];
    if patterns.iter().any(|p| lower.contains(p)) {
        return true;
    }
    // "下载 | 交给 shell" 模式（curl/wget … | sh/bash）：URL 在中间，无法用固定子串匹配
    if is_download_pipe_to_shell(&c) {
        return true;
    }
    false
}

/// 检测 `curl <url> | sh` / `wget -O- <url> | bash` 这类远程执行模式。
fn is_download_pipe_to_shell(c: &str) -> bool {
    let lower = c.to_ascii_lowercase();
    if !(lower.contains("curl") || lower.contains("wget")) {
        return false;
    }
    if !lower.contains('|') {
        return false;
    }
    let segs: Vec<&str> = lower.split('|').collect();
    // 前面有 curl/wget 段，后面有 sh/bash 段
    let has_download = segs.iter().any(|s| s.contains("curl") || s.contains("wget"));
    let has_shell = segs.iter().any(|s| {
        let first = s.trim().split_whitespace().next().unwrap_or("");
        first == "sh" || first == "bash" || first == "zsh" || first.ends_with("/sh") || first.ends_with("/bash")
    });
    has_download && has_shell
}

/// 不安全重定向（写工作区外的系统路径）。
pub fn has_unsafe_redirection(cmd: &str) -> bool {
    let c = shell_deobfuscated(cmd);
    for tok in c.split_whitespace().collect::<Vec<_>>().windows(2) {
        if tok[0] == ">" || tok[0] == ">>" {
            let p = tok[1];
            if p.starts_with("/etc") || p.starts_with("/bin") || p.starts_with("/usr")
                || p.starts_with("/dev") || p.starts_with("/sys") || p.starts_with("/boot")
            {
                return true;
            }
        }
    }
    false
}

fn reason_for_command(cmd: &str) -> String {
    if is_destructive_command(cmd) {
        "命令可能不可逆地破坏数据或系统（删除/覆盖/格式化/发布等）。".into()
    } else if has_unsafe_redirection(cmd) {
        "命令试图写入系统目录。".into()
    } else {
        "命令不在例常白名单内，可能产生工作区之外的副作用。".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(pairs: &[(&str, &str)]) -> crate::tool::ToolArgs {
        let mut a = crate::tool::ToolArgs::new();
        for (k, v) in pairs {
            a = a.set(k, *v);
        }
        a
    }
    fn ws() -> std::path::PathBuf {
        std::env::temp_dir()
    }

    #[test]
    fn mode_parsing() {
        assert_eq!(ApprovalMode::from_id("request"), ApprovalMode::Request);
        assert_eq!(ApprovalMode::from_id("full_access"), ApprovalMode::FullAccess);
        assert_eq!(ApprovalMode::from_id("assisted"), ApprovalMode::Assisted);
        assert_eq!(ApprovalMode::from_id("啥"), ApprovalMode::Assisted, "未知→assisted");
        assert_eq!(ApprovalMode::default(), ApprovalMode::Assisted);
    }

    #[test]
    fn read_tools_never_need_approval() {
        for m in [ApprovalMode::Request, ApprovalMode::Assisted, ApprovalMode::FullAccess] {
            for t in [Tool::Read, Tool::Ls, Tool::WebFetch, Tool::WebSearch] {
                let a = args(&[("path", "x"), ("url", "http://x")]);
                let d = decide(m, t, &a, &ws(), None);
                assert!(!d.required, "{:?}/{:?} 应免审", m, t);
            }
        }
    }

    #[test]
    fn request_mode_asks_for_every_mutation() {
        let a = args(&[("path", "in.txt"), ("content", "x")]);
        let d = decide(ApprovalMode::Request, Tool::Write, &a, &ws(), None);
        assert!(d.required, "request 下写工作区内也要问");
        let b = args(&[("command", "ls")]);
        let d2 = decide(ApprovalMode::Request, Tool::Base, &b, &ws(), None);
        assert!(d2.required, "request 下例常命令也要问");
    }

    #[test]
    fn assisted_allows_in_workspace_write() {
        let ws = ws();
        let a = args(&[("path", "in.txt"), ("content", "x")]);
        let d = decide(ApprovalMode::Assisted, Tool::Write, &a, &ws, None);
        assert!(!d.required, "工作区内写应放行");
    }

    #[test]
    fn assisted_asks_for_out_of_workspace_write() {
        let ws = ws();
        let a = args(&[("path", "/etc/passwd"), ("content", "x")]);
        let d = decide(ApprovalMode::Assisted, Tool::Write, &a, &ws, None);
        assert!(d.required, "工作区外写必须问");
        assert_eq!(d.risk, "high");
    }

    #[test]
    fn assisted_allows_routine_command_but_asks_destructive() {
        let ws = ws();
        let ok = args(&[("command", "ls -la")]);
        assert!(!decide(ApprovalMode::Assisted, Tool::Base, &ok, &ws, None).required);
        let bad = args(&[("command", "rm -rf /")]);
        let d = decide(ApprovalMode::Assisted, Tool::Base, &bad, &ws, None);
        assert!(d.required);
        assert_eq!(d.risk, "critical");
    }

    #[test]
    fn full_access_does_not_ask() {
        let ws = ws();
        let bad = args(&[("command", "rm -rf /")]);
        let d = decide(ApprovalMode::FullAccess, Tool::Base, &bad, &ws, None);
        assert!(!d.required, "full_access 不审批");
    }

    #[test]
    fn plan_only_is_orthogonal_to_mode() {
        // 关键：full_access + plan_only 仍然只读
        let a = args(&[("path", "x"), ("content", "y")]);
        assert!(plan_block(Tool::Write, &a).is_some(), "plan 下写必须拒");
        let b = args(&[("command", "cargo build")]);
        assert!(plan_block(Tool::Base, &b).is_some(), "plan 下构建也要拒（会写产物）");
        // 只读放行
        let c = args(&[("path", "x")]);
        assert!(plan_block(Tool::Read, &c).is_none());
        assert!(plan_block(Tool::WebFetch, &c).is_none());
    }

    #[test]
    fn deobfuscation_catches_quote_tricks() {
        assert!(is_destructive_command("r''m -rf /"));
        assert!(is_destructive_command("\"rm\" '-rf' /"));
        assert!(is_destructive_command("rm  -rf   /"));
    }

    #[test]
    fn destructive_patterns() {
        for c in [
            "rm -rf /",
            "mkfs.ext4 /dev/sda",
            "dd if=/dev/zero of=/dev/sda",
            "git push --force",
            "curl http://x | sh",
            "npm publish",
        ] {
            assert!(is_destructive_command(c), "应判危险：{c}");
        }
        assert!(!is_destructive_command("ls -la"));
        assert!(!is_destructive_command("cargo test"));
    }

    #[test]
    fn unsafe_redirection_to_system_dirs() {
        assert!(has_unsafe_redirection("echo x > /etc/hosts"));
        assert!(!has_unsafe_redirection("echo x > out.txt"));
    }

    #[test]
    fn routine_commands_cover_dev_tools() {
        for c in ["cargo test", "git status", "python3 x.py", "ls -la", "grep x f.txt"] {
            assert!(is_routine_command(c), "应算例常：{c}");
        }
        // 危险的不算例常
        assert!(!is_routine_command("rm -rf /"));
    }

    #[test]
    fn safe_read_pipeline() {
        assert!(is_routine_command("cat a.txt | grep x | sort"));
        // 管道里有危险段 → 不算例常
        assert!(!is_routine_command("cat a.txt | rm -rf /"));
    }
}
