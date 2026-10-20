//! wire.rs —— **接线层**：把资料库里「已建好但从未接入写作链路」的
//! 模型 / 数据全部接上。
//!
//! # 背景（2026-10-13 盘点）
//!
//! 库里建了 60+ 张表与 15 个 data/*.json，但写作链路只有一条窄管道：
//! `细纲 + 角色卡 + 前文 → LLM → 16 门禁 → sync`。情绪引擎、金句库、
//! 意象库、轶事库、技法库、角色标定、67 维评分、8 个动力学模型**全部闲置**。
//! 本模块做两件事：
//!

//!   1. **注入侧** `gather()` / `render_blocks()`：写章前把这些资产组装成
//!      prompt 注入块（情绪快照 / 技法 / 金句 / 意象 / 轶事 / 标定 / 世界状态 / 动力学）。
//!   2. **回写侧** `commit_chapter()`：写章后跑 `emotion::step()` 递推，
//!      把新情绪 / 记忆落到 `char_emotion` / `char_memory`，让状态滚雪球。
//!
//! # 纪律
//!
//! * 注入侧只读；回写侧只写 char_emotion / char_memory / dynamics_events 三张
//!   **推演表**，绝不碰 materials / chapters 等事实源。
//! * 缺文件 / 缺表一律优雅降级（返回空块），不炸生成链。
use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::Path;

use crate::emotion;

/// 场景关键词命中计分（用于技法条排序）。
fn scene_kw_hits(scene: &str, blob: &str) -> i32 {
    let kws: &[&str] = match scene {
        "战斗" | "大战" => &["战斗", "厮杀", "冲突", "高潮", "战"],
        "修炼" | "突破" => &["修炼", "突破", "升级", "成长", "灵气"],
        "社交" | "对话" => &["对话", "人物", "关系", "权谋"],
        "心理" | "独白" => &["心理", "情绪", "内心", "独白"],
        "勘察" | "探索" => &["悬念", "铺垫", "探索", "伏笔"],
        _ => &["日常", "过渡", "铺垫", "节奏"],
    };
    kws.iter().filter(|k| blob.contains(**k)).count() as i32
}

/// 读一个 json 文件（缺失/坏格式返回 None，不报错）。
fn jread(root: &Path, rel: &str) -> Option<Value> {
    let p = root.join(rel);
    let t = std::fs::read_to_string(p).ok()?;
    serde_json::from_str(&t).ok()
}

/// 只读开 DB（库缺失返回 None）。
fn open_ro(root: &Path) -> Option<Connection> {
    let db = root.join("novel.db");
    if !db.exists() {
        return None;
    }
    Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()
}

// --------------------------------------------- 场景 -> 标签映射

/// 场景（中文，来自细纲推断）-> 金句/意象 scene_tag 候选。
fn scene_tags(scene: &str) -> Vec<&'static str> {
    match scene {
        "战斗" | "大战" => vec!["战斗", "决绝", "战斗前", "代价"],
        "修炼" | "突破" => vec!["信念", "执迷", "哲理"],
        "社交" | "对话" => vec!["权谋", "政治", "忠义"],
        "心理" | "独白" => vec!["孤独", "执迷", "哲理"],
        "勘察" | "探索" => vec!["悬念", "哲理"],
        "离别" => vec!["离别", "希望"],
        _ => vec!["哲理", "希望", "代价"],
    }
}


/// 技法 applies_to 是**粒度**（章/开篇/全书/卷），不是场景。
/// 本章可用的 = 粒度在「章/开篇/全书」的条目。
fn tech_applies(_scene: &str) -> Vec<&'static str> {
    vec!["章", "开篇", "全书"]
}

// --------------------------------------------- 注入侧

/// 接线数据包（注入侧产出的结构化结果）。
#[derive(Debug, Default, Clone)]
pub struct WireData {
    pub emotion: Vec<Value>,
    pub calib: Vec<Value>,
    pub technique: Vec<Value>,
    pub quotes: Vec<Value>,
    pub imagery: Vec<Value>,
    pub anecdotes: Vec<Value>,
    pub world: Vec<Value>,
    pub dynamics: Value,
    /// 世界物理推演（civ.tide/ent.entropy/res.decay）
    pub world_physics: Value,
    /// 状态表（势力/物品/科技/丹药/宝材/地图/文化/子文明）
    pub states: Vec<Value>,
    /// 角色默认情绪填充（char_default_fill.json，无历史角色用）
    pub default_fill: Vec<Value>,
    /// FTS 相关卡推荐（细纲未显式引用但相关）
    pub fts_related: Vec<Value>,
}

