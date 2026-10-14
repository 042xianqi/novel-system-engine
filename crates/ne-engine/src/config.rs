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
    /// 书专属档案（书名/主角/禁词/家庭 canon/开篇断言词…）。
    /// 缺省 = 《残响之河》既有硬编码值（行为不变）；新书在
    /// `05_管线框架/data/book_profile.json` 或 `dynamics_config.json` 的
    /// `book_profile` 键提供即可覆盖。
    #[serde(default)]
    pub book_profile: BookProfile,
    /// 原始 JSON（便于门禁读未建模的键，不丢信息）
    #[serde(skip)]
    pub raw: serde_json::Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GatesCfg {
    /// 章字数硬上限（gate14 g09 用），默认 4000
    #[serde(default)]
    pub chapter_word_max: Option<u32>,
    /// 章字数下限基准（scoring 用），默认 2500
    #[serde(default)]
    pub chapter_word_min: Option<u32>,
    /// 分卷字数下限爬坡（micro-chapter 结构用）：
    /// 取 `from_volume` ≤ 当前卷号 中最大的那条。
    /// 例：`[{"from_volume":4,"word_min":2500}]` + 基准 1200
    /// → 卷01-03 下限 1200（微章），卷04+ 下限 2500（长期导向）。
    #[serde(default)]
    pub chapter_word_min_ramp: Option<Vec<WordMinRamp>>,
}

/// 分卷字数下限条目（见 [`GatesCfg::chapter_word_min_ramp`]）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WordMinRamp {
    pub from_volume: u32,
    pub word_min: u32,
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

// ───────────────────────────── 书专属档案（通用化）
/// 把原本写死在 Rust 源码里的《残响之河》专属值外置。
///
/// 每个字段都是 `Option`：`None` 时访问器回落到 `gates.rs` 的历史常量，
/// 故**存量《残响之河》行为逐字不变**；新书只要在
/// `05_管线框架/data/book_profile.json`（或 `dynamics_config.json` 的
/// `book_profile` 键）里给出对应值即可覆盖。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BookProfile {
    /// 书名（system prompt / 提示用语）
    #[serde(default)]
    pub title: Option<String>,
    /// 主角名（G01 断言 A「主角出场」/ G05 视角锁定）
    #[serde(default)]
    pub protagonist: Option<String>,
    /// 恐怖章诡异要素词（G10）
    #[serde(default)]
    pub horror_keywords: Option<Vec<String>>,
    /// 能力动用词（G04）
    #[serde(default)]
    pub ability_keywords: Option<Vec<String>>,
    /// 代价词（G04）
    #[serde(default)]
    pub cost_keywords: Option<Vec<String>>,
    /// 开篇断言 B：金手指（G01）
    #[serde(default)]
    pub g01_b: Option<Vec<String>>,
    /// 开篇断言 C：核心矛盾（G01）
    #[serde(default)]
    pub g01_c: Option<Vec<String>>,
    /// 开篇断言 D：身份交代（G01）
    #[serde(default)]
    pub g01_d: Option<Vec<String>>,
    /// 家庭 canon 称谓→定版名（G15）
    #[serde(default)]
    pub canon_family: Option<Vec<(String, String)>>,
    /// 家庭 canon 合法名集合（G15）
    #[serde(default)]
    pub canon_names: Option<Vec<String>>,
    /// 来源神话硬闸禁词（G14）
    #[serde(default)]
    pub myth_banned: Option<Vec<String>>,
    /// 正典禁词（chapter.rs / ne-qc 确定性规则）
    #[serde(default)]
    pub banned_words: Option<Vec<String>>,
    /// 小说 ID（写库 `dynamics_events.novel_id` 用；默认 canxiang）
    #[serde(default)]
    pub novel_id: Option<String>,
}

fn owned(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_string()).collect()
}

