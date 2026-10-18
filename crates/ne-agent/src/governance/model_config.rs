//! 模型配置的**可读可写**接口 —— 2026-10-17
//!
//! # 为什么需要这个模块（上一版"只能看不能改"的根因）
//!
//! 面板原来有两个配置接口，**都不是模型配置**：
//!
//! | 接口 | 实际读写 | 内容 |
//! |---|---|---|
//! | `GET/POST /api/config/llm` | `资料库/config/llm.toml` | 超时/重试/缓存/熔断阈值 |
//!
//! 而真正决定"用哪个模型、连哪个网关、拿什么 key"的是
//! **`.neagent/models.yml`**（由 `ne_engine::providers` 读取），
//! 而 `providers.rs` **只有解析函数、没有序列化函数** —— 物理上无法写。
//!
//! 所以用户看到"设置界面只能看不能改"不是 UI 问题，是**后端能力缺失**。
//!
//! # 本模块做什么
//!
//! · `GET  /api/config/model` → 读 models.yml，返回结构化 + **key 打码**
//! · `POST /api/config/model` → 写 models.yml，**先备份、后写、再回读校验**
//!
//! # 安全（三条硬规则）
//!
//! 1. **key 永不回显**。读接口返回 `api_key_set: true` + `api_key_mask: "sk-***abcd"`，
//!    绝不返回明文。否则任何人打开页面就能抄走 key。
//! 2. **写时接受明文**（这是唯一能给 key 的途径），但响应里同样只回掩码。
//! 3. **写前必备份 + 写后必回读校验**。回读解析失败或 provider 数不符 → 自动回滚，
//!    并返回 500 + 原因。宁可写不成，不能写坏——写坏了模型就调不通了。
//!
//! # 热加载
//!
//! `models.yml` 是**每次调用时现读**的（`providers::load_from_dir`），
//! 没有任何内存缓存，所以写盘即生效，**不需要重启任何进程**。
//! 面板会顺手做一次 `wg_llm_status` 探活，把"是否真的通了"告诉用户。

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use std::collections::BTreeMap;
use ne_engine::providers::{self, Provider, Providers};

/// models.yml 相对于"LLM 配置目录"的文件名。
const FILE: &str = "models.yml";

/// **真实生成**读的那份（`l5.py` / `cardgen.py`）。
///
/// `l5.py:46` 写死：`OMPO_DIR = os.environ.get("OMP_AGENT_DIR", "/root/.omp/agent")`，
/// 然后读 `$OMP_AGENT_DIR/models.yml`。这是唯一决定"写正文/建卡时用哪个模型"的文件。
///
/// ⚠ 它的解析器（`l5.py:_omp_parse_yaml_blocks`）**只认 `apiKeys:` 旧结构**，
///   写成 `task.keyPool:` 会读到 0 个 key → 生成时直接失败。
const OMP_DIR_ENV: &str = "OMP_AGENT_DIR";
const OMP_DIR_DEFAULT: &str = "/root/.omp/agent";

/// 取真实生成用的配置目录（支持 OMP_AGENT_DIR 覆盖）。
fn omp_dir() -> PathBuf {
    std::env::var(OMP_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(OMP_DIR_DEFAULT))
}

/// 三个写入目标 + 它们的格式要求。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyStyle {
    /// `apiKeys:`（缩进 4，项缩进 6）—— l5.py 只认这个
    ApiKeys,
    /// `task: / keyPool:`（缩进 6，项缩进 8）—— providers.rs 认这个
    KeyPool,
}

/// 一个写入目标。
struct Target {
    path: PathBuf,
    style: KeyStyle,
    label: &'static str,
}

/// 解析 models.yml 所在目录。
///
/// 复用 `ne_engine::providers::resolve_llm_dir`（它已有完整查找顺序：
/// 环境变量 → 库根 → .neagent → 旧位置），**不另立一套**。
pub fn llm_dir(lib_root: &Path) -> PathBuf {
    providers::resolve_llm_dir(Some(lib_root))
}

/// key 打码：保留前 3 后 4，中间固定 `***`。
///
/// 例：`sk-abcdefgh1234` → `sk-***1234`
/// 短 key（≤8）→ 全掩码 `***`，避免"打个码等于没打"。
fn mask_key(k: &str) -> String {
    let k = k.trim();
    if k.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = k.chars().collect();
    if chars.len() <= 8 {
        return "***".to_string();
    }
    let head: String = chars.iter().take(3).collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}***{tail}")
}

// ══════════════════════════════════════════ 多格式序列化

