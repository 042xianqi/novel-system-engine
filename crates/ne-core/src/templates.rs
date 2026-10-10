//! 卡模板注册表 —— 全库 20 类的**唯一事实源**。
//!
//! # 背景：三套互相打架的权威源
//!
//! 全量精读 818 张卡后发现，字段定义存在**三个互不相同的来源**：
//!
//! | 源 | 位置 | 问题 |
//! |---|---|---|
//! | A 分类写作规范 | `_元数据/_规范/<类>/<类>.md`（含 spec-meta） | 后写的收口目标，存量卡多未遵守 |
//! | B OMP 模板 | `_元数据/_规范/_templates_all.json` | 定义 19 类，缺 6 类；声称"OMP 消费" |
//! | C AI 规则卡 | `_元数据/05_类建设进度/ai_directives/<类>_规则.md` | 未落地的提案，必填项与 A/B 均冲突 |
//!
//! 实测存量卡的字段命名（如 `关联` vs `关联标注`）在各类间不统一，
//! 同一类内也混用。本模块以 **A 规范为主体、B 补齐缺失、C 仅作参考**，
//! 合成 20 类模板，并显式记录每类的**同义字段别名**，供校验器双写识别。
//!
//! # 与 Python 版的差异
//!
//! 旧实现（OMP 侧）直接读 `_templates_all.json` —— 那是 **19 类且缺 6 类**，
//! 且**未收录 `伏笔`/`物品`**（这两类在文件系统里真实存在 22/11 张卡）。
//! 本模块把 20 类全部纳入，并标注了 `aliases`。

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
    /// 编号前缀（如 CHAR/EVT/LOC）
    pub code_prefix: String,
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

