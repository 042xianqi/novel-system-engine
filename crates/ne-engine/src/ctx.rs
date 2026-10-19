//! 门禁上下文的生产实现：从真仓库（novel.db + 04_正文 + 03_叙事规划）读取输入。
//!
//! 安全约定：**所有 DB 访问一律只读**（OpenFlags::SQLITE_OPEN_READONLY），
//! 与 ne-mcp 同口径，绝不因质检而改写用户资料库。

use crate::gates::GateCtx;
use anyhow::Result;
use rusqlite::OpenFlags;
use serde_json::Value;
use std::path::{Path, PathBuf};

// ── 场景关键词表（逐字照抄 context_budget.SCENE_KEYWORDS，顺序即优先级）──
const SCENE_KEYWORDS_BATTLE: &[&str] =
    &["战斗", "厮杀", "攻击", "斩", "刺", "兽虎", "凶兽", "清道队", "脉冲", "剥离", "高熵"];
const SCENE_KEYWORDS_CULTIVATE: &[&str] =
    &["修为", "境界", "功法", "修炼", "运转", "灵力", "熵流体悟", "逆流", "滞流", "锚点"];
const SCENE_KEYWORDS_SOCIAL: &[&str] =
    &["议会", "报告", "判定", "七柱", "联席", "审判", "证词", "表决", "申请", "提案"];
const SCENE_KEYWORDS_PSYCH: &[&str] =
    &["恐惧", "焦虑", "被遗忘", "丧失", "透明", "消散", "忘完", "薄", "空壳"];
const SCENE_KEYWORDS_GRAND: &[&str] =
    &["大撕裂", "九界域", "文明碰撞", "全面", "所有阵营"];
const SCENE_KEYWORDS_EXPLORE: &[&str] =
    &["发现", "锚定", "刻痕", "频率", "感知", "低语", "脉动"];
const SCENE_KEYWORDS_TRANSIT: &[&str] =
    &["赶路", "离开", "回到", "走出", "进入", "一年之后"];

pub struct RepoCtx {
    root: PathBuf,
    con: Option<rusqlite::Connection>,
}

impl RepoCtx {
    pub fn open(root: &Path) -> Result<Self> {
        let db = root.join("novel.db");
        let con = if db.exists() {
            let flags = OpenFlags::from_bits_truncate(0x0000_0001); // READONLY
            Some(rusqlite::Connection::open_with_flags(&db, flags)?)
        } else {
            None
        };
        Ok(RepoCtx { root: root.to_path_buf(), con })
    }

    /// 场景粗判 —— **逐字移植 `context_budget.infer_scene`**：
    /// 各场景按关键词命中**个数**打分，取最高分；全 0 则返回 "日常"；
    /// 并列时取 `SCENE_KEYWORDS` 的**声明序**第一个（Python dict 保序，max 取首个最大）。
    fn infer_scene(&self, seg: &str) -> String {
        let scores: [(&str, usize); 7] = [
            ("战斗", SCENE_KEYWORDS_BATTLE.iter().filter(|k| seg.contains(**k)).count()),
            ("修炼", SCENE_KEYWORDS_CULTIVATE.iter().filter(|k| seg.contains(**k)).count()),
            ("社交", SCENE_KEYWORDS_SOCIAL.iter().filter(|k| seg.contains(**k)).count()),
            ("心理", SCENE_KEYWORDS_PSYCH.iter().filter(|k| seg.contains(**k)).count()),
            ("大场面", SCENE_KEYWORDS_GRAND.iter().filter(|k| seg.contains(**k)).count()),
            ("探索", SCENE_KEYWORDS_EXPLORE.iter().filter(|k| seg.contains(**k)).count()),
            ("过渡", SCENE_KEYWORDS_TRANSIT.iter().filter(|k| seg.contains(**k)).count()),
        ];
        let max = scores.iter().map(|(_, n)| *n).max().unwrap_or(0);
        if max == 0 {
            return "日常".to_string();
        }
        // 声明序第一个达到最高分的
        scores.iter().find(|(_, n)| *n == max).map(|(s, _)| s.to_string()).unwrap()
    }

    /// 读细纲文件全文（卷 NN.md）。
    fn read_outline(&self, volume: &str) -> Option<String> {
        let dir = self.root.join("03_叙事规划").join("细纲");
        for name in [
            format!("{volume}.md"),
            format!("卷{}.md", volume.trim_start_matches("卷")),
        ] {
            let p = dir.join(&name);
            if p.exists() {
                return std::fs::read_to_string(p).ok();
            }
        }
        None
    }

