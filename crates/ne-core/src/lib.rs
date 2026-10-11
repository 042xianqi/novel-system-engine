//! ne-core：novel-engine 核心数据模型 + funnel.yaml 解析 + 细纲章解析。
//!
//! 定位：整条管线的"数据结构层"。不碰 LLM、不碰文件 IO 的复杂部分，
//! 只定义 注入包 / 章细纲 / 卡引用 / 漏斗层 这几个贯穿 M1~M3 的类型，
//! 让 ne-injection / ne-cardgen / ne-qc / ne-db 都有共同语言。

use serde::{Deserialize, Serialize};

pub mod templates;
use std::path::PathBuf;

/// 漏斗层（对应 funnel.yaml 的 L0~L5）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Layer {
    L0,
    L1,
    L1x,
    L2,
    L3,
    L4,
    L5,
}

impl std::fmt::Display for Layer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Layer::L0 => "L0",
            Layer::L1 => "L1",
            Layer::L1x => "L1x",
            Layer::L2 => "L2",
            Layer::L3 => "L3",
            Layer::L4 => "L4",
            Layer::L5 => "L5",
        };
        write!(f, "{s}")
    }
}

/// funnel.yaml 里的一层 stage。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stage {
    pub id: String,
    pub name: Option<String>,
    pub file: Option<String>,
    pub dir: Option<String>,
    pub status: String,
    #[serde(default)]
    pub deps: Vec<String>,
}

/// funnel.yaml 全文（我们只取最关心的字段，其余 serde 忽略）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Funnel {
    #[serde(default)]
    pub root: String,
    #[serde(default)]
    pub layers: Vec<Stage>,
}

impl Funnel {
    /// 从 yaml 文本解析。
    pub fn parse(text: &str) -> Result<Self, anyhow::Error> {
        Ok(serde_yaml::from_str(text)?)
    }

    /// 找到当前待推进的 stage（status 纯 "done" 才算完成；章节态 chapters_* 表示还有后续章，属 pending）。
    pub fn pending(&self) -> Vec<&Stage> {
        self.layers
            .iter()
            .filter(|s| s.status != "done")
            .collect()
    }

    /// 按 L 层取某一 stage。
    pub fn stage_of(&self, layer: Layer) -> Option<&Stage> {
        self.layers.iter().find(|s| s.id == layer.to_string())
    }
}

/// 场景类型（8 类，对应 chapter_engine scene_dims）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SceneType {
    Combat,
    Cultivation,
    Social,
    BigScene,
    Psychology,
    Explore,
    Transition,
    #[default]
    Daily,
}

/// 一张细纲章（`### 第N章 标题` 段解析结果）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChapterOutline {
    pub number: u32,
    pub title: String,
    /// 概要正文（目标/在场/阻碍/选择/钩子 + 决定→代价→变化 + 锚点）。
    pub outline: String,
    /// 场景类型（从细纲锚点推断，推断不出则 Daily）。
    pub scene_type: SceneType,
    /// 细纲里引用的卡编号（CHAR-xx / EVT-xx / LOC-xx ...）。
    pub card_refs: Vec<String>,
    /// 出现的人物名（在"在场："行里）。
    pub characters: Vec<String>,
    /// 锚点键值（引典 / 文明状态 / 情绪曲线 等）。
    pub anchors: serde_json::Value,
}