/// 20 类全表。
///
/// 来源标注：`A` = 分类规范，`B` = _templates_all.json，`C` = ai_directives 规则卡。
pub fn all_templates() -> Vec<CardTemplate> {
    use FieldKind::*;
    let t = |category: &str, code_prefix: &str, fields: Vec<Field>| CardTemplate {
        category: category.into(),
        code_prefix: code_prefix.into(),
        h1_pattern: "{code}｜{name}".into(),
        fields,
        home_dir: category.into(),
        may_be_empty: false,
    };

    vec![
        // ─────────── 角色（241 张；A+B 一致度最高）
        t("角色", "CHAR", vec![
            Field::new("介绍", Capsule, true).alias("胶囊").note("40–70 字一句话；实测中位 57 字"),
            Field::new("详细信息", Section, true).note("容器段，其下用 H3 展开"),
            Field::new("基础信息", SubSection, true).alias("基本信息"),
            Field::new("外貌", SubSection, false).alias("外貌特征"),
            Field::new("性格", SubSection, true),
            Field::new("核心动机", SubSection, true),
            Field::new("人物弧线", SubSection, true).alias("成长轨迹"),
            Field::new("标志性台词", SubSection, true),
            Field::new("历史", Section, true).note("骨架 100% 但实质内容仅 22% —— 存量普遍空心"),
            Field::new("时间", Section, true).note("实质 33%"),
            Field::new("背景", Section, true).alias("背景故事").note("实质 48%"),
            Field::new("关联", Section, true).alias("关联标注").note("实质 32%；子字段见关联维度"),
        ]),
        // ─────────── 事件（112 张，含 14 张 ANC 误类）
        t("事件", "EVT", vec![
            Field::new("胶囊", Capsule, true).note("实测仅 2/112 有 —— 最大缺口"),
            Field::new("事件名", Section, true),
            Field::new("编号", Section, true).note("须与 H1 码一致"),
            Field::new("时间", Section, true).note("太玄历锚点；禁自造纪元"),
            Field::new("涉及文明", Section, true),
            Field::new("因果链", Section, true).alias("因果链路").note("A 列可选、B 列必填；建议必填"),
            Field::new("关联标注", Section, true).alias("关联").note("A/C 用此名，B 用「关联」；实测 74% 用「关联」"),
        ]),
        // ─────────── 时间（9 张）
        t("时间", "TIME", vec![
            Field::new("胶囊", Capsule, false),
            Field::new("主线阶段", Section, true),
            Field::new("关键事件", Section, true),
            Field::new("关联标注", Section, true).alias("关联"),
        ]),
        // ─────────── 地图/地点（38 张）
        t("地图", "LOC", vec![
            Field::new("胶囊", Capsule, true),
            Field::new("地名", Section, true).note("非空、真实地名，禁模板变量"),
            Field::new("编号", Section, false).note("A 列可选、骨架有、B 无 —— 取骨架口径"),
            Field::new("所在文明/区域", Section, true).alias("所在文明-区域"),
            Field::new("空间层级", Section, true).note("∈{宇宙/界域/星球/区域/城市/秘境}"),
            Field::new("生态", Section, true),
            Field::new("纪年挂靠", Section, false),
            Field::new("关联", Section, true).alias("关联标注").note("契约§九格式：`- 类名: 纯编号`；实测 0/38 合规"),
        ]),
        // ─────────── 文明（65 张；含跨文明关系卡混放）
        t("文明", "ORG", vec![
            Field::new("胶囊", Capsule, true).alias("定位"),
            Field::new("文明名", Bold, true),
            Field::new("类型", Bold, true).note("∈{正名,次级,创造A,创造B,创造C}"),
            Field::new("谱系", Bold, true).alias("文明谱系"),
            Field::new("等级", Bold, true).note("S/A/B 单字母"),
            Field::new("要素清单", Section, false),
            Field::new("关联标注", Section, true).alias("关联"),
        ]),
        // ─────────── 跨文明关系（混在文明目录中）
        t("跨文明关系", "CULT", vec![
            Field::new("胶囊", Capsule, true),
            Field::new("A文明", Bold, true).alias("A 文明"),
            Field::new("B文明", Bold, true).alias("B 文明"),
            Field::new("外交", Bold, true).note("实测为 `**外交**：xxx` 加粗行，非 H2"),
            Field::new("贸易", Bold, false),
            Field::new("军事", Bold, false),
            Field::new("文化", Bold, false),
            Field::new("关联", Section, true).alias("关联标注"),
        ]),
        // ─────────── 势力（真实实卡 0 张，目录里是 2 张 REL 误放）
        t("势力", "ORG", vec![
            Field::new("胶囊", Capsule, true),
            Field::new("目标", Section, true).note("≥1 条可检验，禁抽象口号"),
            Field::new("组织结构", Section, true),
            Field::new("立场", Section, true).note("∈{中立,亲皇室,反皇室,敌对外部,盟友}"),
            Field::new("成员", Section, false).note("≤5 位核心，全部 CHAR-* 编号引"),
            Field::new("资源", Section, false),
            Field::new("关联标注", Section, true).alias("关联"),
        ]),
        // ─────────── 功法（1 张，还是建卡规则）
        t("功法", "TECH", vec![
            Field::new("胶囊", Capsule, true),
            Field::new("功法名", Section, true),
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
        // ─────────── 力量体系（25 张）
        t("力量体系", "TECH", vec![
            Field::new("胶囊", Capsule, true),
            Field::new("体系名", Bold, true),
            Field::new("归属文明", Bold, true),
            Field::new("境界分层", Section, true),
            Field::new("对照关系", Section, true),
            Field::new("关联", Section, true).alias("关联标注"),
        ]),
        // ─────────── 丹药（60 张，平均仅 2 行 —— 严重不达标）
        t("丹药", "MTC", vec![
            Field::new("胶囊", Capsule, true).note("实测仅 6/60 有"),
            Field::new("丹名", Section, true),
            Field::new("品阶", Section, true),
            Field::new("用途", Section, true),
            Field::new("炼制要点", Section, true).alias("制备要点"),
            Field::new("稀缺度说明", Section, false),
            Field::new("关联标注", Section, true).alias("关联").note("实测 59/60 用此段，但内容普遍空"),
        ]),
        // ─────────── 物品（11 张；B 遗漏，实为「天材地宝」落点，前缀 ART）
        t("物品", "ART", vec![
            Field::new("胶囊", Capsule, true),
            Field::new("物名", Section, true),
            Field::new("形态", Section, true).alias("外观"),
            Field::new("效果", Section, true),
            Field::new("产地", Section, false).alias("来源"),
            Field::new("关联", Section, true).alias("关联标注"),
        ]),
        // ─────────── 装备（4 张）
        t("装备", "ART", vec![
            Field::new("胶囊", Capsule, true),
            Field::new("装备名", Section, true),
            Field::new("材质", Section, true),
            Field::new("功能", Section, true),
            Field::new("持有者", Section, false),
            Field::new("关联标注", Section, true).alias("关联"),
        ]),
        // ─────────── 科技（14 张）
        t("科技", "TECH", vec![
            Field::new("胶囊", Capsule, true),
            Field::new("技术名", Section, true),
            Field::new("技术原理", Section, true),
            Field::new("能量来源", Section, false),
            Field::new("副作用", Section, false),
            Field::new("要素清单", Section, false),
            Field::new("关联标注", Section, true).alias("关联"),
        ]),
        // ─────────── 伏笔（22 张；B 遗漏！前缀 CAS+REL 混用）
        t("伏笔", "CAS", vec![
            Field::new("胶囊", Capsule, true).note("22/22 都有"),
            Field::new("起点", Section, true).note("埋设章"),
            Field::new("终点", Section, true).note("兑现章"),
            Field::new("因果链", Section, true).alias("因果链路"),
            Field::new("关联", Section, true).alias("关联标注"),
        ]),
        // ─────────── 文化（顶层 21 张全是模板卡，实卡在文明子目录）
        t("文化", "CULT", vec![
            Field::new("胶囊", Capsule, true),
            Field::new("所属文明", Section, true).alias("文明名").alias("归属文明"),
            Field::new("文化维度", Section, true).note("∈{信仰,习俗,艺术,哲学,货币,语言,礼仪}，一卡一维度"),
            Field::new("核心内容", Section, true).note("≥3 条可核对条款/仪式/器物"),
            Field::new("禁忌", Section, false),
            Field::new("对外输出", Section, false),
            Field::new("关联标注", Section, true).alias("关联").alias("【关联标注】"),
        ]),
        // ─────────── 关系
        t("关系", "REL", vec![
            Field::new("胶囊", Capsule, true),
            Field::new("关系双方", Section, true),
            Field::new("关系性质", Section, true),
            Field::new("演变", Section, false),
            Field::new("关联标注", Section, true).alias("关联"),
        ]),
        // ─────────── 因果线
        t("因果线", "CAS", vec![
            Field::new("胶囊", Capsule, true),
            Field::new("起因", Section, true),
            Field::new("经过", Section, true),
            Field::new("结果", Section, true),
            Field::new("关联标注", Section, true).alias("关联"),
        ]),
        // ─────────── 世界观
        t("世界观", "WORLD", vec![
            Field::new("胶囊", Capsule, true),
            Field::new("核心法则", Section, true),
            Field::new("边界", Section, true),
            Field::new("关联标注", Section, true),
        ]),
        // ─────────── 境界（目录空，但模板存在）
        t("境界", "REALM", vec![
            Field::new("胶囊", Capsule, true),
            Field::new("境界名", Section, true),
            Field::new("所属体系", Section, true),
            Field::new("晋级条件", Section, true),
            Field::new("能力特征", Section, true),
            Field::new("关联标注", Section, true).alias("关联"),
        ]),
        // ─────────── 修炼体系（目录空）
        t("修炼体系", "TECH", vec![
            Field::new("胶囊", Capsule, true),
            Field::new("体系名", Section, true),
            Field::new("基础理论", Section, true),
            Field::new("层次划分", Section, true),
            Field::new("关联标注", Section, true).alias("关联"),
        ]),
        // ─────────── 历史（目录空）
        t("历史", "HIST", vec![
            Field::new("胶囊", Capsule, true),
            Field::new("时期", Section, true),
            Field::new("重大事件", Section, true),
            Field::new("关联标注", Section, true).alias("关联"),
        ]),
        // ─────────── 建筑（目录空）
        t("建筑", "BLDG", vec![
            Field::new("胶囊", Capsule, true),
            Field::new("建筑名", Section, true),
            Field::new("所在地", Section, true),
            Field::new("功能", Section, true),
            Field::new("外观", Section, false),
            Field::new("关联标注", Section, true).alias("关联"),
        ]),
        // ─────────── 技艺（目录空）
        t("技艺", "CRAFT", vec![
            Field::new("胶囊", Capsule, true),
            Field::new("技艺名", Section, true),
            Field::new("所属文明", Section, true),
            Field::new("技术要点", Section, true),
            Field::new("传承方式", Section, false),
            Field::new("关联标注", Section, true).alias("关联"),
        ]),
        // ─────────── 天材地宝（B 有、目录无）
        t("天材地宝", "ART", vec![
            Field::new("胶囊", Capsule, true),
            Field::new("名称", Section, true),
            Field::new("品阶", Section, true),
            Field::new("生长环境", Section, true),
            Field::new("采集方式", Section, false),
            Field::new("药用价值", Section, true),
            Field::new("关联标注", Section, true).alias("关联"),
        ]),
        // ─────────── 诡异
        t("诡异", "ANC", vec![
            Field::new("胶囊", Capsule, true),
            Field::new("名称", Section, true),
            Field::new("特性", Section, true),
            Field::new("克制", Section, true),
            Field::new("出现条件", Section, true),
            Field::new("关联标注", Section, true).alias("关联"),
        ]),
    ]
}

/// 按类别名取模板。
pub fn template_of(category: &str) -> Option<CardTemplate> {
    let want = normalize_category(category);
    all_templates().into_iter().find(|t| t.category == want)
}

/// 类别名归一（把存量里的异名映射到规范名）。
///
/// 实测异名来源：
/// - DB `materials.category` 用 `体系力量`，文件系统用 `力量体系`
/// - DB 里没有 `伏笔`/`物品` 的类目行
pub fn normalize_category(c: &str) -> String {
    match c.trim() {
        "体系力量" => "力量体系".into(),
        "地点" => "地图".into(),
        "跨文明" => "跨文明关系".into(),
        other => other.to_string(),
    }
}

/// 全部类名。
pub fn all_categories() -> Vec<String> {
    all_templates().into_iter().map(|t| t.category).collect()
}

/// 由编号推断类别（CHAR-99-001 → 角色）。
pub fn category_of_code(code: &str) -> Option<String> {
    let p = code.split('-').next()?;
    // 前缀有歧义（ORG 同时是 文明/势力；CULT 同时是 文明/文化；TECH 同时是 功法/力量体系/科技）
    // → 返回主归属（取 all_templates 中首个匹配）
    all_templates()
        .into_iter()
        .find(|t| t.code_prefix.split('/').any(|x| x == p))
        .map(|t| t.category)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_all_missing_categories() {
        // B（_templates_all.json）遗漏、但文件系统真实存在的两类
        assert!(template_of("伏笔").is_some(), "伏笔 类是 B 的遗漏");
        assert!(template_of("物品").is_some(), "物品 类是 B 的遗漏");
    }

    #[test]
    fn covers_20_dirs() {
        let dirs = ["丹药","事件","伏笔","修炼体系","力量体系","功法","势力","历史",
                    "地图","地点","建筑","技艺","文化","文明","时间","物品","科技",
                    "装备","角色","境界"];
        for d in dirs {
            assert!(template_of(d).is_some(), "目录 {d} 无模板");
        }
    }

    #[test]
    fn normalize_synonyms() {
        assert_eq!(normalize_category("体系力量"), "力量体系");
        assert_eq!(normalize_category("地点"), "地图");
        assert_eq!(normalize_category("角色"), "角色");
    }

    #[test]
    fn field_matches_aliases() {
        let t = template_of("事件").unwrap();
        let f = t.fields.iter().find(|f| f.name == "关联标注").unwrap();
        assert!(f.matches("关联标注"));
        assert!(f.matches("关联"), "别名「关联」应命中（实测 74% 用此名）");
        assert!(f.matches("【关联标注】"), "全角括号应被剥掉");
        assert!(!f.matches("无关字段"));
    }

    #[test]
    fn every_template_has_capsule_and_required() {
        for t in all_templates() {
            assert!(!t.fields.is_empty(), "{} 无字段", t.category);
            assert!(t.required_fields().count() >= 1, "{} 无必填字段", t.category);
        }
    }

    #[test]
    fn category_of_code_basic() {
        assert_eq!(category_of_code("CHAR-99-001").as_deref(), Some("角色"));
        assert_eq!(category_of_code("EVT-99-005").as_deref(), Some("事件"));
        assert_eq!(category_of_code("LOC-99-020").as_deref(), Some("地图"));
        assert_eq!(category_of_code("MTC-011").as_deref(), Some("丹药"));
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