/// 收集接线数据（只读）。
pub fn gather(
    root: &Path,
    volume: &str,
    chapter: u32,
    scene: &str,
    char_codes: &[String],
) -> WireData {
    let mut d = WireData::default();
    let con = open_ro(root);

    // (1) 角色情绪快照：char_emotion 里各角色 < 本章 的最新一行
    if let Some(c) = &con {
        for code in char_codes {
            if let Ok(mut st) = c.prepare(
                "SELECT chapter,anger,fear,grief,joy,anxiety,calm,shame,awe,compassion,hope,\
                 dominant,strength,suppress FROM char_emotion \
                 WHERE char_code=?1 AND chapter<?2 ORDER BY chapter DESC LIMIT 1",
            ) {
                if let Ok(row) = st.query_row(rusqlite::params![code, chapter as i64], |r| {
                    let dims = ["anger", "fear", "grief", "joy", "anxiety", "calm", "shame", "awe", "compassion", "hope"];
                    let mut em = Map::new();
                    for (i, dim) in dims.iter().enumerate() {
                        em.insert(dim.to_string(), json!(r.get::<_, f64>(i + 1).unwrap_or(50.0)));
                    }
                    Ok(json!({
                        "char_code": code,
                        "chapter": r.get::<_, i64>(0).unwrap_or(0),
                        "emotions": Value::Object(em),
                        "dominant": r.get::<_, Option<String>>(11).unwrap_or(None),
                        "strength": r.get::<_, f64>(12).unwrap_or(0.0),
                        "suppress": r.get::<_, f64>(13).unwrap_or(1.0),
                    }))
                }) {
                    d.emotion.push(row);
                }
            }
        }
    }

    // (2) 角色说话标定
    for f in ["char_calib_99.json", "char_calib_20_70.json", "char_calib_longtail.json"] {
        let Some(v) = jread(root, &format!("05_管线框架/data/{f}")) else { continue };
        let Some(arr) = v.get("characters").and_then(|x| x.as_array()) else { continue };
        for c in arr {
            let code = c.get("code").and_then(|x| x.as_str()).unwrap_or("");
            if !char_codes.iter().any(|x| x == code) {
                continue;
            }
            let base: Vec<String> = c
                .get("emotion_base")
                .and_then(|x| x.as_object())
                .map(|o| {
                    o.iter()
                        .filter_map(|(k, v)| v.get("value").and_then(|x| x.as_f64()).map(|n| format!("{k}={n:.0}")))
                        .collect()
                })
                .unwrap_or_default();
            let sup = c.get("suppress").and_then(|x| x.get("value")).and_then(|x| x.as_f64());
            let lam = c.get("lambda").and_then(|x| x.get("value")).and_then(|x| x.as_str());
            d.calib.push(json!({
                "code": code,
                "name": c.get("name").and_then(|x| x.as_str()).unwrap_or(""),
                "type": c.get("card_type").and_then(|x| x.as_str()).unwrap_or(""),
                "emotion_base": base.join(" "),
                "suppress": sup,
                "lambda": lam,
            }));
        }
    }

    // (3) 技法卡：technique_items 按场景 applies_to
    if let Some(c) = &con {
        let wants = tech_applies(scene);
        if let Ok(mut st) = c.prepare(
            "SELECT code,name,applies_to,quantified FROM technique_items LIMIT 120",
        ) {
            let rows = st.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0).unwrap_or_default(),
                    r.get::<_, String>(1).unwrap_or_default(),
                    r.get::<_, Option<String>>(2).unwrap_or(None),
                    r.get::<_, Option<String>>(3).unwrap_or(None),
                ))
            });
            let mut cand: Vec<(i32, Value)> = Vec::new();
            if let Ok(iter) = rows {
                for (code, name, applies, quant) in iter.flatten() {
                    let a = applies.unwrap_or_default();
                    if !wants.iter().any(|w| a.contains(w)) {
                        continue;
                    }
                    let blob = format!("{name}{}", quant.clone().unwrap_or_default());
                    let score = scene_kw_hits(scene, &blob);
                    cand.push((score, json!({
                        "code": code, "name": name,
                        "applies_to": a,
                        "quantified": quant.unwrap_or_default(),
                    })));
                }
            }
            cand.sort_by(|x, y| y.0.cmp(&x.0));
            for (_, v) in cand.into_iter().take(6) {
                d.technique.push(v);
            }
        }
    }

    // (4)(5)(6) 金句 / 意象 / 轶事
    let tags = scene_tags(scene);
    if let Some(v) = jread(root, "05_管线框架/data/kb_quotes.json") {
        if let Some(arr) = v.get("entries").and_then(|x| x.as_array()) {
            for e in arr {
                let st = e.get("scene_tag").and_then(|x| x.as_str()).unwrap_or("");
                if tags.iter().any(|t| st.contains(t)) {
                    d.quotes.push(e.clone());
                    if d.quotes.len() >= 4 { break; }
                }
            }
        }
    }
    if let Some(v) = jread(root, "05_管线框架/data/kb_imagery.json") {
        if let Some(arr) = v.get("entries").and_then(|x| x.as_array()) {
            for e in arr {
                let us: Vec<String> = match (e.get("use_scene"), e.get("use_scenes")) {
                    (Some(Value::String(s)), _) => vec![s.clone()],
                    (_, Some(Value::Array(a))) => a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect(),
                    _ => Vec::new(),
                };
                if us.iter().any(|x| tags.iter().any(|t| x.contains(t))) {
                    d.imagery.push(e.clone());
                    if d.imagery.len() >= 3 { break; }
                }
            }
        }
    }
    if let Some(v) = jread(root, "05_管线框架/data/kb_anecdotes.json") {
        if let Some(arr) = v.get("entries").and_then(|x| x.as_array()) {
            for e in arr {
                let st = e.get("scene_tag").and_then(|x| x.as_str()).unwrap_or("");
                if tags.iter().any(|t| st.contains(t)) {
                    d.anecdotes.push(e.clone());
                    if d.anecdotes.len() >= 2 { break; }
                }
            }
        }
    }

    // (7) 世界状态
    if let Some(c) = &con {
        if let Ok(mut st) = c.prepare(
            "SELECT civilization,chapter,share,notes FROM world_state WHERE volume=?1 ORDER BY share DESC LIMIT 6",
        ) {
            if let Ok(iter) = st.query_map(rusqlite::params![volume], |r| {
                Ok(json!({
                    "civ": r.get::<_, String>(0).unwrap_or_default(),
                    "chapter": r.get::<_, i64>(1).unwrap_or(0),
                    "share": r.get::<_, f64>(2).unwrap_or(0.0),
                    "notes": r.get::<_, Option<String>>(3).unwrap_or(None),
                }))
            }) {
                d.world = iter.flatten().collect();
            }
        }
    }

    // (8) 动力学
    d.dynamics = match &con { Some(c) => run_dynamics_ref(c, volume, chapter, scene), None => Value::Object(Map::new()) };

    // (9) 世界物理推演（文明潮汐/熵债/残响衰减）
    d.world_physics = match &con { Some(c) => run_world_physics_ref(c, volume, chapter), None => Value::Object(Map::new()) };

    // (10) 状态表（本章相关势力/物品/科技/宝材/地图等）
    d.states = load_states(&con, volume, chapter);

    // (11) 角色默认情绪填充（无历史角色）
    d.default_fill = load_default_fill(root, char_codes);

    // (12) FTS 相关卡推荐（细纲关键词全文检索）
    if let Some(c) = &con {
        let outline = c.query_row("SELECT 1", [], |_| Ok(String::new())).unwrap_or_default();
        let _ = outline;
    }
    d.fts_related = match (&con, outline_seg_opt(root, volume, chapter)) {
        (Some(c), Some(seg)) => {
            let already: Vec<String> = ch_card_refs(root, volume, chapter);
            fts_related(c, &seg, &already)
        }
        _ => Vec::new(),
    };

    d
}

