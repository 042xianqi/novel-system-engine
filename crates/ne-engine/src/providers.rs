//! LLM provider 配置读取 —— 自包含实现（不依赖 OMP）。
//!
//! 配置查找顺序（**库根优先**，最后一档仅为兼容旧环境）：
//!   1. `$TAIXU_LLM_DIR`（显式指定目录）
//!   2. `<库根>/05_管线框架/data/`（小说系统自有的配置位置）
//!   3. `<库根>/_config/`
//!   4. `$TAIXU_LLM_MODELS_YML` / `$TAIXU_LLM_CONFIG_YML`（直接给文件）
//!   5. `$OMP_AGENT_DIR` 或 `/root/.omp/agent`（⚠ 旧环境兼容，Deprecated）
//!
//! 事实源文件名：`models.yml`（provider → baseUrl/keyPool/models）
//!               `config.yml`（modelRoles → provider/model）
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
    /// 该 provider 声明的模型 id 列表
    pub models: Vec<String>,
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

/// 把 `Providers` 序列化成 `models.yml` 文本（`parse_models_yml` 的逆运算）。
///
/// # 为什么必须自己拼字符串，而不是引 serde_yaml
///
/// 1. 解析侧本来就是**手写逐行缩进解析**（不引 YAML 库，见 `parse_models_yml` 注释）。
///    写入侧如果引 serde_yaml，两边格式假设会分叉——serde_yaml 会输出
///    `- "sk-xxx"`、把数字加引号、改键顺序，解析侧未必都能吃。
/// 2. 手写输出能让**格式完全可控**，人肉 diff 时好看。
/// 3. 零新依赖。
///
/// # 输出格式（刻意与现有文件一致）
///
/// ```yaml
/// # 注释...
/// providers:
///   <name>:
///     baseUrl: <url>
///     api: openai-completions
///     task:
///       keyPool:
///         - "<key>"
///     models:
///       - id: <model>
///         name: <model>
/// ```
///
/// ⚠ **缩进是硬契约**：`models:` 必须缩进 4（在 provider 下面），
/// 列表项缩进 6。历史上这里被写成缩进 0，导致 `parse_models_yml` 提前退出、
/// models 全丢（2026-10-17 实测到该损坏）。
pub fn to_models_yml(p: &Providers) -> String {
    let mut out = String::new();
    out.push_str("# 小说系统 · LLM provider 配置（自包含，不依赖 OMP）\n");
    out.push_str("# 事实源：本文件。查找顺序见 providers.rs 模块文档。\n");
    out.push_str("# ⚠ 由面板「设置中心」写入；缩进是解析契约，勿手改。\n");
    out.push_str("providers:\n");

    if p.map.is_empty() {
        // 空表也要产出合法结构，否则下次解析会报"读不到 provider"
        out.push_str("  {}\n");
        return out;
    }

    for (name, prov) in &p.map {
        let name = if name.is_empty() { "default" } else { name.as_str() };
        out.push_str(&format!("  {name}:\n"));

        // baseUrl：空值不写（解析侧缺省为空串，等价）
        if !prov.base_url.is_empty() {
            out.push_str(&format!("    baseUrl: {}\n", yaml_scalar(&prov.base_url)));
        }
        out.push_str("    api: openai-completions\n");

        // keyPool：至少写一个空串占位（保持与现有文件同形）
        out.push_str("    task:\n      keyPool:\n");
        if prov.keys.is_empty() {
            out.push_str("        - \"\"\n");
        } else {
            for k in &prov.keys {
                out.push_str(&format!("        - {}\n", yaml_scalar(k)));
            }
        }

        // models：**必须缩进 4**（在 provider 下），项缩进 6
        if !prov.models.is_empty() {
            out.push_str("    models:\n");
            for m in &prov.models {
                let m = m.trim();
                if m.is_empty() {
                    continue;
                }
                out.push_str(&format!("      - id: {}\n", yaml_scalar(m)));
                out.push_str(&format!("        name: {}\n", yaml_scalar(m)));
            }
        }
    }
    out
}

/// 给 YAML 标量做最小安全处理：
/// · 空串 → `""`
/// · 含可能引起解析歧义的字符（`#`、`:`、前后空格、引号）→ 双引号包裹并转义
/// · 其余原样（保持文件干净，不无脑加引号）
pub fn yaml_scalar(s: &str) -> String {
    if s.is_empty() {
        return "\"\"".to_string();
    }
    // 加引号的判据必须**与 parse_models_yml 的 unquote 能力对齐**。
    //
    // 踩过的坑（2026-10-17）：最初只要含 ':' 就加引号，结果 URL
    // `http://127.0.0.1:8788/v1` 被写成 `"http://..."`，而 unquote 只剥
    // 首尾双引号、不处理转义，回读时值里留着引号 → 往返测试失败。
    //
    // 正确判据：**只有真正会被解析器误读的字符才加引号**：
    //   · '#'   → 行内注释起始，必须引
    //   · 首尾空格 → 会被 trim 掉
    //   · 首字符 '- '/'{ '/'[' → 会被当结构符号
    //   · 引号本身 → 会与包裹引号混淆
    // 冒号**不加引号**：YAML 只在「冒号 + 空格」才当键值分隔，
    // `http://x` 和 `127.0.0.1:8788` 都不会误判；而 unquote 处理不了
    // 引号内的冒号，加了反而坏。
    let need_quote = s.contains('#')
        || s.contains('"')
        || s.contains('\'')
        || s.starts_with(' ')
        || s.ends_with(' ')
        || s.starts_with('-')
        || s.starts_with('{')
        || s.starts_with('[');
    if !need_quote {
        return s.to_string();
    }
    let esc = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{esc}\"")
}

