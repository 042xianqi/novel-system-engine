//! ne-cardgen：语义建卡编排（M2 核心）。
//!
//! 职责：读细纲头部【待建卡】清单 + 卡面素材 → 按 14 空类归堆 →
//!   调 LLM(经 ne-engine)抽实体填 _规范 模板 → 校验 → 落 02_资料库。
//!
//! M2 骨架：先把"清单抽取 + 按类归堆 + 卡路径规划"做确定性地跑通（0 LLM），
//!   抽实体的 LLM 调用留一个清晰的 TODO 接缝（接 omp-rs-7k dispatch_7k）。
//!   这样 M2 能离线验收"该建哪些卡、建到哪个路径"，M3 再灌真 LLM。

use ne_core::{data_lib_root};
use regex::Regex;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// 一张待建卡工单（从细纲【待建卡】清单抽出来的）。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CardJob {
    /// 卡面里的名字（如"白鸦""灶王爷"）
    pub name: String,
    /// 来源引用（如 "REL-99-004 影阁内门"）
    pub source_ref: String,
    /// 落到哪个 14 类（事件/境界/角色/...）
    pub category: String,
    /// 细纲里的卡面素材（"老矿工·口述传统持有者"这类描述）
    pub material: String,
    /// 目标文件路径（02_资料库/角色/主角/白鸦.md）
    pub target_path: String,
}

/// 建卡落点目录规划：20 类权威口径（2026-10-08，spec novel-pipeline Requirement 2）
/// + 旧 14 类兼容（历史真卡落点，待导入归并）。
const CATEGORY_DIR: &[(&str, &str)] = &[
    // 20 类
    ("角色", "角色"),
    ("势力", "势力"),
    ("地点", "地点"),
    ("物品", "物品"),
    ("文化", "文化"),
    ("技艺", "技艺"),
    ("丹药", "丹药"),
    ("科技", "科技"),
    ("地图", "地图"),
    ("事件", "事件"),
    ("历史", "历史"),
    ("时间", "时间"),
    ("建筑", "建筑"),
    ("装备", "装备"),
    ("功法", "功法"),
    ("修炼体系", "修炼体系"),
    ("境界", "境界"),
    ("力量体系", "力量体系"),
    ("文明", "文明"),
    ("伏笔", "伏笔"),
    // 旧 14 类兼容
    ("关系", "关系"),
    ("因果线", "因果线"),
    ("天材地宝", "天材地宝"),
    ("诡异", "诡异"),
    ("跨文明关系", "跨文明关系"),
    ("世界观", "世界观"),
];

/// 建卡落点根（02_资料库）。
pub fn lib_root(root: &str) -> PathBuf {
    data_lib_root(root)
}

/// 目标卡路径：02_资料库/<类>/<卡名>.md
pub fn card_target(root: &str, category: &str, name: &str) -> String {
    let _ = CATEGORY_DIR.iter().any(|t| t.0 == category); // 校验类合法（未知类仍放行，M3 再收紧）
    format!("{}/{}/{}.md", data_lib_root(root).display(), category, name)
}

/// 从细纲文本的【待建卡】段抽工单。
/// 认两种形态：
///   > - 白鸦（柒玖）（REL-99-004 影阁内门，无角色卡）
///   > - 灶王爷（下城区老铁匠·左腿=净化远征救人回路过载所失·ORG-99-011 §胶囊·无独立 CHAR 卡）
/// 每条抽：名字（首段括号前）+ source_ref（括号里第一个带编号/§的部分）+ material（描述）。
///
/// **必须带行首 `>` 前缀**：这是「【待建卡】清单」的专属标记。
/// 早先用 `^>?`（`>` 可选），会把细纲正文里的普通列表行也扫进来——
/// 实测卷01 匹配 542 行、真卡仅 9 张（`- **钩子**：…白鸦（柒玖）【待建卡…】` 之类全被误抽）。
pub fn extract_card_jobs(outline_text: &str) -> Vec<CardJob> {
    let re = Regex::new(r"(?m)^>\s*-\s+([^（\n]+)（(.*)）\s*$").unwrap();
    let mut jobs = Vec::new();
    for m in re.captures_iter(outline_text) {
        let name = m[1].trim();
        let inner = &m[2];
        // 只取"待建"的：内文含"无角色卡/无 LOC 卡/无独立/无编号/无 EVT 卡/无卡/待建"等标记
        if !inner.contains("无") && !inner.contains("待建") {
            continue;
        }
        let (source_ref, material, category) = parse_inner(inner);
        if name.is_empty() {
            continue;
        }
        jobs.push(CardJob {
            name: name.to_string(),
            source_ref,
            category,
            material,
            target_path: String::new(), // 调用方填（要 root）
        });
    }
    jobs
}

/// 解析括号内文 → (来源引用, 卡面素材, 类别)。
fn parse_inner(inner: &str) -> (String, String, String) {
    // 段以 · / 中文逗号切
    let segs: Vec<&str> = inner.split(['·', '，', ',']).map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    let mut source_ref = String::new();
    let mut material = String::new();
    let category = infer_category(inner);
    for s in &segs {
        let has_code = s.contains("REL-") || s.contains("EVT-") || s.contains("ORG-") || s.contains("CHAR-") || s.contains("LOC-") || s.contains("ART-");
        if has_code && source_ref.is_empty() {
            source_ref = s.to_string();
        } else if !has_code && !s.starts_with("无") && !s.starts_with("卡面") {
            material.push_str(if material.is_empty() { s } else { "·" });
            material.push_str(s);
        }
    }
    (source_ref, material, category)
}