/// 按目标风格把 `Providers` 渲染成 YAML 文本。
///
/// # 两种风格的缩进契约（都是硬约束，错一个字符就解析不出来）
///
/// ```yaml
/// # ── KeyStyle::ApiKeys（l5.py 只认这个）──
/// providers:
///   agens:
///     baseUrl: http://127.0.0.1:8788/v1
///     api: openai-completions
///     apiKeys:            # 缩进 4
///       - ""              # 缩进 6
///     models:             # 缩进 4
///       - id: glm-5.3-flash
///         name: glm-5.3-flash
///
/// # ── KeyStyle::KeyPool（providers.rs 认这个）──
/// providers:
///   workbuddy:
///     baseUrl: ...
///     api: openai-completions
///     task:
///       keyPool:          # 缩进 6
///         - ""            # 缩进 8
///     models:             # 缩进 4
///       - id: ...
/// ```
fn render_yml(p: &Providers, style: KeyStyle) -> String {
    let mut out = String::new();
    out.push_str("# 小说系统 · LLM provider 配置\n");
    out.push_str("# ⚠ 由面板「设置中心」写入。缩进是解析契约，勿手改。\n");
    match style {
        KeyStyle::ApiKeys => {
            out.push_str("# 本文件供 l5.py / cardgen.py 读取，必须使用 apiKeys: 结构。\n");
        }
        KeyStyle::KeyPool => {
            out.push_str("# 本文件供 ne-engine::providers 读取，使用 task.keyPool: 结构。\n");
        }
    }
    out.push_str("providers:\n");

    if p.map.is_empty() {
        out.push_str("  {}\n");
        return out;
    }

    for (name, prov) in &p.map {
        let name = if name.is_empty() { "default" } else { name.as_str() };
        out.push_str(&format!("  {name}:\n"));

        if !prov.base_url.is_empty() {
            out.push_str(&format!("    baseUrl: {}\n", ne_engine::providers::yaml_scalar(&prov.base_url)));
        }
        out.push_str("    api: openai-completions\n");

        // ── key 段：两种风格 ──
        match style {
            KeyStyle::ApiKeys => {
                out.push_str("    apiKeys:\n");
                if prov.keys.is_empty() {
                    out.push_str("      - \"\"\n");
                } else {
                    for k in &prov.keys {
                        out.push_str(&format!("      - {}\n", ne_engine::providers::yaml_scalar(k)));
                    }
                }
            }
            KeyStyle::KeyPool => {
                out.push_str("    task:\n      keyPool:\n");
                if prov.keys.is_empty() {
                    out.push_str("        - \"\"\n");
                } else {
                    for k in &prov.keys {
                        out.push_str(&format!("        - {}\n", ne_engine::providers::yaml_scalar(k)));
                    }
                }
            }
        }

        // ── models 段：两种风格都一样（缩进 4）──
        if !prov.models.is_empty() {
            out.push_str("    models:\n");
            for m in &prov.models {
                let m = m.trim();
                if m.is_empty() {
                    continue;
                }
                out.push_str(&format!("      - id: {}\n", ne_engine::providers::yaml_scalar(m)));
                out.push_str(&format!("        name: {}\n", ne_engine::providers::yaml_scalar(m)));
                out.push_str("        contextWindow: 128000\n");
                out.push_str("        maxTokens: 32000\n");
            }
        }
    }
    out
}

/// 计算本次写入的全部目标。
fn targets(lib_root: &Path) -> Vec<Target> {
    let mut v = Vec::new();

    // ① OMP 池 —— **真实生成**读的那份，必须 apiKeys 结构
    v.push(Target {
        path: omp_dir().join(FILE),
        style: KeyStyle::ApiKeys,
        label: "omp（l5.py / cardgen.py 真生成）",
    });

    // ② data/models.yml —— Rust 侧 resolve_llm_dir 找到的那份
    let primary = llm_dir(lib_root);
    v.push(Target {
        path: primary.join(FILE),
        style: KeyStyle::KeyPool,
        label: "data（ne-engine::providers）",
    });

    // ③ .neagent/models.yml —— 旧路径，若存在就一并修好
    //    位置推导：数据根/crates/ne-agent → 上两级是仓库根 → .neagent
    if let Ok(p) = std::env::var("NE_AGENT_VAULT") {
        v.push(Target {
            path: PathBuf::from(p).join(FILE),
            style: KeyStyle::KeyPool,
            label: "vault（NE_AGENT_VAULT）",
        });
    }

    // 去重（万一两个路径指向同一文件）
    let mut seen = std::collections::HashSet::new();
    v.retain(|t| seen.insert(t.path.clone()));
    v
}

/// 一次写入的结果明细。
struct WriteOutcome {
    label: &'static str,
    path: String,
    ok: bool,
    detail: String,
}

