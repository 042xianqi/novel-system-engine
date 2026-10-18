//! 运行时配置 —— `runtime.yaml` 的读写，以及对现有 `ne-engine::providers` 的封装。
//!
//! 设计原则（学太墟的"配置分层"）：
//!   * `runtime.yaml` 管**运行时参数**（角色、预算、工具开关、记忆策略）
//!   * `models.yml` + `config.yml` 管**模型配置**（沿用 ne-engine 格式，零迁移成本）
//!   * 本模块**不重新实现** YAML 解析语义，provider/role 解析直接委托 ne-engine
//!     （它已同时兼容 `apiKeys:` 与 `task.keyPool:` 两种结构，见 providers.rs 文档）

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// `runtime.yaml` 的结构。字段都有 `#[serde(default)]`，允许用户只写一部分。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RuntimeConfig {
    pub model: ModelSection,
    pub context: ContextSection,
    pub tools: ToolsSection,
    pub memory: MemorySection,
    pub mcp: McpSection,
    pub approval: ApprovalSection,
}

/// 审批策略（M6）。契约见 `crate::approval`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApprovalSection {
    /// 审批模式：request / assisted / full_access（未知值→assisted）
    pub mode: String,
    /// 只读规划模式：true 时禁止一切副作用工具（与 mode **正交**）
    pub plan_only: bool,
}

impl Default for ApprovalSection {
    fn default() -> Self {
        ApprovalSection { mode: "assisted".into(), plan_only: false }
    }
}