/// 取本章细纲段落（供 FTS 关键词）。
fn outline_seg_opt(root: &Path, volume: &str, chapter: u32) -> Option<String> {
    let p = root.join("03_叙事规划/细纲").join(format!("{volume}.md"));
    let t = std::fs::read_to_string(p).ok()?;
    let re = regex::Regex::new(&format!(r"(?m)^###\s*第{chapter}章")).ok()?;
    let m = re.find(&t)?;
    let rest = &t[m.start()..];
    let next = regex::Regex::new(r"(?m)^###\s*第\d+章").ok()?.find(&rest[3..]);
    let end = next.map(|n| m.start()+3+n.start()).unwrap_or(t.len());
    Some(t[m.start()..end].to_string())
}

/// 本章细纲显式引用的卡号。
fn ch_card_refs(root: &Path, volume: &str, chapter: u32) -> Vec<String> {
    let Some(seg) = outline_seg_opt(root, volume, chapter) else { return vec![] };
    let re = regex::Regex::new(r"[A-Z]{2,5}-\d+-\d+").unwrap();
    re.find_iter(&seg).map(|m| m.as_str().to_string()).collect()
}

/// FTS5 相关卡推荐：从本章细纲提关键词 → 全文检索 → 推荐细纲未显式引用但相关的卡。
fn fts_related(con: &Connection, outline_seg: &str, already: &[String]) -> Vec<Value> {
    // 从细纲提取 2-4 字中文词（粗切：连续汉字串）
    let mut kws: Vec<String> = Vec::new();
    let mut cur = String::new();
    for ch in outline_seg.chars() {
        if ch >= '\u{4e00}' && ch <= '\u{9fff}' {
            cur.push(ch);
        } else {
            if cur.chars().count() >= 2 && cur.chars().count() <= 6 {
                kws.push(cur.clone());
            }
            cur.clear();
        }
        if kws.len() >= 12 { break; }
    }
    if cur.chars().count() >= 2 && cur.chars().count() <= 6 { kws.push(cur); }
    kws.dedup();
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for kw in kws {
        let q = format!("\"{}\"", kw.replace('"', ""));
        if let Ok(mut st) = con.prepare(
            "SELECT code, name FROM materials_fts WHERE materials_fts MATCH ?1 LIMIT 3",
        ) {
            if let Ok(iter) = st.query_map(rusqlite::params![q], |r| {
                Ok((r.get::<_, String>(0).unwrap_or_default(), r.get::<_, String>(1).unwrap_or_default()))
            }) {
                for (code, name) in iter.flatten() {
                    if already.iter().any(|x| x == &code) || !seen.insert(code.clone()) {
                        continue;
                    }
                    out.push(json!({"code": code, "name": name, "kw": kw}));
                    if out.len() >= 6 { return out; }
                }
            }
        }
    }
    out
}

