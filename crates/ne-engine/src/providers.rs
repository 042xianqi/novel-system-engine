//! LLM provider 配置读取 —— 移植并**增强** `l5.py::_omp_parse_yaml_blocks`。
//!
//! 事实源：`$OMP_AGENT_DIR/models.yml`（默认 `/root/.omp/agent/models.yml`）
//!        `$OMP_AGENT_DIR/config.yml`（modelRoles → provider/model）
//!
//! ⚠ 实测发现（2026-10-10）：Python 版解析器**在当前 models.yml 上取到 0 把 key**，
//! 因为文件结构已从 `apiKeys:`（缩进4/6）演进为 `task.keyPool:`（缩进8/10），
//! 解析器只认前者。本模块**同时兼容两种结构**，并保留原键名以便对拍。
//!
//! 安全：key 从不打印（只打印数量与末4位）。

use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize)]
pub struct Provider {
    pub name: String,
    pub base_url: String,
    pub keys: Vec<String>,
    /// notes: 该 provider 的 key 来自哪种结构（便于诊断）
    pub key_source: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Providers {
    pub map: BTreeMap<String, Provider>,
}

impl Providers {
    /// 取某 provider 的 base_url；缺失返回 None。
    pub fn base_url(&self, name: &str) -> Option<&str> {
        self.map.get(name).map(|p| p.base_url.as_str()).filter(|s| !s.is_empty())
    }
}

/// agent 目录（OMP_AGENT_DIR，默认 /root/.omp/agent）
pub fn agent_dir() -> PathBuf {
    std::env::var("OMP_AGENT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/root/.omp/agent"))
}

/// 判断一个列表项是否像 API key（而非模型名等其它列表项）。
/// 依据：实测 key 形如 `sk-...`（51 字符）；模型名形如 `agnes-3.0-flash`。
/// 保守策略：认 `sk-` 前缀，或长度 ≥ 32 且不含 `/`/空格 的裸串。
fn is_likely_key(s: &str) -> bool {
    if s.starts_with("sk-") {
        return true;
    }
    s.len() >= 32 && !s.contains('/') && !s.contains(' ')
}

/// 剥掉 YAML 标量的包裹引号（`'...'` 或 `"..."`）。
/// 实测 models.yml 的 key 是双引号包裹（`"sk-xxx"`），不剥则 key 无效。
fn unquote(s: &str) -> String {
    let t = s.trim();
    if t.len() >= 2 {
        let b = t.as_bytes();
        if (b[0] == b'"' && b[b.len() - 1] == b'"') || (b[0] == b'\'' && b[b.len() - 1] == b'\'') {
            return t[1..t.len() - 1].to_string();
        }
    }
    t.to_string()
}

