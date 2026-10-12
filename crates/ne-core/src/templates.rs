//! 卡模板注册表 —— 全库 **26 类**的**唯一事实源**。
//!
//! # 三套权威源的合并（2026-10-13 定稿）
//!
//! 历史上字段/类定义有三个来源，互相冲突：
//!
//! | 源 | 位置 | 类数 | 缺 |
//! |---|---|---|---|
//! | A 本模块 | `crates/ne-core/src/templates.rs` | 25 | 缺「系统规则」 |
//! | B JSON | `_元数据/_规范/_templates_all.json` | 19 | 缺 物品/伏笔/系统规则/关系/跨文明关系/世界观/技艺/建筑/历史/修炼体系 |
//! | C 编号规范 | `docs/specs/编号规范-v3.md` | 23（仅定前缀） | — |
//!
//! **并集 = 26 类**（A 的 25 + C 独有 `系统规则`）。本模块即 26 类全表。
//! 前缀统一按 v3（`CIVI/EVNT/PILL/LOCA/POWR/CLTR/…`），旧前缀列入
//! `code_prefix_aliases` 供识别历史编号。
//!
//! # v3 关键：前缀全局唯一
//!
//! v2 的痛点是**同一前缀被多类共用**（TECH=功法/力量体系/修炼体系/科技；
//! ART=物品/装备/天材地宝；CAS=伏笔/因果线；ORG=文明/势力；CULT=文明/文化）。
//! v3 给每类独立前缀，本表断言该唯一性。
//!
//! # 与磁盘/DB 的关系
//!
//! DB `materials` 现有 18 类（部分类尚未建卡）。本表 26 类含 `may_be_empty=true`
//! 的未建类，供建卡器按需生成骨架。

use serde::{Deserialize, Serialize};

/// 字段出现位置（对齐实测的四种版式）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    /// H1 之下的一行引用块：`> 胶囊：...`
    Capsule,
    /// `## 段名` 二级标题段
    Section,
    /// `### 段名` 三级标题（角色类的细目多用此）
    SubSection,
    /// 卡内任意位置的 `**字段名**：值`
    Bold,
    /// 表格行 `| 字段 | 值 |`
    TableRow,
}

impl FieldKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            FieldKind::Capsule => "capsule",
            FieldKind::Section => "section",
            FieldKind::SubSection => "subsection",
            FieldKind::Bold => "bold",
            FieldKind::TableRow => "table_row",
        }
    }
}

/// 一个字段定义。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Field {
    /// 规范名（新卡应使用此名）
    pub name: String,
    pub kind: FieldKind,
    pub required: bool,
    /// 同义异名 —— 存量卡用这些名字也算命中（校验器双写识别）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

impl Field {
    fn new(name: &str, kind: FieldKind, required: bool) -> Self {
        Field { name: name.into(), kind, required, aliases: Vec::new(), note: String::new() }
    }
    fn alias(mut self, a: &str) -> Self {
        self.aliases.push(a.into());
        self
    }
    fn note(mut self, n: &str) -> Self {
        self.note = n.into();
        self
    }
    /// 该字段是否由给定名命中（含别名）。
    pub fn matches(&self, got: &str) -> bool {
        let g = got.trim().trim_start_matches(['【', '[']).trim_end_matches(['】', ']']).trim();
        g == self.name || self.aliases.iter().any(|a| a == g)
    }
}

/// 一个卡片类别（对应 `02_资料库/<目录>/`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardTemplate {
    /// 类别名（目录名）
    pub category: String,
    /// 编号前缀（v3，全局唯一）
    pub code_prefix: String,
    /// 历史前缀（v3 迁移前），供识别旧编号
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub code_prefix_aliases: Vec<String>,
    /// H1 格式：`{code}｜{name}`
    pub h1_pattern: String,
    pub fields: Vec<Field>,
    /// 该类别卡应归属的目录（用于检出跨类混放）
    pub home_dir: String,
    /// 是否允许在存量库中缺席（尚未建卡）
    #[serde(default)]
    pub may_be_empty: bool,
}