/// 跑世界物理 3 公式（payload 从状态表/卷信息拼）。
fn run_world_physics_ref(con: &Connection, volume: &str, chapter: u32) -> Value {
    let mut out = Map::new();
    // 文明潮汐：取本卷 share 最高势力，相位按卷序
    let (civ, share) = {
        let c = con;
        c.query_row(
                "SELECT civilization, share FROM world_state WHERE volume=?1 ORDER BY share DESC LIMIT 1",
                rusqlite::params![volume],
                |r| Ok((r.get::<_, String>(0).unwrap_or_default(), r.get::<_, f64>(1).unwrap_or(0.5))),
            )
            .ok()
    }
    .unwrap_or((String::new(), 0.5));
    if !civ.is_empty() {
        let p = json!({"civilization": civ, "chapter": chapter, "phase": (share * 100.0), "stance": "守成"});
        if let Ok(v) = crate::world_physics::run_model("civ.tide", &p) {
            out.insert("civ.tide".into(), v);
        }
    }
    // 熵债：以本章序号推耗散
    let p = json!({"actor": "world", "entropy_debt": (chapter as f64 * 0.5), "power_used": 1, "power_cost": 1.0});
    if let Ok(v) = crate::world_physics::run_model("ent.entropy", &p) {
        out.insert("ent.entropy".into(), v);
    }
    // 残响衰减：本章距开篇
    let p = json!({"resonance_id": volume, "intensity": 1.0, "age_ch": chapter, "decay_rate": 0.92});
    if let Ok(v) = crate::world_physics::run_model("res.decay", &p) {
        out.insert("res.decay".into(), v);
    }
    Value::Object(out)
}

/// 读 8 张状态表（本章相关条目；chapter=0 的静态快照也带上）。
fn load_states(con: &Option<Connection>, _volume: &str, _chapter: u32) -> Vec<Value> {
    let Some(c) = con else { return vec![] };
    let tables: [(&str, &str); 8] = [
        ("map_state", "地图/地点"),
        ("power_state", "力量体系"),
        ("tech_state", "科技"),
        ("elixir_state", "丹药/药剂"),
        ("treasure_state", "天材地宝"),
        ("item_state", "物品"),
        ("culture_state", "文化"),
        ("civ_sub_state", "文明子项"),
    ];
    let mut out = Vec::new();
    for (t, label) in tables {
        let sql = format!("SELECT code, name, chapter FROM [{t}] LIMIT 8");
        if let Ok(mut st) = c.prepare(&sql) {
            if let Ok(iter) = st.query_map([], |r| {
                Ok(json!({
                    "tbl": t, "label": label,
                    "code": r.get::<_, String>(0).unwrap_or_default(),
                    "name": r.get::<_, String>(1).unwrap_or_default(),
                }))
            }) {
                for v in iter.flatten() {
                    out.push(v);
                }
            }
        }
    }
    out
}

/// 读 char_default_fill.json（无历史角色的情绪默认值）。
fn load_default_fill(root: &Path, codes: &[String]) -> Vec<Value> {
    let Some(v) = jread(root, "05_管线框架/data/char_default_fill.json") else { return vec![] };
    let Some(arr) = v.get("chars").and_then(|x| x.as_array()) else { return vec![] };
    let mut out = Vec::new();
    for c in arr {
        let code = c.get("code").and_then(|x| x.as_str()).unwrap_or("");
        if codes.iter().any(|x| x == code) {
            out.push(c.clone());
        }
    }
    out
}

/// 跑动力学模型子集（payload 从库/场景拼，缺数据则各模型自报 _error）。
fn run_dynamics_ref(con: &Connection, volume: &str, chapter: u32, scene: &str) -> Value {
    let mut payload = Map::new();
    {
        let c = con;
        if let Ok(n) = c.query_row("SELECT COUNT(*) FROM events", [], |r| r.get::<_, i64>(0)) {
            payload.insert("atk_num".into(), json!(n.max(1)));
            payload.insert("def_num".into(), json!((n / 2).max(1)));
        }
        if let Ok(mut st) = c.prepare("SELECT category, COUNT(*) FROM card_usage GROUP BY category") {
            if let Ok(iter) = st.query_map([], |r| {
                Ok((r.get::<_, String>(0).unwrap_or_default(), r.get::<_, i64>(1).unwrap_or(0)))
            }) {
                let mut steps = Map::new();
                let mut total = 0i64;
                for (k, v) in iter.flatten() {
                    steps.insert(k, json!(v));
                    total += v;
                }
                payload.insert("card_steps".into(), Value::Object(steps));
                payload.insert("entity_count".into(), json!(total.max(1)));
            }
        }
    }
    payload.insert("chapter".into(), json!(chapter));
    payload.insert("deadline".into(), json!(chapter + 10));
    payload.insert("scene".into(), json!(scene));
    payload.insert("domain".into(), json!(volume));
    let p = Value::Object(payload);
    let mut out = Map::new();
    for mid in ["lst.square", "bayes.mood", "tlm.tension", "dmg.10dim", "sna.causal_density"] {
        let v = crate::dynamics::run_model(mid, &p).unwrap_or_else(|e| json!({"_error": e.to_string()}));
        out.insert(mid.to_string(), v);
    }
    Value::Object(out)
}