impl BookProfile {
    pub fn title(&self) -> &str {
        self.title.as_deref().unwrap_or(crate::gates::BOOK_TITLE_DEFAULT)
    }
    pub fn protagonist(&self) -> &str {
        self.protagonist.as_deref().unwrap_or(crate::gates::PROTAGONIST_DEFAULT)
    }
    pub fn horror_keywords(&self) -> Vec<String> {
        self.horror_keywords.clone().unwrap_or_else(|| owned(crate::gates::HORROR_KEYWORDS))
    }
    pub fn ability_keywords(&self) -> Vec<String> {
        self.ability_keywords.clone().unwrap_or_else(|| owned(crate::gates::ABILITY_KEYWORDS))
    }
    pub fn cost_keywords(&self) -> Vec<String> {
        self.cost_keywords.clone().unwrap_or_else(|| owned(crate::gates::COST_KEYWORDS))
    }
    pub fn g01_b(&self) -> Vec<String> {
        self.g01_b.clone().unwrap_or_else(|| owned(crate::gates::G01_B))
    }
    pub fn g01_c(&self) -> Vec<String> {
        self.g01_c.clone().unwrap_or_else(|| owned(crate::gates::G01_C))
    }
    pub fn g01_d(&self) -> Vec<String> {
        self.g01_d.clone().unwrap_or_else(|| owned(crate::gates::G01_D))
    }
    pub fn canon_family(&self) -> Vec<(String, String)> {
        self.canon_family.clone().unwrap_or_else(|| {
            crate::gates::CANON_FAMILY
                .iter()
                .map(|(t, c)| ((*t).to_string(), (*c).to_string()))
                .collect()
        })
    }
    pub fn canon_names(&self) -> Vec<String> {
        self.canon_names.clone().unwrap_or_else(|| owned(crate::gates::CANON_NAMES))
    }
    pub fn myth_banned(&self) -> Vec<String> {
        self.myth_banned.clone().unwrap_or_else(|| owned(crate::gates::MYTH_BANNED))
    }
    pub fn banned_words(&self) -> Vec<String> {
        self.banned_words.clone().unwrap_or_else(|| owned(&crate::chapter::BANNED_WORDS))
    }
    /// 含主角名 + 开篇断言 + canon 名的「硬编码敏感词」集合（供 G13 禁词维等）
    pub fn stop_words(&self) -> Vec<String> {
        owned(crate::gates::G13_STOP_WORDS)
    }
    /// 小说 ID（写库用；默认 canxiang）
    pub fn novel_id(&self) -> &str {
        self.novel_id.as_deref().unwrap_or(crate::ops::NOVEL_ID)
    }
    /// 是否有任何自定义（用于日志）
    pub fn is_custom(&self) -> bool {
        self.title.is_some() || self.protagonist.is_some()
    }
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

    /// 按卷号取章字数下限（支持 micro-chapter 分卷爬坡，见 [`GatesCfg::chapter_word_min_ramp`]）。
    /// 无 ramp 配置时回落 [`Config::word_min`]。
    pub fn word_min_for_volume(&self, volume: &str) -> u32 {
        let base = self.word_min();
        let vnum = parse_volume_no(volume);
        let Some(ramp) = self.gates.chapter_word_min_ramp.as_ref() else {
            return base;
        };
        ramp.iter()
            .filter(|r| r.from_volume <= vnum)
            .max_by_key(|r| r.from_volume)
            .map(|r| r.word_min)
            .unwrap_or(base)
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
        return Ok(with_book_profile(Config::default(), root));
    }
    let txt = std::fs::read_to_string(&p)
        .with_context(|| format!("读不到配置：{}", p.display()))?;
    let raw: serde_json::Value = serde_json::from_str(&txt)
        .with_context(|| format!("配置 JSON 解析失败：{}", p.display()))?;
    let mut cfg: Config = serde_json::from_value(raw.clone()).unwrap_or_default();
    cfg.raw = raw;
    Ok(with_book_profile(cfg, root))
}

/// 合并书档案：优先独立文件 `book_profile.json`，其次 `dynamics_config.json`
/// 内嵌的 `book_profile` 键；两处都没有则维持默认（《残响之河》行为不变）。
fn with_book_profile(mut cfg: Config, root: &Path) -> Config {
    let bp_file = root.join("05_管线框架").join("data").join("book_profile.json");
    if let Ok(txt) = std::fs::read_to_string(&bp_file) {
        if let Ok(bp) = serde_json::from_str::<BookProfile>(&txt) {
            cfg.book_profile = bp;
            return cfg;
        }
    }
    if !cfg.book_profile.is_custom() {
        if let Some(bp_val) = cfg.raw.get("book_profile") {
            if let Ok(bp) = serde_json::from_value::<BookProfile>(bp_val.clone()) {
                cfg.book_profile = bp;
            }
        }
    }
    cfg
}

pub fn config_path(root: &Path) -> PathBuf {
    root.join("05_管线框架").join("data").join("dynamics_config.json")
}