/// 注入包（M1 的产出）：一章生成正文所需的全部动态数据。
/// 对应 chapter_engine.build_prompt 里注入的 7 大类。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InjectionPacket {
    pub chapter: ChapterOutline,
    /// 出场角色性格卡（查角色类）。
    pub chars: Vec<CharacterSnapshot>,
    /// 关联资料卡全文/摘要（查 14 类，空类如实置空 + missing_refs）。
    pub materials: Vec<MaterialRef>,
    /// 前文末尾 ~500 字（查 04_正文 上一章）。
    pub prev_tail: String,
    /// 伏笔（必须兑现 + 可埋设）。
    pub foreshadow_due: Vec<String>,
    pub foreshadow_plant: Vec<String>,
    /// 事件状态。
    pub events: Vec<EventRef>,
    /// 库中缺失的引用（空类查不到 → 如实列，供建卡 + 提示 LLM 别杜撰）。
    pub missing_refs: Vec<String>,
    /// 禁词表（写前注入第 5 类：细纲+资料+前章摘要+状态+禁词）。
    #[serde(default)]
    pub banned_words: Vec<String>,
    /// 两层选卡后的预算报告（default 表示未装配）。
    #[serde(default)]
    pub budget: BudgetReport,
    /// 世界推演（文明相位/熵债/残响衰减）。M1 先占位，M3 填公式。
    pub world: serde_json::Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CharacterSnapshot {
    pub name: String,
    pub code: String,
    pub realm: String,
    pub location: String,
    pub status: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MaterialRef {
    pub code: String,
    pub name: String,
    /// 全文层内容（截到 FULL_CARD_TOKEN_CAP；摘要层为空）。
    pub content: String,
    /// 3 行摘要字段（摘要层注入用；确定性截断，非 LLM 现场生成）。
    #[serde(default)]
    pub summary: String,
    pub is_full: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EventRef {
    pub id: String,
    pub name: String,
    pub status: String,
}

// ==== 2026-10-08 口径：20 类卡片 + 两层选卡 + 预算（spec: novel-pipeline）====

/// 20 类资料卡权威清单（编号前缀待真卡扫描后回填，暂以类别名对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CardClass {
    Character,    // 角色
    Faction,      // 势力
    Location,     // 地点
    Item,         // 物品
    Culture,      // 文化
    Skill,        // 技艺
    Medicine,     // 丹药
    Tech,         // 科技
    Map,          // 地图
    Event,        // 事件
    History,      // 历史
    Timeline,     // 时间
    Building,     // 建筑
    Equipment,    // 装备
    Gongfa,       // 功法
    Cultivation,  // 修炼体系
    Realm,        // 境界
    PowerSystem,  // 力量体系
    Civilization, // 文明
    Foreshadow,   // 伏笔
}

impl CardClass {
    pub const ALL: [CardClass; 20] = [
        CardClass::Character,
        CardClass::Faction,
        CardClass::Location,
        CardClass::Item,
        CardClass::Culture,
        CardClass::Skill,
        CardClass::Medicine,
        CardClass::Tech,
        CardClass::Map,
        CardClass::Event,
        CardClass::History,
        CardClass::Timeline,
        CardClass::Building,
        CardClass::Equipment,
        CardClass::Gongfa,
        CardClass::Cultivation,
        CardClass::Realm,
        CardClass::PowerSystem,
        CardClass::Civilization,
        CardClass::Foreshadow,
    ];

    pub fn name(&self) -> &'static str {
        match self {
            CardClass::Character => "角色",
            CardClass::Faction => "势力",
            CardClass::Location => "地点",
            CardClass::Item => "物品",
            CardClass::Culture => "文化",
            CardClass::Skill => "技艺",
            CardClass::Medicine => "丹药",
            CardClass::Tech => "科技",
            CardClass::Map => "地图",
            CardClass::Event => "事件",
            CardClass::History => "历史",
            CardClass::Timeline => "时间",
            CardClass::Building => "建筑",
            CardClass::Equipment => "装备",
            CardClass::Gongfa => "功法",
            CardClass::Cultivation => "修炼体系",
            CardClass::Realm => "境界",
            CardClass::PowerSystem => "力量体系",
            CardClass::Civilization => "文明",
            CardClass::Foreshadow => "伏笔",
        }
    }
}