/// 渲染成 prompt 注入块（markdown 文本）。
pub fn render_blocks(d: &WireData) -> String {
    let mut parts: Vec<String> = Vec::new();

    if !d.emotion.is_empty() {
        let mut lines = Vec::new();
        for e in &d.emotion {
            let code = e["char_code"].as_str().unwrap_or("");
            let dom = e["dominant"].as_str().unwrap_or("—");
            let sup = e["suppress"].as_f64().unwrap_or(1.0);
            let ems: Vec<String> = e["emotions"]
                .as_object()
                .map(|o| o.iter().map(|(k, v)| format!("{k}={:.0}", v.as_f64().unwrap_or(0.0))).collect())
                .unwrap_or_default();
            let ch = e["chapter"].as_i64().unwrap_or(0);
            lines.push(format!(
                "  【{code}】截至第{ch}章：主导={dom}  压抑={sup:.2}  {}\n    -> 本章须延续此情绪基线（除非事件强力扭转）",
                ems.join(" ")
            ));
        }
        parts.push(format!("【角色情绪快照（情绪引擎·连续递推）】\n{}", lines.join("\n")));
    }

    if !d.calib.is_empty() {
        let lines: Vec<String> = d.calib.iter().map(|c| {
            format!(
                "  【{} {}】（{}）情绪基线：{}  压抑系数={}  记忆衰减={}",
                c["code"].as_str().unwrap_or(""),
                c["name"].as_str().unwrap_or(""),
                c["type"].as_str().unwrap_or(""),
                c["emotion_base"].as_str().unwrap_or(""),
                c["suppress"].as_f64().map(|v| format!("{v}")).unwrap_or_else(|| "待标定".into()),
                c["lambda"].as_str().unwrap_or("默认"),
            )
        }).collect();
        parts.push(format!("【角色说话标定（char_calib）】\n{}", lines.join("\n")));
    }

    if !d.technique.is_empty() {
        let lines: Vec<String> = d.technique.iter().map(|t| {
            format!(
                "  【{} {}】适用：{}  量化：{}",
                t["code"].as_str().unwrap_or(""),
                t["name"].as_str().unwrap_or(""),
                t["applies_to"].as_str().unwrap_or(""),
                t["quantified"].as_str().unwrap_or(""),
            )
        }).collect();
        parts.push(format!("【技法卡（本章场景匹配）】\n{}", lines.join("\n")));
    }

    if !d.quotes.is_empty() {
        let lines: Vec<String> = d.quotes.iter().map(|q| {
            format!("  「{}」（{}·{}）", q["quote"].as_str().unwrap_or(""), q["source"].as_str().unwrap_or(""), q["insert_pos"].as_str().unwrap_or(""))
        }).collect();
        parts.push(format!("【金句参考（可化用/借意象，勿照抄）】\n{}", lines.join("\n")));
    }

    if !d.imagery.is_empty() {
        let lines: Vec<String> = d.imagery.iter().map(|q| {
            let m: String = q["metaphor"].as_str().unwrap_or("").chars().take(60).collect();
            format!("  {} —— {}", q["imagery"].as_str().unwrap_or(""), m)
        }).collect();
        parts.push(format!("【意象库参考】\n{}", lines.join("\n")));
    }

    if !d.anecdotes.is_empty() {
        let lines: Vec<String> = d.anecdotes.iter().map(|q| {
            let a: String = q["anecdote"].as_str().unwrap_or("").chars().take(50).collect();
            format!("  {}（{}）寓意：{}", a, q["source"].as_str().unwrap_or(""), q["moral"].as_str().unwrap_or(""))
        }).collect();
        parts.push(format!("【轶事/寓言参考】\n{}", lines.join("\n")));
    }

    if !d.world.is_empty() {
        let lines: Vec<String> = d.world.iter().map(|w| {
            format!("  【{}】份额={:.2}  {}", w["civ"].as_str().unwrap_or(""), w["share"].as_f64().unwrap_or(0.0), w["notes"].as_str().unwrap_or(""))
        }).collect();
        parts.push(format!(
            "【世界势力状态（world_state）】\n> 注：下表为本卷卷末目标格局（供方向参考，非本章现状；本章势力关系尚未演进至此）\n{}",
            lines.join("\n")
        ));
    }

    if d.dynamics.is_object() && !d.dynamics.as_object().map(|o| o.is_empty()).unwrap_or(true) {
        let txt = serde_json::to_string_pretty(&d.dynamics).unwrap_or_default();
        parts.push(format!("【动力学模型推演（8 模型子集，供参考不强制）】\n{}", txt));
    }

    if d.world_physics.is_object() && !d.world_physics.as_object().map(|o| o.is_empty()).unwrap_or(true) {
        let txt = serde_json::to_string_pretty(&d.world_physics).unwrap_or_default();
        parts.push(format!("【世界物理推演（文明潮汐/熵债/残响衰减）】\n{}", txt));
    }

    if !d.states.is_empty() {
        let lines: Vec<String> = d.states.iter().map(|s| {
            format!("  【{}·{}】{}", s["label"].as_str().unwrap_or(""), s["tbl"].as_str().unwrap_or(""), s["name"].as_str().unwrap_or(""))
        }).collect();
        parts.push(format!("【世界状态表（势力/物品/科技/宝材等）】\n{}", lines.join("\n")));
    }

    if !d.default_fill.is_empty() {
        let lines: Vec<String> = d.default_fill.iter().take(4).map(|c| {
            format!("  【{}】{}", c["code"].as_str().unwrap_or(""), c["char_name"].as_str().unwrap_or(""))
        }).collect();
        parts.push(format!("【角色情绪默认填充（无历史，char_default_fill）】\n{}", lines.join("\n")));
    }

    if !d.fts_related.is_empty() {
        let lines: Vec<String> = d.fts_related.iter().map(|s| {
            format!("  【{}】{}（命中词：{}）", s["code"].as_str().unwrap_or(""), s["name"].as_str().unwrap_or(""), s["kw"].as_str().unwrap_or(""))
        }).collect();
        parts.push(format!("【全文检索·相关卡推荐（细纲未显式引用，供参考）】\n{}", lines.join("\n")));
    }
    parts.join("\n\n")
}