/// 把 **patch 里提到的 provider** 合并进每个目标各自的现状，然后写盘。
///
/// # 为什么不是"把同一个 Providers 写三遍"（2026-10-17 实测踩到的 bug）
///
/// 初版把同一个 `Providers` 渲染给所有目标，结果：
///   OMP 池原本只有 `agens`，却凭空多出一个 `workbuddy`
///   （因为 data 那份有 workbuddy），且该 provider 缺 models 段 →
///   渲染出**缩进 0 的游离 models: 块**，直接破坏 YAML。
///
/// 正确语义：**每个目标保留自己的 provider 集合**，
/// 只把"用户这次改的 provider"应用进去。这样：
///   · 改 agens 的模型 → 三处都更新 agens
///   · data 独有的 workbuddy → 不会跑到 OMP 池里
///   · OMP 独有的 provider → 不会被删
///
/// 返回 `(全部成功, 明细列表)`。
fn write_all(patch: &Providers, tgs: &[Target]) -> (bool, Vec<WriteOutcome>) {
    // ── 先全部读一遍原文（回滚用内存，不依赖备份文件）──
    let mut originals: Vec<Option<String>> = Vec::new();
    for t in tgs {
        let txt = if t.path.is_file() {
            std::fs::read_to_string(&t.path).ok()
        } else {
            None
        };
        originals.push(txt);
    }

    let mut outcomes: Vec<WriteOutcome> = Vec::new();
    let mut all_ok = true;

    for t in tgs.iter() {
        // 目录不存在 → 建
        if let Some(d) = t.path.parent() {
            if !d.is_dir() {
                if let Err(e) = std::fs::create_dir_all(d) {
                    outcomes.push(WriteOutcome {
                        label: t.label,
                        path: t.path.to_string_lossy().to_string(),
                        ok: false,
                        detail: format!("建目录失败：{e}"),
                    });
                    all_ok = false;
                    continue;
                }
            }
        }

        // ── 读本目标自己的现状，把 patch 合并进来 ──
        let mut merged = if t.path.is_file() {
            std::fs::read_to_string(&t.path)
                .ok()
                .map(|txt| match t.style {
                    // OMP 池用 l5 的语义（apiKeys），但结构兼容：providers::parse 两种都吃
                    KeyStyle::ApiKeys | KeyStyle::KeyPool => {
                        ne_engine::providers::parse_models_yml(&txt)
                    }
                })
                .unwrap_or_default()
        } else {
            Providers::default()
        };

        // 合并规则：**只更新目标自己已有的 provider**，
        // 仅当目标文件原本不存在时才允许新建 provider。
        // 这样「改 agens」不会给只有 workbuddy 的目标凭空塞 agens，
        // 而「首次建文件」仍能一次把 patch 里的 provider 全建出来。
        let target_is_new = !t.path.is_file();
        for (name, p) in &patch.map {
            if merged.map.contains_key(name) || target_is_new {
                merged.map.insert(name.clone(), p.clone());
            }
        }

        // 合并后若为空（patch 空且目标原本空）→ 跳过但不算失败
        if merged.map.is_empty() {
            outcomes.push(WriteOutcome {
                label: t.label,
                path: t.path.to_string_lossy().to_string(),
                ok: true,
                detail: "跳过（无 provider 可写）".to_string(),
            });
            continue;
        }

        // 备份（尽力而为；回滚靠内存原文）
        if t.path.is_file() {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let b = t.path.with_extension(format!("yml.bak.{ts}"));
            let _ = std::fs::copy(&t.path, &b);
        }

        // 写
        let text = render_yml(&merged, t.style);
        if let Err(e) = std::fs::write(&t.path, &text) {
            outcomes.push(WriteOutcome {
                label: t.label,
                path: t.path.to_string_lossy().to_string(),
                ok: false,
                detail: format!("写盘失败：{e}"),
            });
            all_ok = false;
            continue;
        }

        // 回读校验：**必须能把自己刚写的内容解析回来，且 provider 集合一致**
        let parsed = match t.style {
            KeyStyle::KeyPool => ne_engine::providers::load_from_dir(
                t.path.parent().unwrap_or(Path::new(".")),
            )
            .map(|(bp, _)| bp)
            .ok(),
            // ApiKeys 与 KeyPool 的结构差异只在 key 段，
            // providers::parse_models_yml 两种都认，所以同一套校验可用
            KeyStyle::ApiKeys => std::fs::read_to_string(&t.path)
                .ok()
                .map(|txt| ne_engine::providers::parse_models_yml(&txt)),
        };

        let (parsed_ok, detail) = match parsed {
            Some(bp) => {
                let missing: Vec<&String> = merged
                    .map
                    .keys()
                    .filter(|k| !bp.map.contains_key(*k))
                    .collect();
                if missing.is_empty() {
                    (
                        true,
                        format!(
                            "回读通过（{} 个 provider / {} 字节）",
                            bp.map.len(),
                            text.len()
                        ),
                    )
                } else {
                    (false, format!("回读缺 provider：{missing:?}"))
                }
            }
            None => (false, "回读解析失败".to_string()),
        };

        outcomes.push(WriteOutcome {
            label: t.label,
            path: t.path.to_string_lossy().to_string(),
            ok: parsed_ok,
            detail,
        });
        if !parsed_ok {
            all_ok = false;
        }
    }

    // ── 任一处失败 → 全部回滚 ──
    if !all_ok {
        for (i, t) in tgs.iter().enumerate() {
            match &originals[i] {
                Some(txt) => {
                    let _ = std::fs::write(&t.path, txt);
                }
                None => {
                    let _ = std::fs::remove_file(&t.path);
                }
            }
        }
    }

    (all_ok, outcomes)
}

/// 同步 `data/config.yml` 的 `modelRoles.default`。
///
/// 为什么必须同步：`l5.py` 通过 `_omp_roles()` 读 `config.yml` 的
/// `modelRoles.default` 决定"用哪个 provider/model"。如果只改了 models.yml
/// 里的模型名，而 roles 还指向旧名字，拼出来的请求就是 `provider/旧模型` → 404。
fn sync_model_role(path: &Path, provider: &str, model: &str) -> Result<String> {
    let want = format!("{provider}/{model}");
    let text = if path.is_file() {
        std::fs::read_to_string(path).unwrap_or_default()
    } else {
        String::new()
    };

    if text.is_empty() {
        // 文件不存在 → 造一个最小可用结构
        let body = format!(
            "# 小说系统 · 模型角色绑定\nmodelRoles:\n  default: {want}\n"
        );
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).ok();
        }
        std::fs::write(path, &body)
            .map_err(|e| anyhow!("写 config.yml 失败 {}：{e}", path.display()))?;
        return Ok(want);
    }

    // 逐行改 `default:` 那一行（在 modelRoles 块内）
    let mut out: Vec<String> = Vec::new();
    let mut in_roles = false;
    let mut replaced = false;
    for raw in text.lines() {
        let t = raw.trim();
        if !raw.starts_with(' ') && t.ends_with(':') {
            in_roles = t == "modelRoles:";
            out.push(raw.to_string());
            continue;
        }
        if in_roles && t.starts_with("default:") {
            out.push(format!("  default: {want}"));
            replaced = true;
            continue;
        }
        out.push(raw.to_string());
    }
    if !replaced {
        // 没有 modelRoles 块或没有 default → 追加
        if !text.contains("modelRoles:") {
            out.push("modelRoles:".to_string());
        }
        out.push(format!("  default: {want}"));
    }

    let mut body = out.join("\n");
    if !body.ends_with('\n') {
        body.push('\n');
    }
    std::fs::write(path, &body)
        .map_err(|e| anyhow!("写 config.yml 失败 {}：{e}", path.display()))?;
    Ok(want)
}

// ══════════════════════════════════════════ 读