/// L5 上下文预算常量：全文 3 张 ×1500 + 摘要 5 张 ×200 ≈ 5500，总预算 20000。
pub const FULL_CARD_COUNT: usize = 3;
pub const SUMMARY_CARD_COUNT: usize = 5;
pub const FULL_CARD_TOKEN_CAP: usize = 1500;
pub const SUMMARY_TOKEN_CAP: usize = 200;
pub const TOTAL_TOKEN_BUDGET: usize = 20000;

/// 粗估 token：CJK≈1/字，ASCII≈4 字 1 token（做预算闸门够用，非精确分词）。
pub fn estimate_tokens(s: &str) -> usize {
    let (cjk, ascii) = s
        .chars()
        .fold((0usize, 0usize), |(c, a), ch| {
            if ch.is_ascii() { (c, a + 1) } else { (c + 1, a) }
        });
    cjk + (ascii + 3) / 4
}

/// 贪心截断到 token 上限（按字符累计，CJK 1/字、ASCII 4 字 1）。
pub fn truncate_tokens(s: &str, cap: usize) -> String {
    let mut tokens = 0.0f32;
    let mut out = String::new();
    for ch in s.chars() {
        let cost = if ch.is_ascii() { 0.25 } else { 1.0 };
        if tokens + cost > cap as f32 {
            break;
        }
        tokens += cost;
        out.push(ch);
    }
    out
}

/// 确定性选卡数据源：一张卡的用量统计（ne-db card_usage / foreshadow 汇出）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageStat {
    pub code: String,
    pub ref_count: u32,
    pub last_chapter: u32,
    pub foreshadow_due: bool,
}

/// 注入包预算报告（两层选卡后回填）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BudgetReport {
    pub full_count: usize,
    pub summary_count: usize,
    pub card_tokens: usize,
    pub total_est_tokens: usize,
    pub total_budget: usize,
}

