//! 角色卡 / 样本库解析 —— `load_role.py` + `load_samples.py` + `load_state_b.py` 的移植。
//!
//! **修复的历史 bug**：`load_role.py:38` 硬编码 `ROLE_DIR`、`load_samples.py` 硬编码
//! 外部小说路径 —— Rust 版由调用方注入，不再写死。
//!
//! 只做**解析**（产出结构化值）；DB 写入由 `crate::db` 负责。

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ───────────────────────── load_role.py

/// 境界标签（照抄 REALM_LABELS）。
pub const REALM_LABELS: [&str; 2] = ["等级状态", "修为境界"];
/// 地点标签（照抄 LOC_LABELS）。
pub const LOC_LABELS: [&str; 2] = ["关联地点", "活动地点"];


/// 取 H2/H3 段内容（替代 Python 的 `(?ms)^(#{2,3})\s*{name}\s*$(.*?)(?=^#{2,3}\s|\z)`）。
///
/// ⚠ Rust `regex` crate **不支持 look-ahead**，无法照抄该正则；改用「定位段头 →
/// 扫到下一个 H2/H3 或文末」的等价实现。
fn section_body(card: &str, name: &str) -> Option<String> {
    let head = regex::Regex::new(&format!(r"(?m)^#{{2,3}}\s*{}\s*$", regex::escape(name))).ok()?;
    let m = head.find(card)?;
    let rest = &card[m.end()..];
    let next = regex::Regex::new(r"(?m)^#{2,3}\s").ok()?;
    let end = next.find(rest).map(|x| x.start()).unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

/// 剥值里的加粗/表格噪声；空 → None（照抄 `_clean`）。
pub fn clean(v: &str) -> Option<String> {
    if v.is_empty() {
        return None;
    }
    let v = v.trim();
    let v = v.strip_prefix('|').map(|s| s.trim()).unwrap_or(v);
    let v = v.replace("**", "");
    let v = v.trim();
    let v = v.strip_suffix('|').map(|s| s.trim()).unwrap_or(v);
    if v.is_empty() { None } else { Some(v.to_string()) }
}

/// 提取境界（照抄 `extract_realm`）。返回 (值, basis)。
pub fn extract_realm(card: &str) -> (Option<String>, &'static str) {
    // 限定在「基础信息」段内（若存在）
    let scope_owned = section_body(card, "基础信息");
    let scope: &str = scope_owned.as_deref().unwrap_or(card);

    for lab in REALM_LABELS {
        // 加粗式
        let p = format!(r"\*\*{lab}\s*[：:]?\s*\*\*\s*[：:]?\s*(.+?)(?:\n|$)");
        if let Ok(re) = regex::Regex::new(&p) {
            if let Some(c) = re.captures(scope) {
                if let Some(v) = clean(c.get(1).unwrap().as_str()) {
                    return (Some(v), "实值");
                }
            }
        }
        // 表格式
        let p = format!(r"\|\s*{lab}\s*\|\s*([^|\n]+?)\s*\|");
        if let Ok(re) = regex::Regex::new(&p) {
            if let Some(c) = re.captures(scope) {
                if let Some(v) = clean(c.get(1).unwrap().as_str()) {
                    return (Some(v), "实值");
                }
            }
        }
    }
    (None, "null默认值")
}

/// 提取地点（照抄 `extract_location`）。
pub fn extract_location(card: &str) -> (Option<String>, &'static str) {
    for lab in LOC_LABELS {
        // H2/H3 段式（用 section_body 替代 look-ahead 正则）
        if let Some(body) = section_body(card, lab) {
            let body = body.trim().to_string();
            if !body.is_empty() {
                for line in body.lines() {
                    let line = line.trim().trim_start_matches(['-', '*']).trim();
                    if !line.is_empty() && !line.contains("待核") {
                        if let Some(v) = clean(line) {
                            return (Some(v), "实值");
                        }
                    }
                }
            }
        }
        // 加粗式
        let p = format!(r"\*\*{lab}\s*[：:]?\s*\*\*\s*[：:]?\s*(.+?)(?:\n|$)");
        if let Ok(re) = regex::Regex::new(&p) {
            if let Some(c) = re.captures(card) {
                if let Some(v) = clean(c.get(1).unwrap().as_str()) {
                    return (Some(v), "实值");
                }
            }
        }
    }
    (None, "null默认值")
}

/// 提取角色名（照抄 `name_of`）：H1 `# CODE｜名` → `**姓名**` → 编号兜底。
pub fn name_of(card: &str, code: &str) -> String {
    let p = format!(r"(?m)^#\s*{}\s*[｜|]\s*(.+?)\s*$", regex::escape(code));
    if let Ok(re) = regex::Regex::new(&p) {
        if let Some(c) = re.captures(card) {
            if let Some(v) = clean(c.get(1).unwrap().as_str()) {
                return v;
            }
        }
    }
    let re2 = regex::Regex::new(r"\*\*姓名\*\*\s*[：:]?\s*\*\*\s*[：:]?\s*(.+?)(?:\n|$)").unwrap();
    if let Some(c) = re2.captures(card) {
        if let Some(v) = clean(c.get(1).unwrap().as_str()) {
            return v;
        }
    }
    code.to_string() // 禁编造：用编号兜底
}

/// 单张角色卡的解析结果（对应 `load_role` 的每轮 insert 值）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleRow {
    pub code: String,
    pub name: String,
    pub realm: Option<String>,
    pub location: Option<String>,
    pub realm_basis: String,
    pub location_basis: String,
}