    /// 从细纲切出第 N 章段落（对齐 context_budget.extract_chapter 的切分口径）。
    fn extract_chapter<'a>(text: &'a str, chapter: u32) -> Option<&'a str> {
        let re = regex::Regex::new(&format!(r"(?m)^###\s*第{chapter}章.*$")).ok()?;
        let m = re.find(text)?;
        let start = m.start();
        // 到下一个 ### 第N章 为止
        let rest = &text[m.end()..];
        let next = regex::Regex::new(r"(?m)^###\s*第\d+章").ok()?.find(rest);
        let end = match next {
            Some(n) => m.end() + n.start(),
            None => text.len(),
        };
        Some(&text[start..end])
    }
}

impl GateCtx for RepoCtx {
    fn scene_of(&self, volume: &str, chapter: u32) -> String {
        let Some(txt) = self.read_outline(volume) else {
            return "未知".into();
        };
        match Self::extract_chapter(&txt, chapter) {
            Some(seg) => self.infer_scene(seg),
            None => "未知".into(),
        }
    }

    fn prev_chapters_text(&self, volume: &str, chapter: u32, window: u32) -> String {
        let dir = self.root.join("04_正文").join(volume);
        let mut out = String::new();
        let start = chapter.saturating_sub(window).max(1);
        for i in start..chapter {
            let p = dir.join(format!("第{i}章.md"));
            if let Ok(s) = std::fs::read_to_string(&p) {
                out.push_str(&s);
                out.push('\n');
            }
        }
        out
    }

