//! 引擎配置：读取 `05_管线框架/data/dynamics_config.json`（门禁阈值唯一事实源）。
//!
//! 关键原则：**缺字段就用 Python 侧同款默认值，绝不臆造**。
//! 每个默认值都注明来源，便于后续与 Python 对拍。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 全局配置（宽松反序列化：未消费的键直接忽略）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub gates: GatesCfg,
    #[serde(default)]
    pub rhythm: RhythmCfg,
    #[serde(default)]
    pub banned_by_tier: BTreeMap<String, TierCfg>,
    /// 原始 JSON（便于门禁读未建模的键，不丢信息）
    #[serde(skip)]
    pub raw: serde_json::Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GatesCfg {
    /// 章字数硬上限（gate14 g09 用），默认 4000
    #[serde(default)]
    pub chapter_word_max: Option<u32>,
    /// 章字数下限（scoring 用），默认 2500
    #[serde(default)]
    pub chapter_word_min: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RhythmCfg {
    #[serde(default)]
    pub hook_density: Option<HookDensity>,
    /// 信息密度代理上限（gate14 g09），默认 1000
    #[serde(default)]
    pub info_density_max: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HookDensity {
    #[serde(default)]
    pub min_per_chapter: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TierCfg {
    #[serde(default)]
    pub banned_words: Vec<String>,
}

impl Config {
    /// 章字数硬上限（默认 4000，同 gate14 g09）
    pub fn word_max(&self) -> u32 {
        self.gates.chapter_word_max.unwrap_or(4000)
    }
    /// 章字数下限（默认 2500，同 gate14 g13 det_dims「字数」）
    pub fn word_min(&self) -> u32 {
        self.gates.chapter_word_min.unwrap_or(2500)
    }
    /// 章末钩子最少命中数（默认 1，同 gate14 g02）
    pub fn hook_min(&self) -> u32 {
        self.rhythm
            .hook_density
            .as_ref()
            .and_then(|h| h.min_per_chapter)
            .unwrap_or(1)
    }
    /// 信息密度代理上限（默认 1000，同 gate14 g09）
    pub fn info_density_max(&self) -> u32 {
        self.rhythm.info_density_max.unwrap_or(1000)
    }
    /// 某阶位的禁词表
    pub fn banned_words(&self, tier: &str) -> Vec<String> {
        self.banned_by_tier
            .get(tier)
            .map(|t| t.banned_words.clone())
            .unwrap_or_default()
    }
}

pub fn load(root: &Path) -> Result<Config> {
    let p = root.join("05_管线框架").join("data").join("dynamics_config.json");
    if !p.exists() {
        // 配置缺失不是致命错：用默认值继续（与 Python _cfg() 返回 {} 同行为）
        return Ok(Config::default());
    }
    let txt = std::fs::read_to_string(&p)
        .with_context(|| format!("读不到配置：{}", p.display()))?;
    let raw: serde_json::Value = serde_json::from_str(&txt)
        .with_context(|| format!("配置 JSON 解析失败：{}", p.display()))?;
    let mut cfg: Config = serde_json::from_value(raw.clone()).unwrap_or_default();
    cfg.raw = raw;
    Ok(cfg)
}

pub fn config_path(root: &Path) -> PathBuf {
    root.join("05_管线框架").join("data").join("dynamics_config.json")
}