/// 解析单张角色卡（`load_role` 的循环体，去 IO/DB）。
pub fn parse_role_card(code: &str, card: &str) -> RoleRow {
    let (realm, rb) = extract_realm(card);
    let (loc, lb) = extract_location(card);
    RoleRow {
        code: code.to_string(),
        name: name_of(card, code),
        realm,
        location: loc,
        realm_basis: rb.to_string(),
        location_basis: lb.to_string(),
    }
}

// ───────────────────────── load_samples.py

/// 场景关键词（照抄 SCENE_KW）。
pub const SCENE_KW: [(&str, &[&str]); 7] = [
    ("战斗", &["厮杀", "杀", "血", "剑", "刀", "冲", "劈", "刺", "斗", "兵", "箭", "弩"]),
    ("修炼", &["修炼", "功法", "境界", "灵力", "运转", "吐纳", "运功", "内力", "经脉", "心法"]),
    ("社交", &["议会", "判定", "审判", "证词", "表决", "谈判", "议事", "朝议", "朝堂", "朝廷", "大臣", "官", "府", "堂", "座", "君", "相"]),
    ("心理", &["恐惧", "焦虑", "沉思", "犹豫", "念头", "心里", "想着"]),
    ("大场面", &["大战", "全面", "所有阵营", "天下", "席卷", "倾覆", "万军", "城破"]),
    ("探索", &["发现", "感知", "频率", "低语", "脉动", "寻找", "查", "寻", "追", "探"]),
    ("过渡", &["赶路", "离开", "回到", "走出", "进入", "之后", "翌日", "次日"]),
];

/// 场景推断（照抄 `load_samples.infer_scene`：**计数制**，全 0 → 日常）。
///
/// ⚠ 与 `crate::budget::infer_scene` **不同表**：那边是「数量优先」另一套关键词，
/// 这边是 `SCENE_KW` 计数。两者都保留（各自忠实于上游）。
pub fn infer_scene_samples(text: &str) -> String {
    let mut best: Option<(&str, usize)> = None;
    for (scene, kws) in SCENE_KW {
        let n: usize = kws.iter().map(|k| text.matches(*k).count()).sum();
        match best {
            None => best = Some((scene, n)),
            Some((_, bn)) if n > bn => best = Some((scene, n)),
            _ => {}
        }
    }
    match best {
        Some((s, n)) if n > 0 => s.to_string(),
        _ => "日常".to_string(),
    }
}

/// 单章（对应 `_mk` 的返回值）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SampleChapter {
    pub vol: String,
    pub chap_title: String,
    pub text: String,
    pub start: usize,
    pub scene: String,
}

/// 已知卷名（照抄 `split_chapters` 的白名单）。
pub const KNOWN_VOLS: [&str; 7] = [
    "第一卷 在澹州",
    "第二卷 在京都",
    "第三卷 苍山雪",
    "第四卷 北海雾",
    "第五卷 京华江南",
    "第六卷 殿前欢",
    "第七卷 朝天子",
];