/// 两层选卡·第 1 层：确定性排序（0 LLM）。
/// 打分：细纲命中 > 伏笔待收 > 被引次数 > 章距近；返回 (全文 top3, 摘要 top5)。
/// 第 2 层（LLM 在 8 卡候选池内微调排序/摘要-全文边界）由调用方接缝完成。
pub fn rank_cards(
    outline_refs: &[String],
    usage: &[UsageStat],
    current_chapter: u32,
) -> (Vec<String>, Vec<String>) {
    let mut codes: Vec<String> = outline_refs.to_vec();
    for u in usage {
        // 只纳入「确曾在已生成章节用过」（last_chapter > 0）或「伏笔待收」的卡。
        //
        // last_chapter == 0 表示从未在任何已生成章节出现——该行多半是从
        // `materials.refs/ref_by` 静态回填的图权重（见 ne-db::backfill_card_usage_from_legacy），
        // **不代表真实用量**。若照单全收，第 1 章会被全库 ref_count 最高的卡挤满，
        // 注入后期剧情（实测：第1章注入包混入 EVT-141「第七次共振·裂河」、
        // CAS-014「残响归流」等终局卡）→ 剧透穿帮。
        if (u.last_chapter > 0 || u.foreshadow_due) && !codes.contains(&u.code) {
            codes.push(u.code.clone());
        }
    }
    let mut scored: Vec<(i64, String)> = codes
        .into_iter()
        .map(|code| {
            let hit = if outline_refs.iter().any(|r| r == &code) {
                10_000
            } else {
                0
            };
            let (refs, dist, fs) = match usage.iter().find(|u| u.code == code) {
                Some(u) => (
                    u.ref_count as i64 * 10,
                    -(current_chapter.saturating_sub(u.last_chapter).min(999) as i64),
                    if u.foreshadow_due { 5_000 } else { 0 },
                ),
                None => (0, 0, 0),
            };
            (hit + fs + refs + dist, code)
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let full = scored
        .iter()
        .take(FULL_CARD_COUNT)
        .map(|(_, c)| c.clone())
        .collect();
    let summary = scored
        .iter()
        .skip(FULL_CARD_COUNT)
        .take(SUMMARY_CARD_COUNT)
        .map(|(_, c)| c.clone())
        .collect();
    (full, summary)
}

/// 资料库根路径（02_资料库 所在）。
pub fn data_lib_root(root: &str) -> PathBuf {
    PathBuf::from(root).join("02_资料库")
}

/// 细纲目录。
pub fn outline_dir(root: &str) -> PathBuf {
    PathBuf::from(root).join("03_叙事规划").join("细纲")
}

/// 从细纲文本切出第 N 章。细纲章格式：`### 第N章 标题` + 概要，
/// 章间用 `### 第` 或文件尾分界（对应护栏 §三 的 daily_write 正则）。
/// 找不到返回 None（M1 验收点：能切实在的章）。
pub fn extract_chapter(outline_text: &str, number: u32) -> Option<ChapterOutline> {
    // 按 `### 第N章 标题` 切块，找目标号（captures_iter + get，避免 Match 切片）。
    let re = regex::Regex::new(r"###\s*第(\d+)章[ \t]*(.*)").ok()?;
    let caps: Vec<regex::Captures> = re.captures_iter(outline_text).collect();
    for (i, c) in caps.iter().enumerate() {
        let num_str = c.get(1).map(|m| m.as_str().trim()).unwrap_or("");
        let num: u32 = match num_str.parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        if num != number {
            continue;
        }
        let start = c.get(0).unwrap().start();
        let end = caps
            .get(i + 1)
            .map(|nxt| nxt.get(0).unwrap().start())
            .unwrap_or(outline_text.len());
        let block = &outline_text[start..end];
        let title = c.get(2).map(|m| m.as_str().trim().to_string()).unwrap_or_default();
        let mut co = ChapterOutline::default();
        co.number = number;
        co.title = title;
        co.outline = block.to_string();
        co.card_refs = find_card_refs(block);
        co.characters = find_in_chapter(block);
        return Some(co);
    }
    None
}

/// 抓卡编号（CHAR-/EVT-/LOC-/REL-/ART-/ORG-/TECH-/CAS- 等，前缀-数字-数字）。
pub fn find_card_refs(text: &str) -> Vec<String> {
    let re = regex::Regex::new(
        r"\b((?:CHAR|EVT|LOC|REL|ART|ORG|TECH|SYS|CULT|MTC|CAS|CAUSE|TIME|ANC|YGEN|OTHER)-\d+(-\d+)?)",
    )
    .unwrap();
    let mut seen: Vec<String> = re
        .captures_iter(text)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
        .collect();
    seen.sort();
    seen.dedup();
    seen
}

/// 抓"在场："行里的人物名（顿号/逗号/、分隔，去掉卡编号）。
pub fn find_in_chapter(text: &str) -> Vec<String> {
    let re = regex::Regex::new(r"在场[：:].*?([^\n]+)").unwrap();
    let mut out = Vec::new();
    for cap in re.captures_iter(text) {
        let line = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        for name in line.split(['、', '，', ',', '（', '(']).map(|s| s.trim()) {
            let name = name.split('（').next().unwrap_or(name).trim().to_string();
            if !name.is_empty() && name.chars().count() <= 12 && !name.starts_with('[') {
                out.push(name);
            }
        }
    }
    out.dedup();
    out
}

// 让 regex 依赖不报 unused（extract 用到）。
#[allow(dead_code)]
fn _regex_guard() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rank_cards_prefers_hit_foreshadow_refs() {
        let refs = vec!["EVT-99-005".to_string()];
        let usage = vec![
            UsageStat { code: "REL-99-004".into(), ref_count: 9, last_chapter: 1, foreshadow_due: false },
            UsageStat { code: "ART-99-010".into(), ref_count: 1, last_chapter: 2, foreshadow_due: true },
            UsageStat { code: "SYS-99-001".into(), ref_count: 2, last_chapter: 3, foreshadow_due: false },
        ];
        let (full, summary) = rank_cards(&refs, &usage, 10);
        // 细纲命中优先第一
        assert_eq!(full[0], "EVT-99-005");
        // 伏笔待收 > 高被引
        assert_eq!(full[1], "ART-99-010");
        assert_eq!(full[2], "REL-99-004");
        // 第 4 名落摘要层
        assert_eq!(summary[0], "SYS-99-001");
    }

    #[test]
    fn rank_cards_ignores_never_used_cards() {
        // 回归：last_chapter==0 的卡（静态回填的图权重）不得进候选。
        // 否则第1章会被全库 ref_count 最高的后期卡挤满（实测混入 EVT-141 等终局卡）。
        let refs = vec!["EVT-99-005".to_string()];
        let usage = vec![
            // 从没在任何已生成章节出现过，仅图权重高
            UsageStat { code: "EVT-141".into(), ref_count: 7, last_chapter: 0, foreshadow_due: false },
            UsageStat { code: "CAS-014".into(), ref_count: 7, last_chapter: 0, foreshadow_due: false },
            // 真实用过
            UsageStat { code: "CHAR-99-001".into(), ref_count: 3, last_chapter: 3, foreshadow_due: false },
            // 伏笔待收：即便没用过也要注入
            UsageStat { code: "FORE-1".into(), ref_count: 0, last_chapter: 0, foreshadow_due: true },
        ];
        let (full, summary) = rank_cards(&refs, &usage, 1);
        let all: Vec<String> = full.iter().chain(summary.iter()).cloned().collect();
        assert!(!all.contains(&"EVT-141".to_string()), "未用过的卡不得进候选: {all:?}");
        assert!(!all.contains(&"CAS-014".to_string()), "未用过的卡不得进候选: {all:?}");
        assert!(all.contains(&"CHAR-99-001".to_string()), "用过的卡应保留: {all:?}");
        assert!(all.contains(&"FORE-1".to_string()), "伏笔待收应保留: {all:?}");
    }

    #[test]
    fn token_estimate_and_truncate() {
        let s = "测试文本abcd";
        assert_eq!(estimate_tokens(s), 4 + 1);
        let long: String = std::iter::repeat('测').take(3000).collect();
        assert_eq!(estimate_tokens(&truncate_tokens(&long, 100)), 100);
    }

    #[test]
    fn card_class_all_20() {
        assert_eq!(CardClass::ALL.len(), 20);
        assert_eq!(CardClass::Civilization.name(), "文明");
        assert_eq!(CardClass::PowerSystem.name(), "力量体系");
    }

    #[test]
    fn parse_funnel() {
        let yaml = r#"
root: /x/系统/资料库
layers:
  - id: L3
    name: 细纲
    status: done
    deps: [L2]
  - id: L5
    name: 正文
    status: chapters_1_to_1
    deps: [L3]
"#;
        let f = Funnel::parse(yaml).unwrap();
        assert_eq!(f.layers.len(), 2);
        assert!(f.stage_of(Layer::L3).is_some());
        // L5 status=chapters_* → 非纯 done，pending 逻辑：非 done 前缀 → pending
        assert!(f.pending().iter().any(|s| s.id == "L5"));
    }

    #[test]
    fn extract_chapter_and_refs() {
        let text = "### 第1章 涧底醒\n\n**概要**\n- 在场：陈寂（CHAR-99-001）\n- 阻碍：黑牛山北涧【EVT-99-005·地点】\n\n### 第2章 老屋夜\n- 在场：陈寂、陈老头\n- 引典：EVT-99-005\n";
        let ch = extract_chapter(text, 1).unwrap();
        assert_eq!(ch.title, "涧底醒");
        assert!(ch.card_refs.iter().any(|c| c == "EVT-99-005"));
        assert!(ch.card_refs.iter().any(|c| c == "CHAR-99-001"));
        assert!(ch.characters.iter().any(|c| c.contains("陈寂")));
    }
}