/// 读当前模型配置（key 打码）。
pub fn read(lib_root: &Path) -> Result<Value> {
    let dir = llm_dir(lib_root);
    let path = dir.join(FILE);

    let (provs, _extra) = providers::load_from_dir(&dir)
        .map_err(|e| anyhow!("读取 models.yml 失败：{e}"))?;

    let mut items = Vec::new();
    for (name, p) in &provs.map {
        // 只暴露"有没有 key"，不暴露 key 本身
        let keys_masked: Vec<String> = p
            .keys
            .iter()
            .filter(|k| !k.trim().is_empty())
            .map(|k| mask_key(k))
            .collect();
        items.push(json!({
            "name": name,
            "base_url": p.base_url,
            "api": "openai-completions",
            "models": p.models,
            "api_key_set": !keys_masked.is_empty(),
            "api_key_mask": keys_masked.first().cloned().unwrap_or_default(),
            "api_key_count": keys_masked.len(),
        }));
    }

    let file_exists = path.is_file();
    let size = if file_exists {
        std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
    } else {
        0
    };

    // ── 读角色绑定（决定"实际用哪个 provider/model"）──
    //
    // 必须同时报两份：OMP 侧是 l5.py 真生成用的，data 侧是 Rust 侧用的。
    // 两者不一致时前端要能看见（历史故障：只改一处 → 看得到"保存成功"但没生效）。
    let omp_roles_p = omp_dir().join("config.yml");
    let data_roles_p = dir.join("config.yml");
    let roles_of = |p: &PathBuf| -> BTreeMap<String, String> {
        std::fs::read_to_string(p)
            .map(|t| providers::parse_roles(&t))
            .unwrap_or_default()
    };
    let omp_roles = roles_of(&omp_roles_p);
    let data_roles = roles_of(&data_roles_p);
    let omp_def = omp_roles.get("default").cloned().unwrap_or_default();
    let data_def = data_roles.get("default").cloned().unwrap_or_default();
    // 主口径取 OMP 侧；OMP 没有就退到 data 侧
    let effective = if !omp_def.is_empty() {
        omp_def.clone()
    } else {
        data_def.clone()
    };
    // ── 一致性判据：**不是字符串相等** ──
    //
    // 两个池的 provider 集合本来就不同（OMP 可能只有 agens，data 还多一个 workbuddy），
    // 所以 `agens/x` vs `workbuddy/x` 是合理差异，用字符串相等判会永远报"不一致"。
    //
    // 真正要防的是 2026-10-17 09:52 那个高危 bug：
    //   role 指向的 provider **不在该池里** → l5.py 查不到 baseUrl →
    //   回落到硬编码公网地址 https://api.agnes-ai.cn/v1，绕过本地网关。
    //
    // 所以判据是：每个 role 的 provider 必须在它同目录的 models.yml 里存在。
    let role_valid = |role: &str, pool: &Providers| -> bool {
        if role.is_empty() {
            return false;
        }
        match role.split_once('/') {
            Some((p, _)) => pool.map.contains_key(p),
            None => false,
        }
    };
    let omp_pool = providers::load_from_dir(&omp_dir()).map(|(p, _)| p).unwrap_or_default();
    let data_pool = providers::load_from_dir(&dir).map(|(p, _)| p).unwrap_or_default();
    let valid_omp = role_valid(&omp_def, &omp_pool);
    let valid_data = role_valid(&data_def, &data_pool);
    let consistent = valid_omp && valid_data;
    // 差异是否属于"正常差异"（两边都有效，只是指向不同池成员）
    let benign_diff = consistent && omp_def != data_def;

    Ok(json!({
        "ok": true,
        "path": path.to_string_lossy(),
        "dir": dir.to_string_lossy(),
        "file_exists": file_exists,
        "file_size": size,
        "providers": items,
        "model_role": {
            "effective": effective,
            "omp": omp_def,
            "data": data_def,
            "omp_path": omp_roles_p.to_string_lossy(),
            "data_path": data_roles_p.to_string_lossy(),
            "consistent": consistent,
            "valid_omp": valid_omp,
            "valid_data": valid_data,
            "benign_diff": benign_diff,
            "note": if consistent && benign_diff {
                "两份都有效（指向各自池里的 provider；两池集合不同，属正常差异）"
            } else if consistent {
                "两份角色绑定一致"
            } else if !valid_omp && !omp_def.is_empty() {
                "🔴 OMP 角色指向了池里不存在的 provider！l5.py 会回落到公网地址，必须修"
            } else if !valid_data && !data_def.is_empty() {
                "⚠ data 角色指向了池里不存在的 provider"
            } else {
                "⚠ 有一侧角色绑定为空或无效"
            },
        },
        "note": "api_key 永不回显（只报掩码与是否已设置）。写入请用 POST，接受明文。",
        "hot_reload": true,
        "hot_reload_note": "models.yml 每次调用现读，无内存缓存，写盘即生效，无需重启进程。",
    }))
}

// ══════════════════════════════════════════ 写