// --------------------------------------------- 回写侧

/// 写章后：跑情绪递推，落库 char_emotion / char_memory / dynamics_events。
/// dynamics_events 的**单一主体**取值（F10 门禁要求 subject 不含逗号）。
///
/// 约定来源：真实库既有数据 `subject='CHAR-99-001'` 而
/// `subjects_json=["CHAR-99-001","CHAR-99-097"]` —— subject 只放**主实体**，
/// 全体实体码放 subjects_json 数组，二者并存。
///
/// 无角色码时回退 `"—"`：它仍是**单值且无逗号**，满足门禁的可判定性；
/// "本事件无实体"这一语义由 subjects_json 的 `[]` 表达，不靠 subject 承载。
fn primary_subject(codes: &[String]) -> String {
    codes.first().cloned().unwrap_or_else(|| "—".to_string())
}

pub fn commit_chapter(
    db_path: &Path,
    root: &Path,
    volume: &str,
    chapter: u32,
    char_codes: &[String],
    event_ids: &[String],
) -> Result<Vec<(String, String)>> {
    let con = rusqlite::Connection::open(db_path)?;
    // 小说 ID 可配（book_profile.novel_id）；缺省回落 canxiang，行为不变。
    let novel_id = crate::config::load(root)
        .map(|c| c.book_profile.novel_id().to_string())
        .unwrap_or_else(|_| crate::ops::NOVEL_ID.to_string());
    let triggered = build_triggered(&con, event_ids);
    let triggered_v = Value::Array(triggered);
    let mut done = Vec::new();

    for code in char_codes {
        let prev = read_prev_state(&con, root, code, chapter);
        let calib = read_calib(root, code);
        let new_state = match emotion::step(chapter, &triggered_v, &prev, calib.as_ref()) {
            Ok(s) => s,
            Err(e) => {
                done.push((code.clone(), format!("step 失败：{e}")));
                continue;
            }
        };
        write_emotion(&con, code, &new_state, chapter)?;
        write_memory(&con, code, &new_state, chapter)?;
        let dom = new_state["dominant"].as_str().unwrap_or("—");
        done.push((code.clone(), format!("ch{chapter} dominant={dom}")));
    }

    // ── F10 对齐（2026-10-20 三处 P0 修复）────────────────────────────
    // 铁律来源：真实库既有数据 `subject='CHAR-99-001'` + `subjects_json=["CHAR-99-001","CHAR-99-097"]`。
    //   1. `subject` = **单一主体**。唯一键是 (kind, subject, at)，且 F10 门禁规定
    //      subject 含逗号即 RED —— 原先传 `char_codes.join(",")` 会直接亮红灯。
    //   2. `subjects_json` = **全量实体数组**（供门禁按实体反查）。
    //      序列化失败回退 `"[]"` 而非 `""`：空串不是合法 JSON，会让按 JSON 解析的
    //      门禁直接报错；`[]` 才是语义正确的"本事件无实体"。
    //   3. 写入前按 (kind, at, novel_id) **先删后插**：同章重跑时旧行会与
    //      ux_dyn_kind_subject_at 相撞，原实现的 `let _ =` 把失败吞掉 = 静默丢数据。
    let subject = primary_subject(char_codes);
    let subjects_json = serde_json::to_string(char_codes).unwrap_or_else(|_| "[]".to_string());
    let at = chapter as i64;
    // 【注意】DELETE 的范围必须与唯一键 `ux_dyn_kind_subject_at(kind, subject, at)` **严格对齐**：
    // 该索引里**没有 novel_id**，所以清理时也不能用 novel_id 限定——否则当既有行的
    // novel_id 与当前配置不同时，删除匹配不到，插入依旧撞唯一键（实测已复现）。
    // 按 (kind, at) 删除是本章该 kind 的全量清理，是能保证不撞键的最小安全范围。
    let cleanup = |kind: &str| -> Result<()> {
        con.execute(
            "DELETE FROM dynamics_events WHERE kind=?1 AND at=?2",
            rusqlite::params![kind, at],
        )?;
        Ok(())
    };
    cleanup("emotion_engine.step.by_wire")?;
    con.execute(
        "INSERT INTO dynamics_events(seq,novel_id,domain,kind,subject,at,payload,created_at,subjects_json)
         VALUES((SELECT COALESCE(MAX(seq),0)+1 FROM dynamics_events),?1,?2,'emotion_engine.step.by_wire',?3,?4,?5,datetime('now'),?6)",
        rusqlite::params![
            novel_id,
            volume,
            subject,
            at,
            serde_json::to_string(&json!({"chars": char_codes, "events": event_ids})).unwrap_or_default(),
            subjects_json,
        ],
    )?;

    // 落动力学 8 模型 + 世界物理 3 公式（幂等：同章重跑覆盖式新增）
    let dz = run_dynamics_ref(&con, volume, chapter, "");
    let wp = run_world_physics_ref(&con, volume, chapter);
    for (kind, val) in [("dynamics.by_wire", dz), ("world_physics.by_wire", wp)] {
        if val.as_object().map(|o| o.is_empty()).unwrap_or(true) { continue; }
        cleanup(kind)?;
        con.execute(
            "INSERT INTO dynamics_events(seq,novel_id,domain,kind,subject,at,payload,created_at,subjects_json) VALUES((SELECT COALESCE(MAX(seq),0)+1 FROM dynamics_events),?1,?2,?3,?4,?5,?6,datetime('now'),?7)",
            rusqlite::params![novel_id, volume, kind, subject, at, serde_json::to_string(&val).unwrap_or_default(), subjects_json],
        )?;
    }
    Ok(done)
}