/// 按章切分（照抄 `split_chapters`）。`raw_lines` 为原文按行切分。
///
/// 对应 Python 的 `raw_lines[78:96000]` 裁剪，这里由调用方先裁剪。
pub fn split_chapters(raw_lines: &[String]) -> Vec<SampleChapter> {
    let chap_re =
        regex::Regex::new(r"^(第[一二三四五六七八九十百千零\d]+章|楔子|尾声)[\s\u{3000}]*(.{0,20})$").unwrap();
    let vol_re = regex::Regex::new(r"^第([一二三四五六七])卷\s*(\S+)").unwrap();

    let mut cur_vol = KNOWN_VOLS[0].to_string();
    let mut out: Vec<SampleChapter> = Vec::new();
    // cur: (title, body_lines, start_idx)
    let mut cur: Option<(String, Vec<String>, usize)> = None;

    for (idx, line) in raw_lines.iter().enumerate() {
        let s = line.trim();
        if vol_re.is_match(s) && KNOWN_VOLS.contains(&s) {
            cur_vol = s.to_string();
            if let Some((t, b, st)) = cur.take() {
                if !b.is_empty() {
                    if let Some(c) = mk(&t, &b, st, &cur_vol) {
                        out.push(c);
                    }
                }
            }
            continue;
        }
        if chap_re.is_match(s) {
            if let Some((t, b, st)) = cur.take() {
                if !b.is_empty() {
                    if let Some(c) = mk(&t, &b, st, &cur_vol) {
                        out.push(c);
                    }
                }
            }
            cur = Some((s.to_string(), Vec::new(), idx));
            continue;
        }
        if let Some((_, body, _)) = cur.as_mut() {
            if !s.is_empty() {
                body.push(s.to_string());
            }
        }
    }
    if let Some((t, b, st)) = cur {
        if !b.is_empty() {
            if let Some(c) = mk(&t, &b, st, &cur_vol) {
                out.push(c);
            }
        }
    }
    out
}

/// 造章（照抄 `_mk`）：正文 <80 非空白字 → 丢弃（目录残片）。
fn mk(title: &str, body: &[String], start: usize, vol: &str) -> Option<SampleChapter> {
    let text = body.join("\n");
    let n = text.chars().filter(|c| !c.is_whitespace()).count();
    if n < 80 {
        return None;
    }
    Some(SampleChapter {
        vol: vol.to_string(),
        chap_title: title.to_string(),
        scene: infer_scene_samples(&text),
        text,
        start,
    })
}

// ───────────────────────── load_state_b.py

/// 星级归算（照抄 `star_count`）：★ 数 → 1-5；无 → 其它格式；再无 → None。
pub fn star_count(s: &str) -> Option<u32> {
    if s.is_empty() {
        return None;
    }
    let stars = s.matches('★').count();
    if stars > 0 {
        return Some(stars.min(5) as u32);
    }
    let re = regex::Regex::new(r"([1-5])\s*(?:星|档|级)").unwrap();
    if let Some(c) = re.captures(s) {
        if let Some(v) = c.get(1).and_then(|m| m.as_str().parse::<u32>().ok()) {
            return Some(v);
        }
    }
    for (kw, v) in [("S", 5u32), ("A", 4), ("B", 3), ("C", 2), ("D", 1)] {
        let p = format!(r"^\s*{kw}\b");
        if regex::Regex::new(&p).unwrap().is_match(s) {
            return Some(v);
        }
    }
    None
}

/// basis 判定（照抄 `basis_for`）：全空 → null默认值；有值 → 实值。
pub fn basis_for(vals: &[Option<&str>]) -> &'static str {
    if vals.iter().all(|v| v.map(|s| s.is_empty()).unwrap_or(true)) {
        "null默认值"
    } else {
        "实值"
    }
}

/// 从路径推断子类（照抄 `_sub_kind_from_path`）。
pub fn sub_kind_from_path(rel: &str) -> String {
    rel.split('/').next().unwrap_or("").to_string()
}