/// MCP 外部服务清单（太墟命名法：工具暴露为 `mcp__<name>__<tool>`）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpSection {
    pub servers: Vec<McpServerCfg>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerCfg {
    /// 服务名（用于 `mcp__<name>__<tool>` 前缀）
    pub name: String,
    /// 可执行文件
    pub command: String,
    /// 参数
    #[serde(default)]
    pub args: Vec<String>,
    /// 工作目录（可选）
    #[serde(default)]
    pub cwd: Option<String>,
    /// 是否启用（默认 true）
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelSection {
    /// 主生成模型角色（对应 config.yml 的 modelRoles 键）。
    pub role: String,
    pub max_tokens: u32,
    pub temperature: f64,
    pub enable_thinking: bool,
    /// 推理强度档位（OpenAI 兼容 `reasoning_effort`）。
    /// 空 = 不下发该字段（兼容旧行为）；可选 low / medium / high / xhigh / max 等。
    pub reasoning_effort: String,
    /// 是否用流式（SSE）请求模型。
    ///
    /// **必须为 true 才能拿到思考内容**：实测本机网关（WorkBuddy Native API / glm-5.3-flash）
    /// 仅在 `stream=true` 时下发 `reasoning_content`，非流式一律丢弃（`reasoning_tokens: 0`）。
    /// 默认 true —— 宿主 App 需要「思考过程」显示，且流式同时给出正文增量。
    pub stream: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContextSection {
    /// 上下文窗口上限（tokens）。
    pub max_prompt_tokens: u32,
    /// 为模型输出预留的 tokens（从窗口中扣除）。
    pub reserve_tokens: u32,
    /// 是否允许**自动**压缩（超阈值时把早期历史折叠成摘要）。
    ///
    /// 与"手动/命令压缩"正交：这里是策略开关，手动压缩不受它约束
    /// （对齐太墟：`compress` 工具只在用户明确要求时调用）。
    pub auto_compaction: bool,
    /// 自动压缩触发阈值：当投影估算 tokens 超过折叠线的此百分比时触发（1~100）。
    pub auto_threshold_percent: u32,
    /// 保留最近 N 条工具结果不被机械截断（M5）。
    pub keep_recent_tool_results: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToolsSection {
    /// 启用的内置工具名（M1 落地后生效）。
    pub enabled: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MemorySection {
    pub recall_limit: usize,
    pub recall_min_score: f64,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        RuntimeConfig {
            model: ModelSection::default(),
            context: ContextSection::default(),
            tools: ToolsSection::default(),
            memory: MemorySection::default(),
            mcp: McpSection::default(),
            approval: ApprovalSection::default(),
        }
    }
}

impl Default for ModelSection {
    fn default() -> Self {
        ModelSection {
            role: "default".into(),
            max_tokens: 4096,
            temperature: 1.0,
            enable_thinking: false,
            reasoning_effort: String::new(),
            stream: true,
        }
    }
}

impl Default for ContextSection {
    fn default() -> Self {
        ContextSection {
            max_prompt_tokens: 32000,
            reserve_tokens: 4096,
            auto_compaction: false,
            auto_threshold_percent: 90,
            keep_recent_tool_results: crate::context::KEEP_RECENT_TOOL_RESULTS,
        }
    }
}

impl Default for ToolsSection {
    fn default() -> Self {
        ToolsSection {
            enabled: vec![
                "read".into(),
                "write".into(),
                "edit".into(),
                "ls".into(),
                "base".into(),
                "plan".into(),
                "scratchpad".into(),
            ],
        }
    }
}

impl Default for MemorySection {
    fn default() -> Self {
        MemorySection { recall_limit: 5, recall_min_score: 0.0 }
    }
}

impl RuntimeConfig {
    /// 从 vault 根读 `runtime.yaml`；文件缺失时返回 `Default`（而非报错）。
    pub fn load(vault_root: &Path) -> Result<Self> {
        let p = vault_root.join("runtime.yaml");
        if !p.is_file() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&p)
            .with_context(|| format!("读 runtime.yaml 失败：{}", p.display()))?;
        Self::parse(&text)
            .with_context(|| format!("解析 runtime.yaml 失败：{}", p.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let cfg: RuntimeConfig = serde_yaml::from_str(text)?;
        Ok(cfg)
    }

    /// 组装成 ne-engine 的 `LlmParams`。
    ///
    /// * `llm_dir`：provider 配置所在目录（通常来自 `Vault::llm_dir()`）
    /// * 角色优先：先按 `self.model.role` 找 `prefer`，再回退 `config.yml` 的 `default`
    pub fn llm_params(&self, llm_dir: &Path) -> Result<ne_engine::llm::LlmParams> {
        let (provs, roles) = ne_engine::providers::load_from_dir(llm_dir)
            .with_context(|| format!("加载模型配置失败：{}", llm_dir.display()))?;
        if provs.map.values().all(|p| p.keys.is_empty()) {
            anyhow::bail!(
                "模型配置里没有任何 key：{}\n提示：ne-agent vault import-llm 可导入全局 OMP 配置",
                llm_dir.display()
            );
        }

        // 角色：runtime.yaml 的 role 优先；其值形如 `provider/model` 或纯 model
        let mut roles2 = roles.clone();
        if let Some(v) = roles.get(&self.model.role) {
            roles2.insert("default".into(), v.clone());
        }
        let mut p = ne_engine::providers::build_params(&provs, &roles2, None);
        p.max_tokens = self.model.max_tokens;
        p.temperature = self.model.temperature;
        p.enable_thinking = self.model.enable_thinking;
        // 推理强度档位：空串 = 不下发（与旧行为一致）。
        p.reasoning_effort = if self.model.reasoning_effort.trim().is_empty() {
            None
        } else {
            Some(self.model.reasoning_effort.trim().to_string())
        };
        // 流式：拿 `reasoning_content` 的唯一途径（实测非流式网关不给）。
        p.stream = self.model.stream;
        Ok(p)
    }

    /// 供 CLI 展示：列出 provider、key 数量、模型、角色（**不打印 key**）。
    pub fn describe_models(llm_dir: &Path) -> Result<ModelSummary> {
        let (provs, roles) = ne_engine::providers::load_from_dir(llm_dir)?;
        let providers: Vec<ProviderSummary> = provs
            .map
            .values()
            .map(|p| ProviderSummary {
                name: p.name.clone(),
                base_url: p.base_url.clone(),
                key_count: p.keys.len(),
                key_source: p.key_source.clone(),
                models: p.models.clone(),
            })
            .collect();
        Ok(ModelSummary { dir: llm_dir.to_path_buf(), providers, roles })
    }
}

#[derive(Debug, Clone)]
pub struct ModelSummary {
    pub dir: std::path::PathBuf,
    pub providers: Vec<ProviderSummary>,
    pub roles: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct ProviderSummary {
    pub name: String,
    pub base_url: String,
    pub key_count: usize,
    pub key_source: String,
    pub models: Vec<String>,
}

impl ModelSummary {
    /// 人类可读渲染（key 只报数量）。
    pub fn render(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("配置目录：{}\n", self.dir.display()));
        if self.providers.is_empty() {
            s.push_str("  （无 provider —— 请填 models.yml 或跑 `ne-agent vault import-llm`）\n");
        }
        for p in &self.providers {
            s.push_str(&format!(
                "  • {}  keys={}  ({})\n      base={}\n",
                p.name,
                p.key_count,
                if p.key_source.is_empty() { "-" } else { &p.key_source },
                if p.base_url.is_empty() { "(未设置)" } else { &p.base_url },
            ));
            if !p.models.is_empty() {
                s.push_str(&format!("      models={}\n", p.models.join(", ")));
            }
        }
        if !self.roles.is_empty() {
            s.push_str("  角色：\n");
            for (k, v) in &self.roles {
                s.push_str(&format!("    {k} -> {v}\n"));
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_sane() {
        let c = RuntimeConfig::default();
        assert_eq!(c.model.role, "default");
        assert_eq!(c.model.max_tokens, 4096);
        assert!(c.tools.enabled.contains(&"read".to_string()));
    }

    #[test]
    fn parses_partial_yaml() {
        // 用户只写一部分，其余走缺省
        let y = "model:\n  role: advisor\n  max_tokens: 8192\n";
        let c = RuntimeConfig::parse(y).unwrap();
        assert_eq!(c.model.role, "advisor");
        assert_eq!(c.model.max_tokens, 8192);
        assert_eq!(c.model.temperature, 1.0, "缺省值应保留");
        assert_eq!(c.context.max_prompt_tokens, 32000);
    }

    #[test]
    fn parses_vault_default_template() {
        let c = RuntimeConfig::parse(crate::vault::DEFAULT_RUNTIME_YAML).unwrap();
        assert_eq!(c.model.role, "default");
        assert_eq!(c.memory.recall_limit, 5);
    }

    #[test]
    fn rejects_unknown_field() {
        // deny_unknown_fields：写错字段名要报错，不能静默忽略
        let y = "model:\n  role: x\n  typo_field: 1\n";
        assert!(RuntimeConfig::parse(y).is_err());
    }

    #[test]
    fn default_tools_are_all_real() {
        // 回归：默认 runtime.yaml 里出现过 `process`，而实际工具叫 `base`，
        // 导致 chat 一启动就报"未知工具"。默认值必须都在 Tool 注册表里。
        let c = RuntimeConfig::default();
        let cfg = RuntimeConfig::parse(crate::vault::DEFAULT_RUNTIME_YAML).unwrap();
        for name in c.tools.enabled.iter().chain(cfg.tools.enabled.iter()) {
            assert!(
                crate::tool::Tool::from_name(name).is_some(),
                "默认 tools.enabled 含不存在的工具 `{name}`"
            );
        }
    }

    #[test]
    fn missing_file_returns_default() {
        let dir = std::env::temp_dir().join(format!("ne-agent-cfg-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let c = RuntimeConfig::load(&dir).unwrap();
        assert_eq!(c.model.role, "default");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