/// 写模型配置。
///
/// 请求体（两种形态都接受）：
///
/// ```json
/// // ① 单 provider（最常用）
/// {
///   "provider": "workbuddy",
///   "base_url": "http://127.0.0.1:8788/v1",
///   "model": "deepseek-v4.1-flash",
///   "api_key": "sk-xxxx"          // 可选；不传/传空 = 不改动现有 key
/// }
///
/// // ② 全量（多个 provider）
/// { "providers": [ {"name":"a","base_url":"...","models":["m1"]}, ... ] }
/// ```
///
/// ## 关键语义：`api_key` 传空串 = 保留原值
///
/// 因为读接口只回掩码，前端回填时拿到的是 `sk-***abcd` 这种**不是真 key** 的值。
/// 如果用户没动 key 输入框直接保存，前端传上来的就是掩码或空串——
/// 这时**必须保留磁盘上的真 key**，绝不能把掩码写进去（写进去模型就废了）。
pub fn write(conn: &rusqlite::Connection, lib_root: &Path, body: &str) -> Result<Value> {
    let v: Value = serde_json::from_str(body.trim()).unwrap_or(json!({}));
    if v.as_object().map(|o| o.is_empty()).unwrap_or(true) {
        bail!("请求体为空（需 JSON）");
    }

    let dir = llm_dir(lib_root);
    let path = dir.join(FILE);
    if !dir.is_dir() {
        std::fs::create_dir_all(&dir)
            .map_err(|e| anyhow!("创建配置目录失败 {}：{e}", dir.display()))?;
    }

    // ── 读现状（写失败要能回滚，且要保留未提及的字段）──
    let cur: Providers = if path.is_file() {
        providers::load_from_dir(&dir)
            .map(|(p, _)| p)
            .unwrap_or_else(|_| Providers::default())
    } else {
        Providers::default()
    };

    let mut next = cur.clone();

    // 本次真正被改动的 provider 名。
    //
    // `next` 是 `cur` 的全量克隆，`cur` 来自 data 目录（含 data 独有的 provider）。
    // 2026-10-17 真机实测把整个 `next` 当 patch 传下去，结果 OMP 池凭空冒出
    // data 独有的 `workbuddy`，还渲染出游离块把 YAML 写坏。
    // 所以 write_all 只收这一小撮。
    let mut touched: Vec<String> = Vec::new();

    if let Some(arr) = v.get("providers").and_then(|x| x.as_array()) {
        // ── 形态②：全量替换 ──
        let mut fresh: std::collections::BTreeMap<String, Provider> = Default::default();
        for it in arr {
            let name = it.get("name").and_then(|x| x.as_str()).unwrap_or("").trim();
            if name.is_empty() {
                bail!("providers[] 里有条目缺 name");
            }
            let base_url = it.get("base_url").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
            let models: Vec<String> = it
                .get("models")
                .and_then(|x| x.as_array())
                .map(|a| a.iter().filter_map(|m| m.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
                .unwrap_or_default();

            // key：传值且不是掩码 → 用新值；否则沿用旧值
            let incoming = it.get("api_key").and_then(|x| x.as_str()).unwrap_or("").trim();
            let keys = resolve_keys(&cur, name, incoming);

            touched.push(name.to_string());
            fresh.insert(
                name.to_string(),
                Provider {
                    name: name.to_string(),
                    base_url,
                    keys,
                    key_source: "panel".to_string(),
                    models,
                },
            );
        }
        next.map = fresh;
    } else {
        // ── 形态①：单 provider 增量更新 ──
        let name = v
            .get("provider")
            .and_then(|x| x.as_str())
            .map(|s| s.trim().to_string())
            .or_else(|| next.map.keys().next().cloned())
            .ok_or_else(|| anyhow!("缺 provider 名（且现有配置为空，无法推断）"))?;

        let mut p = next.map.get(&name).cloned().unwrap_or_else(|| Provider {
            name: name.clone(),
            ..Default::default()
        });

        if let Some(u) = v.get("base_url").and_then(|x| x.as_str()) {
            p.base_url = u.trim().to_string();
        }
        if let Some(m) = v.get("model").and_then(|x| x.as_str()) {
            let m = m.trim();
            if !m.is_empty() {
                // 单模型替换：保留第一个位置的语义，但清掉旧列表
                p.models = vec![m.to_string()];
            }
        }
        if let Some(arr) = v.get("models").and_then(|x| x.as_array()) {
            p.models = arr
                .iter()
                .filter_map(|m| m.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        let incoming = v.get("api_key").and_then(|x| x.as_str()).unwrap_or("").trim();
        p.keys = resolve_keys(&cur, &name, incoming);

        touched.push(name.clone());
        next.map.insert(name, p);
    }

    // ── 校验：至少一个 provider，且每个 provider 的 baseUrl 不能空（否则必然调不通）──
    if next.map.is_empty() {
        bail!("写入后没有任何 provider，拒绝写入（会导致模型完全不可用）");
    }
    for (n, p) in &next.map {
        if p.base_url.trim().is_empty() {
            bail!("provider `{n}` 的 baseUrl 为空，拒绝写入");
        }
        if p.models.is_empty() {
            bail!("provider `{n}` 没有任何 model，拒绝写入");
        }
    }

    let _ = &dir; // dir 仅用于建目录（已在上面做过），这里显式忽略避免 unused 警告

    // ══════ 方案 A：三处全同步 ══════
    //
    // 为什么必须写多处：系统里有两个独立子系统读配置，格式还不同
    //   · OMP 池（/root/.omp/agent/models.yml）→ l5.py / cardgen.py 真生成，**只认 apiKeys:**
    //   · data + .neagent（models.yml）→ ne-engine::providers，认 task.keyPool:
    // 只写一处 = 用户看到"保存成功"但模型没变。
    let tgs = targets(lib_root);
    // ── 构造 patch：**只含本次改动的 provider** ──
    let mut patch = Providers::default();
    for k in &touched {
        if let Some(p) = next.map.get(k) {
            patch.map.insert(k.clone(), p.clone());
        }
    }
    let (all_ok, outcomes) = write_all(&patch, &tgs);

    if !all_ok {
        // write_all 内部已把三处全部回滚
        let detail: Vec<String> = outcomes
            .iter()
            .map(|o| format!("[{}] {} → {}", if o.ok { "OK" } else { "FAIL" }, o.label, o.detail))
            .collect();
        bail!(
            "写入失败，**三处已全部回滚**（磁盘内容与写入前一致）。\n{}",
            detail.join("\n")
        );
    }

    // ── 同步 modelRoles.default（否则模型名改了但角色指向旧名 → 404）──
    let mut role_note = String::from("未改动");
    let mut role_path = String::new();
    // config.yml 与 models.yml 同目录（l5.py 的 OMPO_ROLES / OMPO_MODELS 同源）
    // 两份 config.yml 都要改：OMP 侧决定 l5.py 用哪个模型，
    // data 侧是 Rust 侧读角色绑定的入口。
    let mut role_files: Vec<(PathBuf, bool)> = vec![(omp_dir().join("config.yml"), true)];
    let data_role = llm_dir(lib_root).join("config.yml");
    if data_role != role_files[0].0 {
        role_files.push((data_role, false));
    }
    role_path = role_files
        .iter()
        .map(|(p, _)| p.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join(" , ");

    // 取"这次改的是哪个 provider/model"来定 role
    // 先按"整体配置"算一个候选（作为 fallback），真正的写入由上面逐池校验决定。
    let (r_prov, r_model) = pick_role_target(&v, &next);
    {
        let fallback = (r_prov, r_model);
        let mut notes: Vec<String> = Vec::new();
        for (f, must) in &role_files {
            if !*must && !f.is_file() {
                continue; // data 侧原本没有 config.yml 就不凭空造
            }
            // **逐池校验**：角色只能指向同一目录 models.yml 里真实存在的 provider。
            // 否则 l5.py 查不到 baseUrl 会回落到硬编码公网地址（实测踩过）。
            let pool_dir = f.parent().unwrap_or(Path::new("."));
            let pool = providers::load_from_dir(pool_dir)
                .map(|(p, _)| p)
                .unwrap_or_default();
            match pick_role_for_pool(&v, &pool, &fallback) {
                Some((prov, model)) => match sync_model_role(f, &prov, &model) {
                    Ok(want) => notes.push(format!("{}={want}", f.display())),
                    Err(e) => notes.push(format!("{} 同步失败：{e}", f.display())),
                },
                None => notes.push(format!(
                    "{} 跳过（{} 里没有可用 provider）",
                    f.display(),
                    pool_dir.join(FILE).display()
                )),
            }
        }
        if !notes.is_empty() {
            role_note = notes.join("; ");
        }
    }

    // ── 审计 ──
    let detail_str: Vec<String> = outcomes
        .iter()
        .map(|o| format!("{}={}", o.label, if o.ok { "ok" } else { "fail" }))
        .collect();
    let _ = super::category_store::log_op(
        conn,
        "llm.models.write",
        FILE,
        &format!(
            "provider={} model={} targets=[{}] role={}",
            v.get("provider").and_then(|x| x.as_str()).unwrap_or("-"),
            v.get("model").and_then(|x| x.as_str()).unwrap_or("-"),
            detail_str.join(","),
            role_note
        ),
        "",
    );

    // 写完把"打码后的新状态"返回，前端不必再发一次 GET
    let after = read(lib_root).unwrap_or_else(|_| json!({}));

    Ok(json!({
        "ok": true,
        "targets": outcomes.iter().map(|o| json!({
            "label": o.label,
            "path": o.path,
            "ok": o.ok,
            "detail": o.detail,
        })).collect::<Vec<_>>(),
        "model_role": {
            "path": role_path,
            "result": role_note,
        },
        "hot_reload": true,
        "note": "三处已同步写入并回读校验通过。models.yml 每次调用现读，无需重启进程，下一次模型调用即生效。",
        "config": after,
    }))
}

/// 从请求体与最终配置里推出"应当写入 modelRoles 的 provider/model"。
///
/// 优先用请求体显式指定的；否则取第一个 provider 的第一个模型。
/// 为**某一个池**挑角色绑定：provider 必须在该池里真实存在。
///
/// # 这是 2026-10-17 修的真实缺陷
///
/// 原实现（`pick_role_target`）直接采信请求里的 provider 名，不看它属于哪个池。
/// 后果：面板改 data 侧独有的 `workbuddy` 时，**OMP 的 config.yml 也被写成
/// `workbuddy/deepseek-v4.1-flash`**，而 OMP 池里根本没有 workbuddy。
///
/// 接着 `l5.py:load_config()` 拿这个角色去池里查 baseUrl，
/// 查不到就回落到硬编码的 `https://api.agnes-ai.cn/v1`：
///
/// ```python
/// base_url = provs.get(prov, {}).get("base_url") or "https://api.agnes-ai.cn/v1"
/// ```
///
/// 结果**本地网关 127.0.0.1:8788 被绕过，真跑打到公网**。
/// 这是"改了不生效"之外更严重的一类故障：静默走错地址。
fn pick_role_for_pool(
    req: &Value,
    pool: &Providers,
    fallback: &(Option<String>, Option<String>),
) -> Option<(String, String)> {
    if pool.map.is_empty() {
        return None;
    }
    // ① 请求里显式指定、且该池确实有 → 用
    let explicit = req
        .get("provider")
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| pool.map.contains_key(s));
    // ② 否则用上层算出的 fallback provider（同样必须在该池里）
    let want = explicit.or_else(|| {
        fallback
            .0
            .clone()
            .filter(|s| pool.map.contains_key(s))
    });
    // ③ 再不行就退到该池第一个 provider（保证角色永远指向一个真实存在的池成员）
    let prov = want.or_else(|| pool.map.keys().next().cloned())?;
    let p = pool.map.get(&prov)?;
    // 模型：请求里指定且该 provider 真的有这个模型 → 用；否则用它第一个
    let model = req
        .get("model")
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && p.models.contains(s))
        .or_else(|| p.models.first().cloned())?;
    Some((prov, model))
}

fn pick_role_target(req: &Value, p: &Providers) -> (Option<String>, Option<String>) {
    // 显式 provider
    let prov = req
        .get("provider")
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .or_else(|| p.map.keys().next().cloned());

    let model = req
        .get("model")
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            prov.as_ref()
                .and_then(|k| p.map.get(k))
                .and_then(|pp| pp.models.first().cloned())
        })
        .or_else(|| p.map.values().next().and_then(|pp| pp.models.first().cloned()));

    (prov, model)
}

/// 决定写入后的 key 列表。
///
/// 核心规则：**空串或形如掩码的输入 = 保留原值**。
///
/// 为什么这条规则至关重要：前端拿到的 `api_key` 是 `sk-***abcd`（掩码，不是真 key）。
/// 用户如果没改 key 就点保存，前端把掩码回传上来——此时若直接写盘，
/// 磁盘上的真 key 就被掩码覆盖了，模型立刻调不通，而且**原 key 找不回来**。
fn resolve_keys(cur: &Providers, name: &str, incoming: &str) -> Vec<String> {
    let trimmed = incoming.trim();

    // 空 = 不改
    if trimmed.is_empty() {
        return cur
            .map
            .get(name)
            .map(|p| p.keys.clone())
            .unwrap_or_default();
    }

    // 看起来像掩码（含 ***）= 前端回传的占位值，不改
    if trimmed.contains("***") {
        return cur
            .map
            .get(name)
            .map(|p| p.keys.clone())
            .unwrap_or_default();
    }

    // 真 key：写入
    vec![trimmed.to_string()]
}

// ══════════════════════════════════════════ 单测

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_short_is_full() {
        assert_eq!(mask_key(""), "");
        assert_eq!(mask_key("abc"), "***");
        assert_eq!(mask_key("12345678"), "***");
    }

    #[test]
    fn mask_keeps_head_tail() {
        let m = mask_key("sk-abcdefgh1234");
        assert!(m.starts_with("sk-"));
        assert!(m.ends_with("1234"));
        assert!(m.contains("***"));
        // 真 key 的核心部分不能泄漏
        assert!(!m.contains("abcdefgh"));
    }

    #[test]
    fn resolve_keeps_original_on_empty() {
        let mut cur = Providers::default();
        cur.map.insert(
            "p".into(),
            Provider {
                name: "p".into(),
                keys: vec!["sk-REAL".into()],
                ..Default::default()
            },
        );
        // 空 → 保留
        assert_eq!(resolve_keys(&cur, "p", ""), vec!["sk-REAL".to_string()]);
        // 掩码 → 保留（**最关键的一条**：防止掩码覆盖真 key）
        assert_eq!(resolve_keys(&cur, "p", "sk-***REAL"), vec!["sk-REAL".to_string()]);
        // 真 key → 替换
        assert_eq!(resolve_keys(&cur, "p", "sk-NEW"), vec!["sk-NEW".to_string()]);
    }

    #[test]
    fn roundtrip_to_and_from_yml() {
        let mut p = Providers::default();
        p.map.insert(
            "workbuddy".into(),
            Provider {
                name: "workbuddy".into(),
                base_url: "http://127.0.0.1:8788/v1".into(),
                keys: vec!["sk-test-123456".into()],
                key_source: "panel".into(),
                models: vec!["deepseek-v4.1-flash".into(), "glm-5.3-flash".into()],
            },
        );
        let text = providers::to_models_yml(&p);
        // 缩进契约
        assert!(text.contains("\nproviders:\n"), "要有 providers 顶层块");
        assert!(text.contains("  workbuddy:\n"), "provider 缩进 2");
        assert!(text.contains("    baseUrl:"), "baseUrl 缩进 4");
        assert!(text.contains("    models:\n"), "**models 必须缩进 4**");
        assert!(text.contains("      - id: "), "模型项缩进 6");

        // 逆运算
        let back = providers::parse_models_yml(&text);
        let got = back.map.get("workbuddy").expect("应能解析回来");
        assert_eq!(got.base_url, "http://127.0.0.1:8788/v1");
        assert_eq!(got.keys, vec!["sk-test-123456".to_string()]);
        assert_eq!(got.models.len(), 2);
        assert!(got.models.contains(&"deepseek-v4.1-flash".to_string()));
    }

    #[test]
    fn yaml_scalar_quotes_when_needed() {
        assert_eq!(yaml_scalar("plain"), "plain");
        assert_eq!(yaml_scalar(""), "\"\"");
        assert_eq!(yaml_scalar("has#hash"), "\"has#hash\"");
        // URL **不加引号**：冒号后无空格，YAML 不会误判为键值分隔；
        // 而 unquote 处理不了引号内的冒号，加了反而让值带引号（真实踩过）。
        assert_eq!(yaml_scalar("http://127.0.0.1:8788/v1"), "http://127.0.0.1:8788/v1");
        assert_eq!(yaml_scalar("sk-abcdef123"), "sk-abcdef123");
    }

    /// 关键回归：URL 型 baseUrl 必须能原样往返。
    ///
    /// 这是 2026-10-17 实测失败过的场景：
    /// 写 `baseUrl: "http://..."` → 回读得到 `"http://..."`（带引号）→
    /// 拼 URL 时变成 `"http://.../chat/completions`，请求必失败。
    #[test]
    fn url_roundtrip_without_quotes() {
        let url = "http://127.0.0.1:8788/v1";
        let mut p = Providers::default();
        p.map.insert(
            "w".into(),
            Provider {
                name: "w".into(),
                base_url: url.into(),
                keys: vec!["sk-abcdef123456".into()],
                key_source: "panel".into(),
                models: vec!["glm-5.3-flash".into()],
            },
        );
        let text = providers::to_models_yml(&p);
        assert!(
            text.contains("    baseUrl: http://127.0.0.1:8788/v1\n"),
            "baseUrl 应原样书写，不加引号。实际：\n{text}"
        );
        let back = providers::parse_models_yml(&text);
        assert_eq!(back.map.get("w").unwrap().base_url, url);
    }


    // ══════════════════════════════════════════
    // 回归：patch 只应用到「目标已有」的 provider
    //
    // 锁的是 2026-10-17 真机 bug：OMP 池只有 agens，改一次 agens 的模型后
    // 凭空冒出 data 独有的 workbuddy，还带出缩进 0 的游离块，l5.py 读到 0 个 provider。
    // ══════════════════════════════════════════
    #[test]
    fn write_all_does_not_leak_foreign_providers() {
        let base = std::env::temp_dir().join(format!(
            "nc_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let dir_a = base.join("omp");
        let dir_b = base.join("data");
        std::fs::create_dir_all(&dir_a).unwrap();
        std::fs::create_dir_all(&dir_b).unwrap();

        // 目标 A（模拟 OMP 池）：只有 agens
        let mut a = Providers::default();
        a.map.insert(
            "agens".into(),
            Provider {
                name: "agens".into(),
                base_url: "http://127.0.0.1:8788/v1".into(),
                keys: vec!["".into()],
                key_source: "apiKeys".into(),
                models: vec!["old-a".into()],
            },
        );
        std::fs::write(dir_a.join(FILE), render_yml(&a, KeyStyle::ApiKeys)).unwrap();

        // 目标 B（模拟 data 目录）：agens + workbuddy
        let mut b = a.clone();
        b.map.insert(
            "workbuddy".into(),
            Provider {
                name: "workbuddy".into(),
                base_url: "http://127.0.0.1:8788/v1".into(),
                keys: vec!["".into()],
                key_source: "task.keyPool".into(),
                models: vec!["glm-x".into()],
            },
        );
        std::fs::write(dir_b.join(FILE), render_yml(&b, KeyStyle::KeyPool)).unwrap();

        let tgs = vec![
            Target {
                path: dir_a.join(FILE),
                style: KeyStyle::ApiKeys,
                label: "omp",
            },
            Target {
                path: dir_b.join(FILE),
                style: KeyStyle::KeyPool,
                label: "data",
            },
        ];

        // patch 里**只有 agens**，且模型换成新值
        let mut patch = Providers::default();
        patch.map.insert(
            "agens".into(),
            Provider {
                name: "agens".into(),
                base_url: "http://127.0.0.1:8788/v1".into(),
                keys: vec!["".into()],
                key_source: "apiKeys".into(),
                models: vec!["new-model".into()],
            },
        );

        let (ok, outcomes) = write_all(&patch, &tgs);
        assert!(ok, "写入应成功，实际 outcomes={:?}", outcomes.iter().map(|o| (&o.label, o.ok, &o.detail)).collect::<Vec<_>>());

        // ── 断言 A：不含 workbuddy（核心！）──
        let ta = std::fs::read_to_string(dir_a.join(FILE)).unwrap();
        assert!(
            !ta.contains("workbuddy"),
            "**OMP 池泄漏了 data 独有的 workbuddy**，这正是要锁的 bug。\n{ta}"
        );
        let pa = providers::parse_models_yml(&ta);
        assert_eq!(pa.map.len(), 1, "目标 A 应只有 1 个 provider，实际 {}", pa.map.len());
        assert_eq!(pa.map["agens"].models, vec!["new-model".to_string()], "agens 模型应已更新");

        // ── 断言 B：workbuddy 没被删，agens 已更新 ──
        let tb = std::fs::read_to_string(dir_b.join(FILE)).unwrap();
        let pb = providers::parse_models_yml(&tb);
        assert_eq!(pb.map.len(), 2, "目标 B 应仍有 2 个 provider");
        assert!(pb.map.contains_key("workbuddy"), "workbuddy 不应被删");
        assert_eq!(pb.map["agens"].models, vec!["new-model".to_string()], "agens 模型应已更新");
        assert_eq!(pb.map["workbuddy"].models, vec!["glm-x".to_string()], "workbuddy 模型不应被改");

        // ── 断言 C：两种风格都没写出游离块（缩进 0 的 models:）──
        for (label, txt) in [("A/apiKeys", &ta), ("B/keyPool", &tb)] {
            for ln in txt.lines() {
                assert!(
                    ln != "models:",
                    "{label} 出现缩进 0 的游离 models:，会破坏 YAML"
                );
            }
        }

        let _ = std::fs::remove_dir_all(&base);
    }


    // ══════════════════════════════════════════
    // 回归：角色绑定必须指向该池里真实存在的 provider
    //
    // 锁的是 2026-10-17 真机高危 bug：改 data 侧独有的 workbuddy 时，
    // OMP 的 config.yml 也被写成 workbuddy/...，而 OMP 池里没有它，
    // l5.py 查不到 baseUrl 就回落到硬编码公网地址。
    // ══════════════════════════════════════════
    fn pool_of(names: &[(&str, &[&str])]) -> Providers {
        let mut p = Providers::default();
        for (n, ms) in names {
            p.map.insert(
                (*n).into(),
                Provider {
                    name: (*n).into(),
                    base_url: "http://127.0.0.1:8788/v1".into(),
                    keys: vec!["".into()],
                    key_source: "apiKeys".into(),
                    models: ms.iter().map(|s| (*s).to_string()).collect(),
                },
            );
        }
        p
    }

    #[test]
    fn role_falls_back_when_provider_not_in_pool() {
        // 模拟：请求改 workbuddy，但目标池只有 agens（这就是 OMP 的真实情形）
        let omp = pool_of(&[("agens", &["deepseek-v4.1-flash"])]);
        let req = json!({"provider": "workbuddy", "model": "deepseek-v4.1-flash"});
        let got = pick_role_for_pool(&req, &omp, &(Some("workbuddy".into()), Some("x".into())));
        let (prov, model) = got.expect("池非空必须给出角色");
        assert_eq!(prov, "agens", "**不该把池里没有的 workbuddy 写成角色**（会回落公网）");
        assert_eq!(model, "deepseek-v4.1-flash");
    }

    #[test]
    fn role_uses_explicit_provider_when_present_in_pool() {
        // 请求的 provider 在池里 → 尊重它
        let pool = pool_of(&[("a", &["m1"]), ("b", &["m2"])]);
        let req = json!({"provider": "b", "model": "m2"});
        let (prov, model) = pick_role_for_pool(&req, &pool, &(None, None)).unwrap();
        assert_eq!(prov, "b");
        assert_eq!(model, "m2");
    }

    #[test]
    fn role_ignores_foreign_model_and_uses_pool_first() {
        // provider 在池里，但请求的 model 不在它的模型列表里 → 用池里的第一个模型
        let pool = pool_of(&[("a", &["only-this"])]);
        let req = json!({"provider": "a", "model": "not-in-pool"});
        let (prov, model) = pick_role_for_pool(&req, &pool, &(None, None)).unwrap();
        assert_eq!(prov, "a");
        assert_eq!(model, "only-this", "不存在的模型名不该被写进角色绑定");
    }

    #[test]
    fn role_is_none_for_empty_pool() {
        let empty = Providers::default();
        let req = json!({"provider": "a", "model": "m"});
        assert!(
            pick_role_for_pool(&req, &empty, &(Some("a".into()), Some("m".into()))).is_none(),
            "空池不能给出任何角色（调用方会跳过并报告）"
        );
    }

    #[test]
    fn role_uses_fallback_provider_when_req_has_none() {
        // 请求没指定 provider，但 fallback 指定了且池里有 → 用它
        let pool = pool_of(&[("x", &["mx"]), ("y", &["my"])]);
        let req = json!({});
        let (prov, _) = pick_role_for_pool(&req, &pool, &(Some("y".into()), None)).unwrap();
        assert_eq!(prov, "y");
    }

    // 让 yaml_scalar 在测试里可见
    use ne_engine::providers::yaml_scalar;
}