/// 便捷：把 SampleChapter 转 JSON（供 DB 层灌库）。
pub fn sample_to_json(c: &SampleChapter, source: &str) -> Value {
    serde_json::json!({
        "source": source,
        "vol": c.vol,
        "chap_title": c.chap_title,
        "scene_tag": c.scene,
        "text": c.text,
        "char_count": c.text.chars().filter(|ch| !ch.is_whitespace()).count(),
        "basis": "成功小说样本",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_strips_markdown_noise() {
        assert_eq!(clean("**炼气期**"), Some("炼气期".to_string()));
        assert_eq!(clean("| 炼气期 |"), Some("炼气期".to_string()));
        assert_eq!(clean("   "), None);
        assert_eq!(clean(""), None);
    }

    #[test]
    fn extract_realm_from_bold_and_table() {
        let c1 = "# CHAR-1｜甲\n### 基础信息\n**修为境界**：炼气期三层\n";
        assert_eq!(extract_realm(c1).0.as_deref(), Some("炼气期三层"));
        let c2 = "# CHAR-2｜乙\n### 基础信息\n| 等级状态 | 凝响境 |\n";
        assert_eq!(extract_realm(c2).0.as_deref(), Some("凝响境"));
        let c3 = "# CHAR-3｜丙\n### 基础信息\n无相关字段\n";
        assert_eq!(extract_realm(c3).0, None);
        assert_eq!(extract_realm(c3).1, "null默认值");
    }

    #[test]
    fn extract_location_skips_daihe() {
        let c = "# CHAR-1｜甲\n## 关联地点\n- 青牛镇\n- 待核\n";
        assert_eq!(extract_location(c).0.as_deref(), Some("青牛镇"));
    }

    #[test]
    fn name_of_falls_back_to_code() {
        assert_eq!(name_of("# CHAR-99-001｜陈寂\n", "CHAR-99-001"), "陈寂");
        assert_eq!(name_of("没有 H1 的卡\n", "CHAR-88-002"), "CHAR-88-002");
        let c = "# CHAR-7｜无名\n**姓名**：真名\n";
        assert_eq!(name_of(c, "CHAR-7"), "无名"); // H1 优先
    }

    #[test]
    fn parse_role_card_full() {
        let card = "# CHAR-99-001｜陈寂\n### 基础信息\n**修为境界**：炼气期\n**关联地点**：青牛镇\n";
        let r = parse_role_card("CHAR-99-001", card);
        assert_eq!(r.name, "陈寂");
        assert_eq!(r.realm.as_deref(), Some("炼气期"));
        assert_eq!(r.location.as_deref(), Some("青牛镇"));
    }

    #[test]
    fn infer_scene_samples_counting() {
        // 战斗词 3 个 vs 修炼词 1 个 → 战斗
        assert_eq!(infer_scene_samples("厮杀 剑 刀 修炼"), "战斗");
        assert_eq!(infer_scene_samples("完全无关的内容"), "日常");
    }

    #[test]
    fn split_chapters_basic() {
        let mut lines: Vec<String> = Vec::new();
        lines.push("第一卷 在澹州".into());
        lines.push("第一章 开局".into());
        // 正文需 ≥80 非空白字
        for _ in 0..30 {
            lines.push("他走进屋子，看着灯。".into());
        }
        lines.push("第二章 续".into());
        for _ in 0..30 {
            lines.push("他坐下，拿起书。".into());
        }
        let ch = split_chapters(&lines);
        assert_eq!(ch.len(), 2, "{ch:?}");
        assert_eq!(ch[0].chap_title, "第一章 开局");
        assert_eq!(ch[0].vol, "第一卷 在澹州");
        assert_eq!(ch[1].chap_title, "第二章 续");
    }

    #[test]
    fn split_chapters_drops_short_fragments() {
        let lines = vec![
            "第一章 残片".to_string(),
            "太短".to_string(),
        ];
        assert!(split_chapters(&lines).is_empty(), "不足 80 字应丢弃");
    }

    #[test]
    fn star_count_formats() {
        assert_eq!(star_count("★★★"), Some(3));
        assert_eq!(star_count("★★★★★★"), Some(5), "超过 5 封顶");
        assert_eq!(star_count("4星"), Some(4));
        // ⚠ 对齐 Python 实测：`A级` → None。因为正则 `^\s*A\b` 里 `\b` 要求 A 后是
        // 非单字字符，而「级」是汉字（属于 \w），故 \b 不成立。Python 同此行为。
        assert_eq!(star_count("A级"), None, "A 后接汉字 → \\b 不成立（与 Python 一致）");
        assert_eq!(star_count("A x"), Some(4), "A 后接空格 → \\b 成立");
        assert_eq!(star_count("无关"), None);
        assert_eq!(star_count(""), None);
    }

    #[test]
    fn basis_for_logic() {
        assert_eq!(basis_for(&[None, Some("")]), "null默认值");
        assert_eq!(basis_for(&[None, Some("x")]), "实值");
    }

    #[test]
    fn sample_to_json_shape() {
        let c = SampleChapter {
            vol: "第一卷 在澹州".into(),
            chap_title: "第一章".into(),
            text: "正文".into(),
            start: 0,
            scene: "日常".into(),
        };
        let j = sample_to_json(&c, "庆余年");
        assert_eq!(j["source"], "庆余年");
        assert_eq!(j["scene_tag"], "日常");
        assert_eq!(j["char_count"], 2);
    }
}
