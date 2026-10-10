//! 门禁上下文的生产实现：从真仓库（novel.db + 04_正文 + 03_叙事规划）读取输入。
//!
//! 安全约定：**所有 DB 访问一律只读**（OpenFlags::SQLITE_OPEN_READONLY），
//! 与 ne-mcp 同口径，绝不因质检而改写用户资料库。

use crate::gates::GateCtx;
use anyhow::Result;
use rusqlite::OpenFlags;
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
}
