//! Vault —— 项目隔离库（对标太墟的 workspace / OMP 的 `.omp`）。
//!
//! 一个 vault 就是一个自包含的智能体工作区：
//!
//! ```text
//! <vault>/
//! ├── runtime.yaml    运行时参数（模型角色、预算、开关）
//! ├── models.yml      provider 池（**沿用 ne-engine 的格式**，不新发明）
//! ├── config.yml      modelRoles（沿用 ne-engine 的格式）
//! ├── agent.db       会话 / 记忆 / 任务状态（与小说 novel.db 分家）
//! ├── prompts/       可选：prompt override（缺省回退编译内置）
//! ├── memory/        记忆落盘（pinned / recall 索引）
//! └── workspace/     本项目的工作目录（小说正文、导出等）
//! ```
//!
//! 设计原则（学太墟）：
//!   * 目录不存在 → 自动初始化（第一次进入目录就能用）
//!   * 模型配置**复用现有格式**，避免两套 YAML 方言
//!   * vault 内的 `models.yml` 优先于全局 `/root/.omp/agent`

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// vault 目录下的标准子路径。
pub const RUNTIME_YAML: &str = "runtime.yaml";
pub const MODELS_YML: &str = "models.yml";
pub const CONFIG_YML: &str = "config.yml";
pub const AGENT_DB: &str = "agent.db";
pub const PROMPTS_DIR: &str = "prompts";
pub const MEMORY_DIR: &str = "memory";
pub const WORKSPACE_DIR: &str = "workspace";

#[derive(Debug, Clone)]
pub struct Vault {
    root: PathBuf,
}

impl Vault {
    /// 绑定一个已存在的 vault 目录（不创建）。
    pub fn open(root: impl Into<PathBuf>) -> Self {
        Vault { root: root.into() }
    }