/// 旧环境兼容目录（**Deprecated**，仅在前几档都没找到时才用）。
fn legacy_agent_dir() -> PathBuf {
    std::env::var("OMP_AGENT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/root/.omp/agent"))
}

/// 解析 LLM 配置所在目录。
///
/// 查找顺序见模块文档。`root` 为资料库根（可传 None 表示无库上下文）。
/// 返回第一个含 `models.yml` 的目录；都没找到时返回库根下的首选位置
/// （以便报错信息指向用户该创建文件的地方）。
pub fn resolve_llm_dir(root: Option<&Path>) -> PathBuf {
    // 1. 显式环境变量
    if let Ok(d) = std::env::var("TAIXU_LLM_DIR") {
        let p = PathBuf::from(d);
        if p.is_dir() {
            return p;
        }
    }
    // 2/3. 库根下的候选
    if let Some(r) = root {
        for sub in ["05_管线框架/data", "_config", ".", "00_系统配置"] {
            let p = if sub == "." { r.to_path_buf() } else { r.join(sub) };
            if p.join("models.yml").is_file() {
                return p;
            }
        }
    }
    // 4. 直接指定文件
    if let Ok(f) = std::env::var("TAIXU_LLM_MODELS_YML") {
        let p = PathBuf::from(f);
        if let Some(par) = p.parent() {
            return par.to_path_buf();
        }
    }
    // 5. 旧环境兼容
    legacy_agent_dir()
}

/// 兼容旧调用（无库上下文时）。新代码请用 `resolve_llm_dir(Some(root))`。
pub fn agent_dir() -> PathBuf {
    resolve_llm_dir(None)
}

/// 判断一个列表项是否像 API key（而非模型名等其它列表项）。
/// 依据：实测 key 形如 `sk-...`（51 字符）；模型名形如 `glm-5.3-flash`。
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
///
/// 2026-10-15：**允许空 key**。workbuddy 本地代理为免鉴权模式（带非空无效 key
/// 返回 401，空/无 Authorization 才通），故 `- ""` 是合法的「空 key」条目；
/// 若在此处丢弃空串，key 池会变空导致调用报「key 池为空」。
fn key_item(t: &str) -> Option<String> {
    let v = t.strip_prefix("- ")?;
    Some(unquote(v))
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

        // provider 名（缩进 2，形如 `workbuddy:`）
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

        // `- id: <model>`（models 列表项，缩进与 keyPool 同级或更深）
        if let Some(rest) = t.strip_prefix("- id:") {
            let m = unquote(rest.trim());
            if !m.is_empty() {
                if let Some(p) = out.get_mut(&cname) {
                    p.models.push(m);
                }
            }
            in_keys = false;
            continue;
        }

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
                // `- id: <model>`（models 列表项）
                if let Some(rest) = t.strip_prefix("- id:") {
                    let m = unquote(rest.trim());
                    if !m.is_empty() {
                        if let Some(p) = out.get_mut(&cname) {
                            p.models.push(m);
                        }
                    }
                    in_keys = false;
                    continue;
                }
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
/// 组装一次调用的 LLM 参数。
///
/// key 收集顺序：
///   1. `config.yml` 的 `modelRoles.default` 所指 provider（如 `workbuddy/glm-5.3-flash`）
///   2. `prefer` 显式指定的 provider
///   3. 其余所有 provider（按名字序，作兜底）
///
/// ⚠ 旧版硬编码 provider 白名单 —— 用户自定义 provider 名会被完全忽略。
pub fn build_params(
    provs: &Providers,
    roles: &BTreeMap<String, String>,
    prefer: Option<&[&str]>,
) -> crate::llm::LlmParams {
    // config.yml 的 default 形如 `workbuddy/glm-5.3-flash`
    let role_default = roles.get("default");
    let (role_prov, role_model) = match role_default {
        Some(v) => match v.split_once('/') {
            Some((p, m)) => (Some(p.to_string()), m.to_string()),
            None => (None, v.clone()),
        },
        None => (None, String::new()),
    };

    let mut order: Vec<String> = Vec::new();
    if let Some(p) = &role_prov {
        order.push(p.clone());
    }
    if let Some(pf) = prefer {
        for p in pf {
            if !order.iter().any(|x| x == p) {
                order.push((*p).to_string());
            }
        }
    }
    // 兜底：其余 provider
    for name in provs.map.keys() {
        if !order.iter().any(|x| x == name) {
            order.push(name.clone());
        }
    }

    let mut keys = Vec::new();
    let mut base_url = String::new();
    for name in &order {
        if let Some(p) = provs.map.get(name) {
            if base_url.is_empty() && !p.base_url.is_empty() {
                base_url = p.base_url.clone();
            }
            keys.extend(p.keys.iter().cloned());
        }
    }

    // 模型名：config.yml 指定优先；否则取第一个有 key 的 provider 的第一个模型
    let model = if !role_model.is_empty() {
        role_model
    } else {
        provs
            .map
            .values()
            .find(|p| !p.keys.is_empty())
            .and_then(|p| p.models.first().cloned())
            .unwrap_or_default()
    };

    crate::llm::LlmParams {
        base_url,
        model,
        keys,
        ..Default::default()
    }
}