impl CardTemplate {
    pub fn required_fields(&self) -> impl Iterator<Item = &Field> {
        self.fields.iter().filter(|f| f.required)
    }
}

/// 26 类全表。
pub fn all_templates() -> Vec<CardTemplate> {
    use FieldKind::*;
    let t = |category: &str, prefix: &str, aliases: &[&str], may_be_empty: bool, fields: Vec<Field>| {
        CardTemplate {
            category: category.into(),
            code_prefix: prefix.into(),
            code_prefix_aliases: aliases.iter().map(|s| s.to_string()).collect(),
            h1_pattern: "{code}｜{name}".into(),
            fields,
            home_dir: category.into(),
            may_be_empty,
        }
    };
    let cap = || Field::new("胶囊", Capsule, true).alias("介绍");
    let rel = || Field::new("关联", Section, true).alias("关联标注");

    vec![
        // ─────────── 角色（238 张）
        t("角色", "CHAR", &[], false, vec![
            Field::new("介绍", Capsule, true).alias("胶囊").note("40–70 字一句话"),
            Field::new("详细信息", Section, true).note("容器段，其下用 H3 展开"),
            Field::new("基础信息", SubSection, true).alias("基本信息"),
            Field::new("外貌", SubSection, false).alias("外貌特征"),
            Field::new("性格", SubSection, true),
            Field::new("核心动机", SubSection, true),
            Field::new("人物弧线", SubSection, true).alias("成长轨迹"),
            Field::new("标志性台词", SubSection, true),
            Field::new("历史", Section, true),
            Field::new("时间", Section, true),
            Field::new("背景", Section, true).alias("背景故事"),
            rel(),
        ]),
        // ─────────── 事件（98 张）
        t("事件", "EVNT", &["EVT"], false, vec![
            Field::new("事件名", Section, true).alias("名称"),
            Field::new("编号", Section, false),
            Field::new("时间", Section, true).note("太玄历锚点；禁自造纪元"),
            Field::new("涉及文明", Section, true),
            Field::new("因果链", Section, true).alias("因果链路"),
            Field::new("关联标注", Section, true).alias("关联"),
        ]),
        // ─────────── 时间（9 张）
        t("时间", "TIME", &[], false, vec![
            cap(),
            Field::new("主线阶段", Section, true).alias("阶段"),
            Field::new("关键事件", Section, true),
            Field::new("关联标注", Section, true).alias("关联"),
        ]),
        // ─────────── 地点（36 张）
        t("地点", "LOCA", &["LOC"], false, vec![
            cap(),
            Field::new("地名", Section, true).alias("名称"),
            Field::new("编号", Section, false),
            Field::new("所在文明/区域", Section, true).alias("所在文明-区域"),
            Field::new("空间层级", Section, true).note("∈{宇宙/界域/星球/区域/城市/秘境}"),
            Field::new("生态", Section, false).alias("描述"),
            Field::new("纪年挂靠", Section, false),
            Field::new("关联", Section, true).alias("关联标注"),
        ]),
        // ─────────── 文明（35 张）
        t("文明", "CIVI", &["CIV", "ORG"], false, vec![
            cap().alias("定位"),
            Field::new("文明名", Bold, true),
            Field::new("类型", Bold, true).note("∈{正名,次级,创造A,创造B,创造C}"),
            Field::new("谱系", Bold, true).alias("文明谱系"),
            Field::new("等级", Bold, true).note("S/A/B 单字母"),
            Field::new("要素清单", Section, false),
            Field::new("关联标注", Section, true).alias("关联"),
        ]),
        // ─────────── 跨文明关系（1 张）
        t("跨文明关系", "XREA", &["CULT"], false, vec![
            cap(),
            Field::new("A文明", Bold, true).alias("A 文明"),
            Field::new("B文明", Bold, true).alias("B 文明"),
            Field::new("外交", Bold, true),
            Field::new("贸易", Bold, false),
            Field::new("军事", Bold, false),
            Field::new("文化", Bold, false),
            rel(),
        ]),
        // ─────────── 系统规则（14 张；制度/规则类）
        t("系统规则", "SYSX", &["SYS"], false, vec![
            cap(),
            Field::new("规则名", Section, true).alias("制度名").alias("名称"),
            Field::new("所属文明", Section, true).alias("归属文明"),
            Field::new("核心条款", Section, true).alias("核心内容").alias("内容"),
            Field::new("约束范围", Section, false),
            rel(),
        ]),
        // ─────────── 势力（1 张）
        t("势力", "FACT", &["ORG"], false, vec![
            cap(),
            Field::new("目标", Section, true).note("≥1 条可检验，禁抽象口号"),
            Field::new("组织结构", Section, true),
            Field::new("立场", Section, true),
            Field::new("成员", Section, false),
            Field::new("资源", Section, false),
            Field::new("关联标注", Section, true).alias("关联"),
        ]),
        // ─────────── 功法（1 张）
        t("功法", "ARTS", &["TECH"], false, vec![
            cap(),
            Field::new("功法名", Section, true).alias("名称"),
            Field::new("所属体系", Section, true),
            Field::new("修炼条件", Section, true),
            Field::new("境界等级体系", Section, true),
            Field::new("核心口诀", Section, false),
            Field::new("传承谱系", Section, false),
            Field::new("功法效果", Section, true),
            Field::new("修炼风险", Section, false),
            Field::new("克制关系", Section, false),
            Field::new("关联标注", Section, true).alias("关联"),
        ]),
        // ─────────── 力量体系（21 张）
        t("力量体系", "POWR", &["TECH"], false, vec![
            cap(),
            Field::new("体系名", Bold, true).alias("力量体系"),
            Field::new("归属文明", Bold, true),
            Field::new("境界分层", Section, true).alias("层次划分"),
            Field::new("对照关系", Section, true).note("≥1 处跨体系≈锚点"),
            Field::new("关联", Section, true).alias("关联标注"),
        ]),
        // ─────────── 修炼体系（未建卡）
        t("修炼体系", "PRAC", &["TECH"], true, vec![
            cap(),
            Field::new("体系名", Section, true),
            Field::new("基础理论", Section, true),
            Field::new("层次划分", Section, true),
            Field::new("关联标注", Section, true).alias("关联"),
        ]),
        // ─────────── 丹药（60 张）
        t("丹药", "PILL", &["MTC"], false, vec![
            cap(),
            Field::new("丹名", Section, true).alias("名称"),
            Field::new("稀缺度", Section, true).alias("品阶")
                .note("∈{常见,稀缺,稀有,极稀有,绝品}"),
            Field::new("用途", Section, true),
            Field::new("炼制要点", Section, true).alias("制备要点"),
            Field::new("稀缺度说明", Section, false),
            Field::new("关联标注", Section, true).alias("关联"),
        ]),
        // ─────────── 物品（13 张）
        t("物品", "ITEM", &["ART"], false, vec![
            cap(),
            Field::new("物名", Section, true).alias("名称"),
            Field::new("形态", Section, true).alias("外观"),
            Field::new("效果", Section, true),
            Field::new("产地", Section, false).alias("来源"),
            Field::new("关联", Section, true).alias("关联标注"),
        ]),
        // ─────────── 装备（4 张）
        t("装备", "GEAR", &["ART"], false, vec![
            cap(),
            Field::new("装备名", Section, true).alias("名称"),
            Field::new("材质", Section, true),
            Field::new("功能", Section, true),
            Field::new("持有者", Section, false),
            Field::new("关联标注", Section, true).alias("关联"),
        ]),
        // ─────────── 天材地宝（未建独立卡，混在物品 ART）
        t("天材地宝", "MTRL", &["ART"], true, vec![
            cap(),
            Field::new("名称", Section, true),
            Field::new("品阶", Section, true),
            Field::new("生长环境", Section, true).alias("产地"),
            Field::new("采集方式", Section, false),
            Field::new("药用价值", Section, true).alias("效果"),
            Field::new("关联标注", Section, true).alias("关联"),
        ]),
        // ─────────── 伏笔（22 张）
        t("伏笔", "FORE", &["CAS"], false, vec![
            cap(),
            Field::new("起点", Section, true).note("埋设章"),
            Field::new("终点", Section, true).note("兑现章"),
            Field::new("因果链", Section, true).alias("因果链路"),
            Field::new("关联", Section, true).alias("关联标注"),
        ]),
        // ─────────── 因果线（未建独立卡）
        t("因果线", "CAUS", &["CAS"], true, vec![
            cap(),
            Field::new("起因", Section, true),
            Field::new("经过", Section, true),
            Field::new("结果", Section, true),
            Field::new("关联标注", Section, true).alias("关联"),
        ]),
        // ─────────── 文化（35 张）
        t("文化", "CLTR", &["CULT"], false, vec![
            cap(),
            Field::new("所属文明", Section, true).alias("文明名").alias("归属文明"),
            Field::new("文化维度", Section, true).alias("维度")
                .note("∈{信仰,习俗,艺术,哲学,货币,语言,礼仪}"),
            Field::new("核心内容", Section, true),
            Field::new("禁忌", Section, false),
            Field::new("对外输出", Section, false),
            Field::new("关联标注", Section, true).alias("关联").alias("【关联标注】"),
        ]),
        // ─────────── 关系（2 张）
        t("关系", "RELA", &["REL"], false, vec![
            cap(),
            Field::new("关系双方", Section, true).alias("双方"),
            Field::new("关系性质", Section, true).alias("类型"),
            Field::new("演变", Section, false),
            Field::new("强度", Section, false),
            Field::new("时间", Section, false),
            Field::new("关联标注", Section, true).alias("关联"),
        ]),
        // ─────────── 世界观（未建独立卡）
        t("世界观", "WRLD", &["WORLD"], true, vec![
            cap(),
            Field::new("核心法则", Section, true),
            Field::new("边界", Section, true),
            Field::new("关联标注", Section, true).alias("关联"),
        ]),
        // ─────────── 境界（未建独立卡）
        t("境界", "REAL", &["REALM"], true, vec![
            cap(),
            Field::new("境界名", Section, true),
            Field::new("所属体系", Section, true),
            Field::new("晋级条件", Section, true),
            Field::new("能力特征", Section, true),
            Field::new("关联标注", Section, true).alias("关联"),
        ]),
        // ─────────── 历史（未建独立卡）
        t("历史", "HIST", &[], true, vec![
            cap(),
            Field::new("时期", Section, true),
            Field::new("重大事件", Section, true),
            Field::new("关联标注", Section, true).alias("关联"),
        ]),
        // ─────────── 建筑（未建独立卡）
        t("建筑", "BLDG", &[], true, vec![
            cap(),
            Field::new("建筑名", Section, true),
            Field::new("所在地", Section, true),
            Field::new("功能", Section, true),
            Field::new("外观", Section, false),
            Field::new("关联标注", Section, true).alias("关联"),
        ]),
        // ─────────── 技艺（未建独立卡）
        t("技艺", "CRAFT", &[], true, vec![
            cap(),
            Field::new("技艺名", Section, true),
            Field::new("所属文明", Section, true),
            Field::new("技术要点", Section, true),
            Field::new("传承方式", Section, false),
            Field::new("关联标注", Section, true).alias("关联"),
        ]),
        // ─────────── 诡异（14 张）
        t("诡异", "WEIR", &["ANC"], false, vec![
            cap(),
            Field::new("名称", Section, true),
            Field::new("特性", Section, true),
            Field::new("克制", Section, true),
            Field::new("出现条件", Section, true),
            Field::new("关联标注", Section, true).alias("关联"),
        ]),
        // ─────────── 科技（12 张；前缀 TECH 独占，v3 唯一）
        t("科技", "TECH", &[], false, vec![
            cap(),
            Field::new("技术名", Section, true).alias("名称"),
            Field::new("技术原理", Section, true),
            Field::new("能量来源", Section, false),
            Field::new("副作用", Section, false),
            Field::new("要素清单", Section, false),
            Field::new("关联标注", Section, true).alias("关联"),
        ]),
    ]
}