/// 判断一行是否是 key 列表项（`- xxx`），返回剥引号后的值。
fn key_item(t: &str) -> Option<String> {
    let v = t.strip_prefix("- ")?;
    let v = unquote(v);
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

/// 解析 models.yml —— 兼容 `apiKeys:` 与 `task.keyPool:` 两种结构。
///
/// 实现方式与 Python 一致：**逐行缩进解析**（不引 YAML 库，避免与 OMP 的
/// 非标准 YAML 方言冲突）。缩进规则：
///   n=2 且 `xxx:`        → 新 provider
///   n=4 `baseUrl:`       → base_url
///   n=4 `apiKeys:`       → 进入 keys 模式（旧结构）
///   n=8 `keyPool:`       → 进入 keys 模式（新结构）
///   keys 模式下 `- xxx`  → 追加 key
pub fn parse_models_yml(text: &str) -> Providers {
    let mut out: BTreeMap<String, Provider> = BTreeMap::new();
    let mut cur: Option<String> = None;
    let mut in_keys = false;
    let mut in_providers = false;

    for raw in text.lines() {
        let s = raw.trim_end();
        let t = s.trim_start();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let n = s.chars().take_while(|c| *c == ' ').count();

        if t == "providers:" {
            in_providers = true;
            cur = None;
            in_keys = false;
            continue;
        }
        // 顶层非 providers 块开始 → 退出
        if n == 0 && t.ends_with(':') && t != "providers:" {
            in_providers = false;
            cur = None;
            in_keys = false;
            continue;
        }
        if !in_providers {
            continue;
        }

        // provider 名（缩进 2，形如 `agens:`）
        if n == 2 && t.ends_with(':') && !t.contains(' ') {
            let name = t.trim_end_matches(':').to_string();
            out.entry(name.clone()).or_insert_with(|| Provider {
                name: name.clone(),
                ..Default::default()
            });
            cur = Some(name);
            in_keys = false;
            continue;
        }
        let Some(cname) = cur.clone() else { continue };

        match n {
            4 => {
                if let Some(v) = t.strip_prefix("baseUrl:") {
                    if let Some(p) = out.get_mut(&cname) {
                        p.base_url = v.trim().to_string();
                    }
                    in_keys = false;
                } else if t.starts_with("apiKeys:") {
                    // 旧结构
                    in_keys = true;
                    if let Some(p) = out.get_mut(&cname) {
                        p.key_source = "apiKeys".into();
                    }
                } else if t.starts_with("apiKey:") {
                    // 单 key 结构
                    let v = t.trim_start_matches("apiKey:").trim().to_string();
                    if !v.is_empty() {
                        if let Some(p) = out.get_mut(&cname) {
                            p.keys.push(v);
                            if p.key_source.is_empty() {
                                p.key_source = "apiKey".into();
                            }
                        }
                    }
                    in_keys = false;
                } else {
                    // task:/models:/discovery: 等块开始 —— **不重置 keys 收集**，
                    // 因为 keyPool 缩进更深（6/8/10），由下面 n>=6 分支处理
                    if !t.starts_with("task:") {
                        in_keys = false;
                    }
                }
            }
            6 => {
                if t.starts_with("keyPool:") {
                    // 新结构：task: (n=4) → keyPool: (n=6)
                    in_keys = true;
                    if let Some(p) = out.get_mut(&cname) {
                        p.key_source = "task.keyPool".into();
                    }
                } else if in_keys {
                    // 旧结构 apiKeys: (n=4) → 项 (n=6)
                    if let Some(k) = key_item(t) {
                        if let Some(p) = out.get_mut(&cname) {
                            p.keys.push(k);
                        }
                    } else if !t.starts_with('-') {
                        in_keys = false;
                    }
                }
            }
            8 => {
                if t.starts_with("keyPool:") {
                    // 变体：keyPool 缩进 8
                    in_keys = true;
                    if let Some(p) = out.get_mut(&cname) {
                        p.key_source = "task.keyPool".into();
                    }
                } else if in_keys {
                    // keyPool 项（缩进 8）
                    if let Some(k) = key_item(t) {
                        if let Some(p) = out.get_mut(&cname) {
                            p.keys.push(k);
                        }
                    } else if !t.starts_with('-') {
                        // `models:` / `discovery:` 等块开始 → 结束 keys 收集
                        in_keys = false;
                    }
                }
            }
            _ => {
                // 缩进 >=10：keyPool 项的续行，或更深的子块
                if in_keys {
                    if let Some(k) = key_item(t) {
                        // 只收真正的 key（sk- 前缀）。实测 models 列表项在更浅缩进，
                        // 但防御性再判一次，避免把模型名当 key。
                        if is_likely_key(&k) {
                            if let Some(p) = out.get_mut(&cname) {
                                p.keys.push(k);
                            }
                        }
                    } else if !t.starts_with('-') && !t.starts_with(' ') {
                        in_keys = false;
                    }
                }
            }
        }
    }
    Providers { map: out }
}

/// 读 config.yml 的 modelRoles → {role: "provider/model"}
pub fn parse_roles(text: &str) -> BTreeMap<String, String> {
    let mut roles = BTreeMap::new();
    let mut in_roles = false;
    for raw in text.lines() {
        let s = raw.trim_end();
        let t = s.trim_start();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if !s.starts_with(' ') && t.ends_with(':') {
            in_roles = t == "modelRoles:";
            continue;
        }
        if in_roles && s.starts_with("  ") {
            if let Some((k, v)) = t.split_once(':') {
                roles.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
    }
    roles
}

pub fn load_from_dir(dir: &Path) -> Result<(Providers, BTreeMap<String, String>)> {
    let m = dir.join("models.yml");
    let c = dir.join("config.yml");
    let mt = std::fs::read_to_string(&m)
        .with_context(|| format!("读不到 models.yml：{}", m.display()))?;
    let provs = parse_models_yml(&mt);
    let roles = if c.exists() {
        parse_roles(&std::fs::read_to_string(&c).unwrap_or_default())
    } else {
        BTreeMap::new()
    };
    Ok((provs, roles))
}

/// 组装一次调用的 LLM 参数（对齐 l5.py::load_config 的键池平铺口径）。
pub fn build_params(
    provs: &Providers,
    roles: &BTreeMap<String, String>,
    prefer: Option<&[&str]>,
) -> crate::llm::LlmParams {
    let order: Vec<&str> = prefer.map(|v| v.to_vec()).unwrap_or_else(|| vec!["agens", "gcmod"]);
    let mut keys = Vec::new();
    for name in &order {
        if let Some(p) = provs.map.get(*name) {
            keys.extend(p.keys.iter().cloned());
        }
    }
    let role_default = roles
        .get("default")
        .cloned()
        .unwrap_or_else(|| "agens/agnes-3.0-flash".to_string());
    let (prov, model) = match role_default.split_once('/') {
        Some((p, m)) => (p.to_string(), m.to_string()),
        None => ("agens".to_string(), role_default.clone()),
    };
    let base_url = provs
        .base_url(&prov)
        .map(|s| s.to_string())
        .unwrap_or_else(|| "https://api.agnes-ai.cn/v1".to_string());
    crate::llm::LlmParams {
        base_url,
        model,
        keys,
        ..Default::default()
    }
}