    fn due_foreshadows(&self, volume: &str, chapter: u32) -> Vec<(String, String)> {
        let Some(con) = &self.con else { return vec![] };
        let sql = "SELECT id, content FROM foreshadow \
                   WHERE deadline_volume=? AND deadline_chapter<=? AND status='pending'";
        let Ok(mut st) = con.prepare(sql) else { return vec![] };
        let rows = st.query_map(rusqlite::params![volume, chapter], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1).unwrap_or_default()))
        });
        match rows {
            Ok(iter) => iter.flatten().collect(),
            Err(_) => vec![],
        }
    }

    fn catchphrases(&self) -> Vec<String> {
        let Some(con) = &self.con else { return vec![] };
        let Ok(mut st) = con.prepare("SELECT personality FROM materials WHERE category='角色'") else {
            return vec![];
        };
        let rows = st.query_map([], |r| r.get::<_, Option<String>>(0));
        let mut out = Vec::new();
        if let Ok(iter) = rows {
            for v in iter.flatten().flatten() {
                if let Ok(j) = serde_json::from_str::<serde_json::Value>(&v) {
                    if let Some(c) = j["口头禅"].as_str() {
                        if !c.is_empty() {
                            out.push(c.to_string());
                        }
                    }
                }
                if out.len() >= 200 {
                    break;
                }
            }
        }
        out
    }

    fn drift_terms(&self) -> Vec<String> {
        let Some(con) = &self.con else { return vec![] };
        let Ok(mut st) =
            con.prepare("SELECT canon FROM kb_knowledge WHERE verdict='drift'")
        else {
            return vec![];
        };
        let rows = st.query_map([], |r| r.get::<_, Option<String>>(0));
        let mut terms = Vec::new();
        if let Ok(iter) = rows {
            let re = regex::Regex::new(r"需清理[:：]\s*(.+)").unwrap();
            for v in iter.flatten().flatten() {
                if let Some(c) = re.captures(&v) {
                    for w in c.get(1).unwrap().as_str().split(['、', ',', '，', '/', ' ']) {
                        let w = w.trim();
                        if w.chars().count() >= 2 {
                            terms.push(w.to_string());
                        }
                    }
                }
            }
        }
        terms
    }

    fn scoring_dims_total(&self) -> i64 {
        let Some(con) = &self.con else { return 0 };
        con.query_row("SELECT COUNT(*) FROM scoring_dimensions", [], |r| r.get(0))
            .unwrap_or(0)
    }
    /// G07 OOC：本章登场角色状态（从 char_state + char_emotion 拼）
    fn char_states_for_ooc(&self, volume: &str, chapter: u32) -> Vec<Value> {
        let Some(con) = &self.con else { return vec![] };
        let mut out = Vec::new();
        // 先取本章细纲引用的 CHAR-* 卡（只查登场角色，不全卷扫）
        let mut codes: Vec<String> = Vec::new();
        if let Some(txt) = self.read_outline(volume) {
            if let Some(seg) = Self::extract_chapter(&txt, chapter) {
                let re = regex::Regex::new(r"CHAR-\d+-\d+").unwrap();
                for m in re.find_iter(seg) {
                    let c = m.as_str().to_string();
                    if !codes.contains(&c) { codes.push(c); }
                }
            }
        }
        if codes.is_empty() {
            return vec![];  // 本章无角色引用 → skip
        }
        let in_list = codes.iter().map(|c| format!("'{c}'")).collect::<Vec<_>>().join(",");
        let sql = format!("SELECT cs.char_code, cs.char_name, cs.realm, cs.status,
                          ce.anger,ce.fear,ce.grief,ce.joy,ce.anxiety,ce.calm,ce.shame,ce.awe,ce.compassion,ce.hope,ce.suppress
                   FROM char_state cs
                   LEFT JOIN char_emotion ce ON ce.char_code=cs.char_code AND ce.chapter=(
                     SELECT MAX(chapter) FROM char_emotion WHERE char_code=cs.char_code AND chapter<{chapter})
                   WHERE cs.char_code IN ({in_list})");
        let Ok(mut st) = con.prepare(&sql) else { return vec![] };
        let dims = ["anger","fear","grief","joy","anxiety","calm","shame","awe","compassion","hope"];
        if let Ok(iter) = st.query_map([], |r| {
            let code: String = r.get(0).unwrap_or_default();
            let name: String = r.get(1).unwrap_or_default();
            let realm: Option<String> = r.get(2).unwrap_or(None);
            let status: String = r.get(3).unwrap_or_default();
            let mut em = serde_json::Map::new();
            let mut any=false;
            for (i,d) in dims.iter().enumerate() {
                let v: Option<f64> = r.get(4+i).unwrap_or(None);
                if v.is_some() { any=true; }
                em.insert(d.to_string(), serde_json::json!(v.unwrap_or(50.0)));
            }
            let sup: Option<f64> = r.get(14).unwrap_or(None);
            Ok((code,name,realm,status,serde_json::Value::Object(em),any,sup))
        }) {
            for (code,name,realm,status,em,any,sup) in iter.flatten() {
                let alive = status != "死亡" && status != "已死";
                // _realm_max：简用当前境界（无历史）；后续可接 char_baseline_snap
                let realm_s = realm.clone().unwrap_or_default();
                out.push(serde_json::json!({
                    "char_code": code,
                    "char_name": name,
                    "alive": alive,
                    "realm": realm_s,
                    "_realm_max": realm_s,
                    "suppress": sup.unwrap_or(1.0),
                    "emotions": em,
                    "_has_emotion": any,
                }));
            }
        }
        let _ = volume;
        out
    }
    /// event_template 全表（判据3）
    fn event_templates_map(&self) -> Option<Value> {
        let con = self.con.as_ref()?;
        let mut st = con.prepare("SELECT id, emotion_delta FROM event_template").ok()?;
        let mut m = serde_json::Map::new();
        let iter = st.query_map([], |r| {
            let id: String = r.get(0).unwrap_or_default();
            let d: Option<String> = r.get(1).unwrap_or(None);
            Ok((id,d))
        }).ok()?;
        for (id,d) in iter.flatten() {
            let dv: Value = d.as_deref().and_then(|s| serde_json::from_str(s).ok()).unwrap_or_else(|| serde_json::json!({}));
            m.insert(id, serde_json::json!({"emotion_delta": dv}));
        }
        Some(Value::Object(m))
    }
    /// kb_world_consistency（判据5）
    fn kb_consistency(&self) -> Option<Value> {
        let p = self.root.join("05_管线框架/data/kb_world_consistency.json");
        let t = std::fs::read_to_string(p).ok()?;
        serde_json::from_str(&t).ok()
    }
    /// G13 离线维度：库规模统计
    fn library_counts(&self) -> std::collections::BTreeMap<String, i64> {
        let mut m = std::collections::BTreeMap::new();
        let Some(con) = &self.con else { return m };
        let cats = ["文明", "角色", "地点", "事件", "功法", "科技", "物品"];
        for c in cats {
            let n: i64 = con
                .query_row("SELECT COUNT(*) FROM materials WHERE category=?1", rusqlite::params![c], |r| r.get(0))
                .unwrap_or(0);
            m.insert(c.to_string(), n);
        }
        let total: i64 = con.query_row("SELECT COUNT(*) FROM materials", [], |r| r.get(0)).unwrap_or(0);
        let used: i64 = con.query_row("SELECT COUNT(*) FROM card_usage", [], |r| r.get(0)).unwrap_or(0);
        m.insert("_总卡".into(), total);
        m.insert("_已引用".into(), used);
        m
    }


}