/// 从卷号字符串解析卷序数（对齐 `gates.rs::g12_naming_tier` 的取法：
/// 取末尾两位数字；无法解析时回落 0）。
pub fn parse_volume_no(volume: &str) -> u32 {
    volume
        .chars()
        .rev()
        .take(2)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>()
        .parse()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests_word_min_ramp {
    use super::*;

    fn cfg_with_ramp() -> Config {
        let mut c = Config::default();
        c.gates.chapter_word_min = Some(2500);
        c.gates.chapter_word_min_ramp = Some(vec![
            WordMinRamp { from_volume: 1, word_min: 1200 },
            WordMinRamp { from_volume: 2, word_min: 2500 },
        ]);
        c
    }

    /// 回归：micro 章（卷01）下限 1200，卷02+ 回落 2500。
    #[test]
    fn ramp_picks_largest_from_volume_le_current() {
        let c = cfg_with_ramp();
        assert_eq!(c.word_min_for_volume("01"), 1200);
        assert_eq!(c.word_min_for_volume("卷01"), 1200, "带「卷」前缀也要能解析");
        assert_eq!(c.word_min_for_volume("02"), 2500);
        assert_eq!(c.word_min_for_volume("10"), 2500);
    }

    /// 无 ramp 时回落 word_min()。
    #[test]
    fn no_ramp_falls_back_to_base() {
        let c = Config::default();
        assert_eq!(c.word_min_for_volume("01"), 2500);
    }

    // ───────────────────────── 通用化回归：书档案（新书不被《残响之河》硬编码干扰）

    /// 无 book_profile 时，全部访问器回落历史常量（《残响之河》行为不变）。
    #[test]
    fn empty_profile_falls_back_to_canxiang_defaults() {
        let bp = BookProfile::default();
        assert_eq!(bp.title(), "残响之河");
        assert_eq!(bp.protagonist(), "陈寂");
        assert_eq!(bp.novel_id(), "canxiang");
        assert!(bp.banned_words().contains(&"归墟".to_string()));
        assert!(bp.horror_keywords().contains(&"残响".to_string()));
        assert!(bp.myth_banned().contains(&"存在之基".to_string()));
        let fam = bp.canon_family();
        assert!(fam.iter().any(|(t, c)| t == "父亲" && c == "陈大山"));
    }

    /// 给定 book_profile 时，访问器返回本书值，绝不泄漏《残响之河》专属值。
    #[test]
    fn custom_profile_overrides_all_book_values() {
        let bp: BookProfile = serde_json::from_value(serde_json::json!({
            "title": "星尘渡口",
            "protagonist": "林知远",
            "novel_id": "xingchen",
            "banned_words": ["灵气", "丹田"],
            "horror_keywords": ["异象"],
            "myth_banned": ["星神本源"],
            "canon_family": [["父亲", "林崇山"]],
            "canon_names": ["林崇山"]
        }))
        .unwrap();
        assert_eq!(bp.title(), "星尘渡口");
        assert_eq!(bp.protagonist(), "林知远");
        assert_eq!(bp.novel_id(), "xingchen");
        assert_eq!(bp.banned_words(), vec!["灵气", "丹田"]);
        // 未提供的字段回落历史常量（不算泄漏：只有显式提供才覆盖）
        assert!(bp.ability_keywords().contains(&"修为".to_string()));
        let fam = bp.canon_family();
        assert_eq!(fam.len(), 1);
        assert_eq!(fam[0], ("父亲".to_string(), "林崇山".to_string()));
        assert!(!bp.canon_names().contains(&"陈大山".to_string()));
    }

    /// 从磁盘合并 book_profile.json（优先级高于内嵌键）。
    #[test]
    fn load_reads_book_profile_file() {
        let d = std::env::temp_dir().join(format!("bp_test_{}", std::process::id()));
        let dd = d.join("05_管线框架/data");
        std::fs::create_dir_all(&dd).unwrap();
        std::fs::write(
            dd.join("book_profile.json"),
            r#"{"title":"测试书","protagonist":"张三","novel_id":"test"}"#,
        )
        .unwrap();
        let c = load(&d).unwrap();
        assert_eq!(c.book_profile.title(), "测试书");
        assert_eq!(c.book_profile.protagonist(), "张三");
        assert_eq!(c.book_profile.novel_id(), "test");
        let _ = std::fs::remove_dir_all(&d);
    }
}