/// 从内文推断类别（按关键词；20 类优先，旧口径关键词保留兼容）。
fn infer_category(inner: &str) -> String {
    for (kw, cat) in [
        // 20 类口径
        ("修炼体系", "修炼体系"), ("力量体系", "力量体系"),
        ("文明", "文明"), ("技艺", "技艺"), ("历史", "历史"),
        ("建筑", "建筑"), ("伏笔", "伏笔"), ("物品", "物品"), ("储物", "物品"),
        // 地理实体优先于抽象地点: 涧山城镇河等→地图
        ("涧", "地图"), ("山", "地图"), ("镇", "地图"), ("河", "地图"),
        ("谷", "地图"), ("峰", "地图"), ("城", "地图"), ("湖", "地图"),
        ("海", "地图"), ("岛", "地图"), ("矿洞", "地图"), ("洞", "地图"),
        ("林", "地图"), ("原", "地图"),
        ("地点", "地点"),
        // 旧口径兼容（真卡前缀未定稿前保持历史行为）
        ("区", "地图"), ("矿洞", "地图"), ("据点", "地图"), ("村", "地图"),
        ("事件", "事件"), ("之变", "事件"), ("计划", "事件"),
        ("境界", "境界"), ("阶", "境界"),
        ("家书", "因果线"), ("委托", "因果线"), ("赎罪", "因果线"),
        ("角色", "角色"), ("老矿工", "角色"), ("教主", "角色"), ("长老", "角色"), ("祭司", "角色"),
        ("帝", "角色"), ("总督", "角色"), ("团长", "角色"), ("货栈", "角色"),
        ("丹药", "丹药"), ("手册", "装备"), ("合金", "装备"), ("碎片", "装备"),
        ("海眼", "天材地宝"), ("能量源", "天材地宝"),
        ("敌对", "势力"), ("商会", "势力"),
        ("区", "势力"), ("帝国", "势力"),
        ("时间", "时间"), ("历", "时间"),
    ] {
        if inner.contains(kw) {
            return cat.to_string();
        }
    }
    "事件".to_string() // 默认落事件（最卡的一类）
}

/// 把工单按 14 类归堆。
pub fn group_by_category(jobs: &[CardJob]) -> BTreeMap<String, Vec<usize>> {
    let mut m: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, j) in jobs.iter().enumerate() {
        m.entry(j.category.clone()).or_default().push(i);
    }
    m
}

/// M2 接缝：抽实体填卡的 LLM 调用（接 omp-rs-7k）。
/// 现在返回 TODO 占位；M3 用 dispatch_7k 选 key + 调端点填 _规范 模板。
#[allow(unused)]
pub fn llm_fill_card_stub(job: &CardJob) -> String {
    format!(
        "# {}（待建卡·LLM 抽实体填 _规范/{} 模板）\n\n> 来源引用：{}\n> 卡面素材：{}\n\n## 详细信息\n(LLM M3 生成，7K lane 抽实体)\n",
        job.name, job.category, job.source_ref, job.material
    )
}

/// 7K lane 选择（M2 真建卡时：按 7K 池给每个建卡任务分配 key）。
pub fn pick_key_for_job(pool: &[String], job_index: usize) -> Option<String> {
    ne_engine::keypool::lane_key(pool, job_index + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_jobs_from_volume01_header() {
        let text = "### 待建卡
> - 白鸦（柒玖）（REL-99-004 影阁内门，无角色卡）
> - 陈老头（EVT-99-005 祖父，无角色卡）
> - 黑牛山北涧（EVT-99-005 地点，无 LOC 卡；青牛镇=LOC-99-020）
> - 白芷（白鸦妹妹，REL-99-004 关联，无角色卡）
";
        let jobs = extract_card_jobs(text);
        let names: Vec<&str> = jobs.iter().map(|j| j.name.as_str()).collect();
        assert!(names.contains(&"白鸦"));
        assert!(names.contains(&"陈老头"));
        assert!(names.contains(&"黑牛山北涧"));
        assert!(names.contains(&"白芷"));
        // 黑牛山北涧 应归到 地图（含"地点/涧"）
        let loc = jobs.iter().find(|j| j.name == "黑牛山北涧").unwrap();
        assert_eq!(loc.category, "地图");
    }

    /// 回归：细纲正文里的普通列表行（**不带 `>` 前缀**）不得被当成待建卡。
    /// 旧正则 `^>?` 会误抽 542 行（卷01），真卡仅 9 张。
    #[test]
    fn body_lines_without_quote_prefix_are_ignored() {
        let text = "> - 白鸦（柒玖）（REL-99-004 影阁内门，无角色卡）
- **钩子**：路径会不会引到白鸦（柒玖）【待建卡·REL-99-004】
- **目标**：把白鸦（柒玖）【待建卡·REL-99-004】的家书寄给白芷（青溪村
";
        let jobs = extract_card_jobs(text);
        let names: Vec<&str> = jobs.iter().map(|j| j.name.as_str()).collect();
        assert_eq!(names, vec!["白鸦"], "只应抽出带 `>` 前缀的那一条，实际：{names:?}");
    }

    #[test]
    fn group_by_cat() {
        let jobs = vec![
            CardJob { name: "a".into(), category: "事件".into(), source_ref: String::new(), material: String::new(), target_path: String::new() },
            CardJob { name: "b".into(), category: "事件".into(), source_ref: String::new(), material: String::new(), target_path: String::new() },
            CardJob { name: "c".into(), category: "角色".into(), source_ref: String::new(), material: String::new(), target_path: String::new() },
        ];
        let g = group_by_category(&jobs);
        assert_eq!(g.get("事件").map(|v| v.len()), Some(2));
        assert_eq!(g.get("角色").map(|v| v.len()), Some(1));
    }
}