    /// 打开；若目录不存在则创建并初始化全部标准文件。
    pub fn ensure(root: impl Into<PathBuf>) -> Result<Self> {
        let v = Vault::open(root);
        v.init()?;
        Ok(v)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    pub fn agent_db(&self) -> PathBuf {
        self.path(AGENT_DB)
    }

    pub fn workspace(&self) -> PathBuf {
        self.path(WORKSPACE_DIR)
    }

    /// 初始化：建目录 + 写缺省文件（已存在的不覆盖）。
    ///
    /// 返回本次**新建**的文件列表（便于 CLI 打印）。
    pub fn init(&self) -> Result<Vec<PathBuf>> {
        let mut created = Vec::new();
        for d in ["", PROMPTS_DIR, MEMORY_DIR, WORKSPACE_DIR] {
            let p = if d.is_empty() { self.root.clone() } else { self.path(d) };
            if !p.exists() {
                std::fs::create_dir_all(&p)
                    .with_context(|| format!("创建目录失败：{}", p.display()))?;
                if !d.is_empty() {
                    created.push(p);
                }
            }
        }

        // runtime.yaml：只写缺省，不覆盖用户改动
        let rt = self.path(RUNTIME_YAML);
        if !rt.exists() {
            std::fs::write(&rt, DEFAULT_RUNTIME_YAML)
                .with_context(|| format!("写 runtime.yaml 失败：{}", rt.display()))?;
            created.push(rt);
        }

        // models.yml / config.yml：**只建空壳**，真正内容由用户/导入填充。
        // 若全局已有配置，调用方可用 `import_global_llm_config()` 导入。
        let m = self.path(MODELS_YML);
        if !m.exists() {
            std::fs::write(&m, EMPTY_MODELS_YML)
                .with_context(|| format!("写 models.yml 失败：{}", m.display()))?;
            created.push(m);
        }
        let c = self.path(CONFIG_YML);
        if !c.exists() {
            std::fs::write(&c, EMPTY_CONFIG_YML)
                .with_context(|| format!("写 config.yml 失败：{}", c.display()))?;
            created.push(c);
        }

        Ok(created)
    }

    /// LLM 配置目录。优先级：
    ///   1. vault 内有**实质内容**的 models.yml
    ///   2. `$TAIXU_LLM_DIR`（显式指定）
    ///   3. 全局 OMP 配置（`/root/.omp/agent`）
    ///
    /// 注意：返回的是**目录**（ne-engine 的 API 按目录读）。
    /// ⚠ 空壳（`providers: {}`，vault init 的占位）不算配置，必须回退，
    ///   否则用户导入/环境变量都会被静默绕过。
    pub fn llm_dir(&self) -> PathBuf {
        let m = self.path(MODELS_YML);
        if m.is_file() && !is_placeholder_file(&m) {
            return self.root.clone();
        }
        // 平铺约定：vault 里的 models.yml 只在根目录。
        // 传 None 而非 Some(root)，避免 vault 的空壳 models.yml 被 resolve_llm_dir
        // 的「库根候选」档选中、从而挡住 TAIXU_LLM_DIR 与全局配置。
        ne_engine::providers::resolve_llm_dir(None)
    }

    /// 是否是一个已初始化的 vault（至少有 runtime.yaml + models.yml）。
    pub fn is_initialized(&self) -> bool {
        self.path(RUNTIME_YAML).is_file() && self.path(MODELS_YML).is_file()
    }
}

/// 判断文件是否是"空壳"（vault init 写的模板，无实质内容）。
///
/// 规则：去掉空行与 `#` 注释后，若所有有效行都是以 `{}` 结尾的空块
/// （如 `providers: {}` / `modelRoles: {}`），则视为占位空壳。
pub fn is_placeholder_file(p: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(p) else {
        return true; // 读不到 → 当作无配置
    };
    let meaningful: Vec<&str> = text
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    meaningful.is_empty() || meaningful.iter().all(|l| l.ends_with("{}"))
}

/// 缺省 runtime.yaml（含注释，用户按需改）。
pub const DEFAULT_RUNTIME_YAML: &str = r#"# ne-agent 运行时配置
# 模型角色：优先用 vault 内的 models.yml；缺失时回退全局 OMP 配置。
model:
  # 主生成模型角色名（对应 config.yml 的 modelRoles 键）
  role: default
  max_tokens: 4096
  temperature: 1.0
  enable_thinking: false

context:
  # 上下文窗口策略（M5 落地，先留参数）
  max_prompt_tokens: 32000
  reserve_tokens: 4096

tools:
  # 内置工具开关（M1 落地）
  enabled: [read, write, edit, base, process]

memory:
  # 记忆策略（M2 落地）
  recall_limit: 5
  recall_min_score: 0.0
"#;

/// 空 models.yml（结构注释，实际内容由用户填或导入全局配置）。
pub const EMPTY_MODELS_YML: &str = r#"# provider 池 —— 与 ne-engine 使用同一格式
# 结构（新）：providers.<名>.baseUrl + providers.<名>.task.keyPool
# 结构（旧）：providers.<名>.baseUrl + providers.<名>.apiKeys
# 导入全局配置：ne-agent vault import-llm
providers: {}
"#;

/// 空 config.yml。
pub const EMPTY_CONFIG_YML: &str = r#"# modelRoles：provider/model
modelRoles: {}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("ne-agent-vault-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn init_creates_full_layout() {
        let dir = tmp("init");
        let v = Vault::ensure(&dir).unwrap();
        assert!(v.is_initialized());
        assert!(v.path(RUNTIME_YAML).is_file());
        assert!(v.path(MODELS_YML).is_file());
        assert!(v.path(CONFIG_YML).is_file());
        assert!(v.path(PROMPTS_DIR).is_dir());
        assert!(v.path(MEMORY_DIR).is_dir());
        assert!(v.path(WORKSPACE_DIR).is_dir());
        assert!(!v.agent_db().exists(), "agent.db 应在首次使用时才创建");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn init_is_idempotent_and_preserves_edits() {
        let dir = tmp("idem");
        let v = Vault::ensure(&dir).unwrap();
        // 用户改了 runtime.yaml
        let user = "# 用户自定义\nmodel:\n  role: custom\n";
        std::fs::write(v.path(RUNTIME_YAML), user).unwrap();
        // 再次 init 不应覆盖
        let created = v.init().unwrap();
        assert!(created.iter().all(|p| p.file_name().unwrap() != RUNTIME_YAML));
        assert_eq!(std::fs::read_to_string(v.path(RUNTIME_YAML)).unwrap(), user);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn llm_dir_prefers_vault_when_filled() {
        let dir = tmp("llmdir");
        let v = Vault::ensure(&dir).unwrap();
        // 空壳态不算配置
        assert_ne!(v.llm_dir(), dir);
        // 填了真内容才用 vault
        std::fs::write(v.path(MODELS_YML), "providers:\n  x:\n    baseUrl: https://e.com\n").unwrap();
        assert_eq!(v.llm_dir(), dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn placeholder_models_yml_is_not_config() {
        // 回归：vault init 写的空壳 models.yml 不算配置，llm_dir 必须回退，
        // 否则 TAIXU_LLM_DIR / 全局配置会被静默绕过。
        let dir = tmp("placeholder");
        let v = Vault::ensure(&dir).unwrap();
        assert!(is_placeholder_file(&v.path(MODELS_YML)));
        // 真填内容后就不再是占位
        std::fs::write(v.path(MODELS_YML), "providers:\n  x:\n    baseUrl: https://e.com\n").unwrap();
        assert!(!is_placeholder_file(&v.path(MODELS_YML)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
