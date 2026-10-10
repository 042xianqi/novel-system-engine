//! gate14 对拍：读合成 lab 环境，跑 Rust 16 门禁，输出 JSON 供与 Python 比对。
use ne_engine::gate14::{self, DueForeshadow, GateCfg, GateCtx};
use ne_engine::budget::{self, BudgetCtx};
use serde_json::Value;
use std::path::{Path, PathBuf};

struct Lab {
    root: PathBuf,
    cfg: GateCfg,
    con: rusqlite::Connection,
}

impl Lab {
    fn new(root: &Path) -> Self {
        let cfg_txt = std::fs::read_to_string(root.join("05_管线框架/data/dynamics_config.json"))
            .unwrap_or_else(|_| "{}".into());
        let cfg = GateCfg::from_json(&serde_json::from_str(&cfg_txt).unwrap_or(Value::Null));
        let con = rusqlite::Connection::open(root.join("novel.db")).unwrap();
        Lab { root: root.to_path_buf(), cfg, con }
    }
}

impl GateCtx for Lab {
    fn cfg(&self) -> &GateCfg { &self.cfg }

    fn prev_chapters_text(&self, volume: &str, chapter: u32) -> String {
        let dir = self.root.join("04_正文").join(volume);
        let mut parts = Vec::new();
        for i in (chapter.saturating_sub(gate14::PREV_WINDOW as u32))..chapter {
            if i == 0 { continue; }
            if let Ok(t) = std::fs::read_to_string(dir.join(format!("第{i}章.md"))) {
                parts.push(t);
            }
        }
        parts.join("\n")
    }

    fn role_catchphrases(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let Ok(mut st) = self.con.prepare(
            "SELECT code, personality FROM materials WHERE category='角色'") else { return out };
        let rows = st.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        }).unwrap();
        for r in rows.flatten() {
            let (code, p) = r;
            let catch = p
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .and_then(|v| v.get("口头禅").and_then(|x| x.as_str()).map(String::from))
                .unwrap_or_default();
            out.push((code, catch));
        }
        out
    }

    fn due_foreshadows(&self, volume: &str, chapter: u32) -> Vec<DueForeshadow> {
        let mut out = Vec::new();
        let Ok(mut st) = self.con.prepare(
            "SELECT id, content FROM foreshadow \
             WHERE deadline_volume=? AND deadline_chapter<=? AND status='pending'") else { return out };
        let rows = st.query_map(rusqlite::params![volume, chapter], |r| {
            Ok(DueForeshadow { id: r.get(0)?, content: r.get(1)? })
        }).unwrap();
        for r in rows.flatten() { out.push(r); }
        out
    }

    fn scene_of(&self, volume: &str, chapter: u32) -> String {
        // 复用 budget 的逻辑：读细纲 + extract_chapter + infer_scene
        let p = self.root.join("03_叙事规划/细纲").join(format!("{volume}.md"));
        let Ok(t) = std::fs::read_to_string(p) else { return "未知".into() };
        let ch = budget::extract_chapter(&t, chapter);
        budget::infer_scene(&ch.text)
    }

    fn kb(&self) -> Option<Value> { None } // 对拍时禁用，与 Python 的 kb 缺失路径对齐

    fn kb_drift_terms_from_db(&self) -> Vec<String> {
        let mut terms = Vec::new();
        let Ok(mut st) = self.con.prepare(
            "SELECT canon FROM kb_knowledge WHERE verdict='drift'") else { return terms };
        let rows = st.query_map([], |r| r.get::<_, Option<String>>(0)).unwrap();
        let re = regex::Regex::new(r"需清理[:：]\s*(.+)").unwrap();
        for r in rows.flatten() {
            if let Some(c) = r {
                if let Some(m) = re.captures(&c) {
                    for w in regex::Regex::new(r"[、,，/\s]+").unwrap().split(m.get(1).unwrap().as_str()) {
                        if w.chars().count() >= 2 { terms.push(w.to_string()); }
                    }
                }
            }
        }
        terms
    }

    fn scoring_dim_count(&self) -> usize {
        self.con.query_row("SELECT COUNT(*) FROM scoring_dimensions", [], |r| r.get::<_, i64>(0))
            .unwrap_or(0) as usize
    }
}

fn main() {
    let root = PathBuf::from(std::env::var("LAB").expect("需设 LAB 环境变量"));
    let text_path = root.join("_test_text.txt");
    let text = std::fs::read_to_string(&text_path).expect("读不到测试文本");
    let lab = Lab::new(&root);
    let run = gate14::run_all(&lab, &text, 3, "卷01").expect("run_all 失败");
    println!("{}", serde_json::to_string_pretty(&run).unwrap());
}
