//! ne-agent —— 《残响之河》Agent Runtime。
//!
//! 目标形态：**一个单二进制智能体**，拥有自己的
//!   * 项目隔离库（vault）
//!   * 模型配置（models.yml + config.yml + runtime.yaml，复用 ne-engine）
//!   * 对话（session 持久化 / 恢复）
//!   * 记忆（pinned 常驻 + recall 按需）
//!   * 工具（enum 总线，静态注册）
//!   * 任务（chat / novel / memory / vault / tool）
//!
//! 现有 ne-* crate 一律不改，只被本 crate 调用。
//!
//! 阶段进度见 `docs/ROADMAP-ne-agent.md`。当前：**M1（它自己）**。

pub mod api;
pub mod approval;
pub mod audit_material;
pub mod ask_user;
pub mod compaction;
pub mod checkpoint;
pub mod config;
pub mod context;
pub mod context_tools;
pub mod ctx_window;
pub mod dual_agent;
pub mod harness;
pub mod mcp;
pub mod memory;
pub mod governance;
pub mod memory_system;
pub mod novel_ctx;
pub mod output_retention;
pub mod prompt;
pub mod protocol;
pub mod rewind;
pub mod resources;
pub mod session;
pub mod subagent;
pub mod subagent_runner;
pub mod task;
pub mod task_state;
pub mod tool;
pub mod tool_schema;
pub mod vault;
pub mod web;
pub mod web_html;
pub mod web_search;

pub use config::RuntimeConfig;
pub use vault::Vault;

/// 版本号（取自 Cargo 包版本，与 workspace 统一版本管理）。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 当前开发阶段（随路线图里程碑更新，便于 `--version` 自证进度）。
pub const STAGE: &str = "M10.2 MCP 可达性";

/// 从 `runtime.yaml` 的 `tools.enabled` 解析出启用工具列表。
///
/// 未知名字**报错**而非忽略——避免"我明明配了却没生效"这种静默失败。
///
/// # 自动接线（M10）
///
/// 只要有**已启用的 MCP 服务**，就自动补上 [`tool::Tool::Capability`]（`use_capability`
/// 代理）——否则模型面对 MCP 服务却没有任何发现/调用入口（这正是原先的可达性缺口：
/// MCP 工具名是动态的、不进 provider 工具面，而 `use_capability` 又没被启用）。
pub fn resolve_enabled_tools(cfg: &RuntimeConfig) -> anyhow::Result<Vec<tool::Tool>> {
    let mut out = Vec::new();
    for name in &cfg.tools.enabled {
        match tool::Tool::from_name(name) {
            // 去重（`mcp`/`mcp__x__y` 都归 Tool::Mcp，避免重复）
            Some(t) if !out.contains(&t) => out.push(t),
            Some(_) => {}
            None => anyhow::bail!(
                "runtime.yaml 的 tools.enabled 里有未知工具 `{name}`；可用：{}（另有 mcp__<服务>__<工具>）",
                tool::Tool::all()
                    .iter()
                    .map(|t| t.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
    // 有启用的 MCP 服务 → 自动启用能力代理（模型据此 list/inspect/call）
    let has_mcp = cfg.mcp.servers.iter().any(|s| s.enabled);
    if has_mcp && !out.contains(&tool::Tool::Capability) {
        out.push(tool::Tool::Capability);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_auto_enabled_only_with_live_mcp_server() {
        // 无 MCP 服务：不给代理（避免平白多一个工具进工具面）
        let cfg = RuntimeConfig::default();
        let t = resolve_enabled_tools(&cfg).unwrap();
        assert!(!t.contains(&tool::Tool::Capability));

        // 有已启用 MCP 服务：自动补上代理
        let mut cfg = RuntimeConfig::default();
        cfg.mcp.servers.push(crate::config::McpServerCfg {
            name: "fake".into(),
            command: "python3".into(),
            args: vec![],
            cwd: None,
            enabled: true,
        });
        let t = resolve_enabled_tools(&cfg).unwrap();
        assert!(t.contains(&tool::Tool::Capability));

        // 显式关闭的服务不算数
        let mut cfg = RuntimeConfig::default();
        cfg.mcp.servers.push(crate::config::McpServerCfg {
            name: "off".into(),
            command: "python3".into(),
            args: vec![],
            cwd: None,
            enabled: false,
        });
        let t = resolve_enabled_tools(&cfg).unwrap();
        assert!(!t.contains(&tool::Tool::Capability));
    }

    #[test]
    fn dedup_keeps_single_capability() {
        let mut cfg = RuntimeConfig::default();
        cfg.tools.enabled.push("use_capability".into());
        cfg.mcp.servers.push(crate::config::McpServerCfg {
            name: "fake".into(),
            command: "python3".into(),
            args: vec![],
            cwd: None,
            enabled: true,
        });
        let t = resolve_enabled_tools(&cfg).unwrap();
        assert_eq!(
            t.iter().filter(|x| **x == tool::Tool::Capability).count(),
            1
        );
    }
}