/// 查 event_template，构造 step 需要的 triggered_events 数组。
fn build_triggered(con: &Connection, ids: &[String]) -> Vec<Value> {
    let mut out = Vec::new();
    for id in ids {
        let row = con.query_row(
            "SELECT id,name,affected_roles,emotion_delta,i FROM event_template WHERE id=?1",
            rusqlite::params![id],
            |r| {
                Ok((
                    r.get::<_, String>(0).unwrap_or_default(),
                    r.get::<_, String>(1).unwrap_or_default(),
                    r.get::<_, Option<String>>(2).unwrap_or(None),
                    r.get::<_, Option<String>>(3).unwrap_or(None),
                    r.get::<_, Option<String>>(4).unwrap_or(None),
                ))
            },
        );
        if let Ok((eid, name, roles, delta, i)) = row {
            let affected: Value = roles.as_deref().and_then(|s| serde_json::from_str(s).ok()).unwrap_or_else(|| json!([]));
            let d: Value = delta.as_deref().and_then(|s| serde_json::from_str(s).ok()).unwrap_or_else(|| json!({}));
            let iv: f64 = i.as_deref().and_then(|s| s.parse().ok()).unwrap_or(0.5);
            out.push(json!({
                "event_id": eid,
                "name": name,
                "affected_roles": affected,
                "emotion_delta": d,
                "i": iv,
            }));
        }
    }
    out
}

/// 读上一章情绪状态（char_emotion 最新行 + char_memory），无则用标定初值。
fn read_prev_state(con: &Connection, root: &Path, code: &str, chapter: u32) -> Value {
    let mut state = emotion::initial_state(code, "");
    let has_hist;
    if let Ok(mut st) = con.prepare(
        "SELECT chapter,anger,fear,grief,joy,anxiety,calm,shame,awe,compassion,hope,strength,suppress \
         FROM char_emotion WHERE char_code=?1 AND chapter<?2 ORDER BY chapter DESC LIMIT 1",
    ) {
        if let Ok(row) = st.query_row(rusqlite::params![code, chapter as i64], |r| {
            let dims = ["anger", "fear", "grief", "joy", "anxiety", "calm", "shame", "awe", "compassion", "hope"];
            let mut em = Map::new();
            for (i, dim) in dims.iter().enumerate() {
                em.insert(dim.to_string(), json!(r.get::<_, f64>(i + 1).unwrap_or(50.0)));
            }
            Ok(json!({
                "chapter": r.get::<_, i64>(0).unwrap_or(0),
                "emotions": Value::Object(em),
                "strength": r.get::<_, f64>(11).unwrap_or(0.0),
                "suppress": r.get::<_, f64>(12).unwrap_or(1.0),
            }))
        }) {
            state["chapter"] = row["chapter"].clone();
            state["emotions"] = row["emotions"].clone();
            state["strength"] = row["strength"].clone();
            state["suppress"] = row["suppress"].clone();
            has_hist = true;
        } else {
            has_hist = false;
        }
    } else {
        has_hist = false;
    }
    // 无历史：用角色标定的 emotion_base 作初值（否则从 0 起步不合理）
    if !has_hist {
        if let Some(cal) = read_calib(root, code) {
            if let Some(base) = cal.get("emotion_base").and_then(|v| v.as_object()) {
                let mut em = Map::new();
                for d in ["anger", "fear", "grief", "joy", "anxiety", "calm", "shame", "awe", "compassion", "hope"] {
                    em.insert(d.to_string(), json!(base.get(d).and_then(|v| v.as_f64()).unwrap_or(50.0)));
                }
                state["emotions"] = Value::Object(em);
                let s: f64 = state["emotions"].as_object().map(|o| o.values().filter_map(|v| v.as_f64()).map(|x| x*x).sum::<f64>().sqrt()).unwrap_or(0.0);
                state["strength"] = json!(s);
                if let Some(sup) = cal.get("suppress").and_then(|v| v.as_f64()) {
                    state["suppress"] = json!(sup);
                }
            }
        }
    }
    if let Ok(mut st) = con.prepare(
        "SELECT event_id,source_chapter,dim,e0 FROM char_memory WHERE char_code=?1",
    ) {
        if let Ok(iter) = st.query_map(rusqlite::params![code], |r| {
            Ok(json!({
                "char_code": code,
                "event_id": r.get::<_, String>(0).unwrap_or_default(),
                "source_chapter": r.get::<_, i64>(1).unwrap_or(0),
                "dim": r.get::<_, String>(2).unwrap_or_default(),
                "e0": r.get::<_, f64>(3).unwrap_or(0.0),
            }))
        }) {
            let mems: Vec<Value> = iter.flatten().collect();
            if !mems.is_empty() {
                state["memories"] = json!(mems);
            }
        }
    }
    let _ = (root, chapter);
    state
}