/// 按类别名取模板。
pub fn template_of(category: &str) -> Option<CardTemplate> {
    let want = normalize_category(category);
    all_templates().into_iter().find(|t| t.category == want)
}

/// 类别名归一（把历史异名映射到当前规范名）。
pub fn normalize_category(c: &str) -> String {
    match c.trim() {
        "体系力量" => "力量体系".into(),
        "地图" => "地点".into(),          // v3：地点（LOCA）
        "跨文明" => "跨文明关系".into(),
        "因果线" => "因果线".into(),
        other => other.to_string(),
    }
}

/// 全部类名。
pub fn all_categories() -> Vec<String> {
    all_templates().into_iter().map(|t| t.category).collect()
}

/// 由编号推断类别（CHAR-99-001 → 角色）。
///
/// v3 下**前缀全局唯一**，故无歧义；历史前缀经 `code_prefix_aliases` 兜底。
pub fn category_of_code(code: &str) -> Option<String> {
    let p = code.split('-').next()?;
    all_templates()
        .into_iter()
        .find(|t| t.code_prefix == p || t.code_prefix_aliases.iter().any(|a| a == p))
        .map(|t| t.category)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_26_classes() {
        assert_eq!(all_categories().len(), 26, "应为 26 类：{:?}", all_categories());
    }

    #[test]
    fn all_source_categories_present() {
        // A（本模块原始 25 类）+ C（编号规范 v3 独有的 系统规则）
        for c in ["角色","事件","时间","地图","文明","跨文明关系","势力","功法","力量体系",
                  "丹药","物品","装备","科技","伏笔","文化","关系","因果线","世界观","境界",
                  "修炼体系","历史","建筑","技艺","天材地宝","诡异","系统规则"] {
            let n = normalize_category(c);
            assert!(template_of(&n).is_some(), "缺模板：{c}");
        }
    }

    #[test]
    fn prefixes_are_unique_v3() {
        let mut seen = std::collections::HashSet::new();
        for t in all_templates() {
            assert!(seen.insert(t.code_prefix.clone()), "前缀重复：{}", t.code_prefix);
        }
    }

    #[test]
    fn normalize_synonyms() {
        assert_eq!(normalize_category("体系力量"), "力量体系");
        assert_eq!(normalize_category("地图"), "地点");
        assert_eq!(normalize_category("角色"), "角色");
    }

    #[test]
    fn field_matches_aliases() {
        let t = template_of("事件").unwrap();
        let f = t.fields.iter().find(|f| f.name == "关联标注").unwrap();
        assert!(f.matches("关联标注"));
        assert!(f.matches("关联"));
        assert!(f.matches("【关联标注】"));
        assert!(!f.matches("无关字段"));
    }

    #[test]
    fn every_template_has_required() {
        for t in all_templates() {
            assert!(!t.fields.is_empty(), "{} 无字段", t.category);
            assert!(t.required_fields().count() >= 1, "{} 无必填字段", t.category);
        }
    }

    #[test]
    fn category_of_code_v3_and_alias() {
        assert_eq!(category_of_code("CHAR-99-001").as_deref(), Some("角色"));
        assert_eq!(category_of_code("EVNT-99-005").as_deref(), Some("事件"));
        assert_eq!(category_of_code("EVT-99-005").as_deref(), Some("事件"));
        assert_eq!(category_of_code("PILL-99-001").as_deref(), Some("丹药"));
        assert_eq!(category_of_code("LOCA-99-020").as_deref(), Some("地点"));
        assert_eq!(category_of_code("WEIR-99-001").as_deref(), Some("诡异"));
        assert_eq!(category_of_code("SYSX-99-001").as_deref(), Some("系统规则"));
        assert_eq!(category_of_code("CIVI-99-001").as_deref(), Some("文明"));
    }

    #[test]
    fn serde_roundtrip() {
        let t = template_of("角色").unwrap();
        let j = serde_json::to_string(&t).unwrap();
        let back: CardTemplate = serde_json::from_str(&j).unwrap();
        assert_eq!(back.category, "角色");
        assert_eq!(back.fields.len(), t.fields.len());
    }
}