/// 读角色标定（emotion_base -> 初值；suppress；lambda）。
fn read_calib(root: &Path, code: &str) -> Option<Value> {
    for f in ["char_calib_99.json", "char_calib_20_70.json", "char_calib_longtail.json"] {
        let v = jread(root, &format!("05_管线框架/data/{f}"))?;
        let arr = v.get("characters")?.as_array()?;
        for c in arr {
            if c.get("code").and_then(|x| x.as_str()) != Some(code) {
                continue;
            }
            let sup = c.get("suppress").and_then(|x| x.get("value")).and_then(|x| x.as_f64()).unwrap_or(1.0);
            let lam = match c.get("lambda").and_then(|x| x.get("value")).and_then(|x| x.as_str()) {
                Some("long") => 0.95,
                Some("mid") => 0.85,
                Some("short") => 0.7,
                _ => 0.85,
            };
            // emotion_base -> 10 维初值（无历史时用）
            let mut base = Map::new();
            if let Some(o) = c.get("emotion_base").and_then(|x| x.as_object()) {
                for (k, v) in o {
                    if let Some(n) = v.get("value").and_then(|x| x.as_f64()) {
                        base.insert(k.clone(), json!(n));
                    }
                }
            }
            return Some(json!({
                "personality": {},
                "suppress": sup,
                "lambda_mem": lam,
                "emotion_base": Value::Object(base),
            }));
        }
    }
    None
}

/// 写 char_emotion（含 show 列与 suppress）。
fn write_emotion(con: &Connection, code: &str, ns: &Value, chapter: u32) -> Result<()> {
    let dims = ["anger", "fear", "grief", "joy", "anxiety", "calm", "shame", "awe", "compassion", "hope"];
    let em = &ns["emotions"];
    let sh = &ns["emotions_show"];
    let mut vals: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    let mut cols = vec!["char_code", "char_name", "chapter"];
    vals.push(Box::new(code.to_string()));
    vals.push(Box::new(String::new()));
    vals.push(Box::new(chapter as i64));
    for d in dims {
        cols.push(d);
        vals.push(Box::new(em.get(d).and_then(|v| v.as_f64()).unwrap_or(0.0)));
    }
    for d in dims {
        let c = match d {
            "anger" => "anger_show", "fear" => "fear_show", "grief" => "grief_show",
            "joy" => "joy_show", "anxiety" => "anxiety_show", "calm" => "calm_show",
            "shame" => "shame_show", "awe" => "awe_show", "compassion" => "compassion_show",
            _ => "hope_show",
        };
        cols.push(c);
        vals.push(Box::new(sh.get(d).and_then(|v| v.as_f64()).unwrap_or(0.0)));
    }
    cols.push("dominant");
    vals.push(Box::new(ns["dominant"].as_str().map(|s| s.to_string())));
    cols.push("strength");
    vals.push(Box::new(ns["strength"].as_f64().unwrap_or(0.0)));
    cols.push("suppress");
    vals.push(Box::new(ns["suppress"].as_f64().unwrap_or(1.0)));
    cols.push("basis");
    vals.push(Box::new(format!("wire.commit: 由第{chapter}章事件递推（情绪引擎）")));
    let ph = (1..=cols.len()).map(|i| format!("?{i}")).collect::<Vec<_>>().join(",");
    let sql = format!("INSERT OR REPLACE INTO char_emotion({}) VALUES({})", cols.join(","), ph);
    let refs: Vec<&dyn rusqlite::ToSql> = vals.iter().map(|b| b.as_ref()).collect();
    con.execute(&sql, refs.as_slice())?;
    Ok(())
}

/// 写 char_memory（新记忆追加）。
fn write_memory(con: &Connection, code: &str, ns: &Value, chapter: u32) -> Result<()> {
    let Some(mems) = ns.get("memories").and_then(|v| v.as_array()) else { return Ok(()) };
    for m in mems {
        let eid = m.get("event_id").and_then(|v| v.as_str()).unwrap_or("");
        let dim = m.get("dim").and_then(|v| v.as_str()).unwrap_or("");
        if eid.is_empty() || dim.is_empty() {
            continue;
        }
        let _ = con.execute(
            "INSERT OR IGNORE INTO char_memory(char_code,event_id,source_chapter,dim,e0,e_t,lambda_mem,basis)
             VALUES(?1,?2,?3,?4,?5,?6,0.85,?7)",
            rusqlite::params![
                code, eid, chapter as i64, dim,
                m.get("e0").and_then(|v| v.as_f64()).unwrap_or(0.0),
                m.get("e_t").and_then(|v| v.as_f64()),
                format!("wire.commit: 第{chapter}章事件记忆"),
            ],
        );
    }
    Ok(())
}
