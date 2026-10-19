//! 关系网（world-gov，第 14 批）—— 从卡片正文抽取全局引用边，建**只读**关系图。
//!
//! # 定位
//! 遍历 `library_index` 的每张卡，读取其源文件正文，用 `extract_codes` 抽出其中
//! 出现的全局编号，形成有向边 `src_key → dst_key`。据此可查：
//! 悬空引用（指向不存在的卡）、孤立卡（无任何入边）、上下游邻居、最短引用链、循环引用。
//!
//! # 存储（复用 novel.db，与主索引隔离）
//! `rel_edge(id, src_key, dst_key, observed_in, ts)`
//! `rel_graph_meta(id=1, snap_id, built_ts, nodes, edges, cards_scanned, dangling)`
//! - 均为幂等建表；`build` 时**事务清空重建**，绝不改 `library_index` 与源文件。
//!
//! # 铁则
//! - ❌ 不修改资料库正文、不改 `library_index`。
//! - ✅ 只读源文件 → 写**隔离辅助表**。
//! - ✅ 零外呼；零真实 LLM；不做自动判定（只产证据）。
use anyhow::Result;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;

use super::extract_codes;

const SCHEMA: &str = r#"
-- 关系网引用边（2026-10-14，第 14 批）—— 幂等；与主索引隔离。
CREATE TABLE IF NOT EXISTS rel_edge (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    src_key     TEXT NOT NULL,
    dst_key     TEXT NOT NULL,
    observed_in TEXT NOT NULL DEFAULT '',
    ts          INTEGER NOT NULL,
    UNIQUE(src_key, dst_key)
);
CREATE INDEX IF NOT EXISTS idx_rel_edge_src ON rel_edge(src_key);
CREATE INDEX IF NOT EXISTS idx_rel_edge_dst ON rel_edge(dst_key);
CREATE TABLE IF NOT EXISTS rel_graph_meta (
    id           INTEGER PRIMARY KEY CHECK (id=1),
    snap_id      TEXT,
    built_ts     INTEGER NOT NULL,
    nodes        INTEGER NOT NULL DEFAULT 0,
    edges        INTEGER NOT NULL DEFAULT 0,
    cards_scanned INTEGER NOT NULL DEFAULT 0,
    dangling     INTEGER NOT NULL DEFAULT 0
);
"#;

/// 幂等建表。
pub fn ensure_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA)?;
    Ok(())
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// 叙事层节点白名单（R-04）。
///
/// `rel_edge` 的 `src_key` 里混了 26 个非卡号节点，实测全部是**叙事层文件名**：
/// `00_整本概括` / `00_故事脊梁` / `00_宏大对抗总规划` / `00_叙事总纲` / `_全局角色档案库` /
/// `卷01`…`卷10` / `卷NN·卷主体故事` / `护栏·规划约束` / `跨卷线锚点表`。
///
/// 这些是 `library_index` 里 layer=L0/L0.5/L1/L1x/L2 的**合法条目**（叙事层本来就不是卡片），
/// 由 `graph build` 从 `library_index` 全量扫描自然产生。它们不是脏数据，
/// 但不能参与「悬空引用」统计——否则每次体检都会报 26 个假悬空。
///
/// 判据刻意用**显式枚举 + 结构性模式**，不靠模糊匹配，避免把未来某张卡片误判成叙事层。
pub fn is_narrative_node(key: &str) -> bool {
    // ① 显式清单（当前库里出现的全部 8 个固定名）
    const FIXED: [&str; 8] = [
        "00_整本概括",
        "00_故事脊梁",
        "00_宏大对抗总规划",
        "00_叙事总纲",
        "_全局角色档案库",
        "护栏·规划约束",
        "跨卷线锚点表",
        "L0.5_故事脊梁",
    ];
    if FIXED.contains(&key) {
        return true;
    }
    // ② 结构性模式：`卷NN` 或 `卷NN·卷主体故事`（卷号必须两位数字）
    // 注意用 **chars 计数**而非字节数：「卷01」5 字节 / 「卷01·卷主体故事」21 字节，
    // 按字节判 `>=6` 会把最短的「卷01」漏掉（写错过一次，已由单测兜住）。
    let chars: Vec<char> = key.chars().collect();
    if chars.len() >= 3 && chars[0] == '卷' && chars[1].is_ascii_digit() && chars[2].is_ascii_digit() {
        let rest: String = chars[3..].iter().collect();
        return rest.is_empty() || rest == "·卷主体故事";
    }
    false
}

/// 一张卡的引用信息（纯结构，供 build 与测试复用）。
#[derive(Debug, Clone)]
pub struct CardRefs {
    pub key: String,
    pub layer: String,
    pub source: String,
    /// 正文中抽出的引用编号（已去自引用、去重、排序）。
    pub refs: Vec<String>,
}

/// 纯函数：从 (key, layer, source, 正文) 计算出该卡的引用列表（去自引用/去重/排序）。
pub fn refs_of_card(key: &str, text: &str) -> Vec<String> {
    let mut set: BTreeSet<String> = extract_codes(text).into_iter().collect();
    set.remove(key);
    set.into_iter().collect()
}

/// 构建关系图：遍历 library_index 每张卡读源文件 → 抽引用 → 事务重建 rel_edge。
/// `max_cards` 可限流（None=全量）。返回统计 Value。
pub fn build(conn: &Connection, lib_root: &Path, max_cards: Option<usize>, dry_run: bool, snap_id: Option<&str>) -> Result<Value> {
    ensure_table(conn)?;
    // ① 读全部卡（key, layer, source）
    let mut stmt = conn.prepare("SELECT key,layer,IFNULL(source,'') FROM library_index ORDER BY key")?;
    let cards: Vec<(String, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .filter_map(|x| x.ok())
        .collect();
    let known: HashSet<String> = cards.iter().map(|c| c.0.clone()).collect();

    let mut per_card: Vec<CardRefs> = Vec::new();
    let mut scanned = 0usize;
    let mut unreadable = 0usize;
    for (key, layer, source) in &cards {
        if let Some(m) = max_cards {
            if scanned >= m { break; }
        }
        scanned += 1;
        let path = lib_root.join(source);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => {
                unreadable += 1;
                String::new()
            }
        };
        per_card.push(CardRefs { key: key.clone(), layer: layer.clone(), source: source.clone(), refs: refs_of_card(key, &text) });
    }

    // ② 统计
    let edges: Vec<(String, String, String)> = per_card
        .iter()
        .flat_map(|c| c.refs.iter().map(move |d| (c.key.clone(), d.clone(), c.source.clone())))
        .collect();
    let node_set: BTreeSet<String> = edges
        .iter()
        .flat_map(|(s, d, _)| [s.clone(), d.clone()])
        .collect();
    // 悬空号分类（R-04 扩展）：
    //   · card_dangling：被**卡片**引用的不存在编号 → 真故障，必须修
    //   · narrative_only：只被**叙事层文档**引用的不存在编号 → 多为例子/占位
    //     （实测 `护栏·规划约束.md:16` 写「如 TECH-99-006 / CHAR-99-001 / LOCA-99-001 / CIVI-99-001」，
    //      那是格式范例不是真实引用，LOCA-99-001 从未建卡；混进 dangling 就是永久假阳性）
    // 两者都返回，但只有前者参与 `dangling` 计数，避免体检永远红着。
    let mut card_dangling: BTreeSet<String> = BTreeSet::new();
    let mut narrative_only: BTreeSet<String> = BTreeSet::new();
    for (s, d, _) in &edges {
        if known.contains(d) || is_narrative_node(d) {
            continue;
        }
        if is_narrative_node(s) {
            narrative_only.insert(d.clone());
        } else {
            card_dangling.insert(d.clone());
        }
    }
    // 只被叙事层引用的，若同时被卡片引用，则升级为 card_dangling
    narrative_only.retain(|d| !card_dangling.contains(d));
    let dangling: Vec<String> = card_dangling.iter().cloned().collect();

    if dry_run {
        return Ok(json!({
            "command": "world-gov graph build",
            "dry_run": true,
            "cards_scanned": scanned,
            "unreadable": unreadable,
            "nodes": node_set.len(),
            "edges": edges.len(),
            "dangling": dangling.clone(),
            "dangling_unique": dangling.len(),
            "narrative_only": narrative_only.iter().cloned().collect::<Vec<_>>(),
            "note": "只统计未落库。",
        }));
    }

    // ③ 事务重建（只动隔离表）
    let ts = now_secs();
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM rel_edge", [])?;
    {
        let mut ins = tx.prepare(
            "INSERT OR IGNORE INTO rel_edge(src_key,dst_key,observed_in,ts) VALUES(?1,?2,?3,?4)",
        )?;
        for (s, d, src) in &edges {
            ins.execute(params![s, d, src, ts])?;
        }
    }
    let real_edges: i64 = tx.query_row("SELECT COUNT(*) FROM rel_edge", [], |r| r.get(0)).unwrap_or(0);
    tx.execute(
        "INSERT INTO rel_graph_meta(id,snap_id,built_ts,nodes,edges,cards_scanned,dangling)
         VALUES(1,?1,?2,?3,?4,?5,?6)
         ON CONFLICT(id) DO UPDATE SET snap_id=excluded.snap_id, built_ts=excluded.built_ts,
           nodes=excluded.nodes, edges=excluded.edges, cards_scanned=excluded.cards_scanned, dangling=excluded.dangling",
        params![snap_id, ts, node_set.len() as i64, real_edges, scanned as i64, dangling.len() as i64],
    )?;
    tx.commit()?;

    Ok(json!({
        "command": "world-gov graph build",
        "snapshot": snap_id,
        "cards_scanned": scanned,
        "unreadable": unreadable,
        "nodes": node_set.len(),
        "edges": real_edges,
        "dangling": dangling.clone(),
        "dangling_unique": dangling.len(),
        "narrative_only": narrative_only.iter().cloned().collect::<Vec<_>>(),
        "note": "只写隔离表 rel_edge/rel_graph_meta；未改 library_index 与源文件。",
    }))
}

fn meta(conn: &Connection) -> Value {
    conn.query_row(
        "SELECT IFNULL(snap_id,''),built_ts,nodes,edges,cards_scanned,dangling FROM rel_graph_meta WHERE id=1",
        [],
        |r| Ok(json!({
            "snap_id": r.get::<_,String>(0)?,
            "built_ts": r.get::<_,i64>(1)?,
            "nodes": r.get::<_,i64>(2)?,
            "edges": r.get::<_,i64>(3)?,
            "cards_scanned": r.get::<_,i64>(4)?,
            "dangling": r.get::<_,i64>(5)?,
        })),
    ).unwrap_or(json!({"built": false, "hint": "先跑 world-gov graph build"}))
}

/// 读取全部边（src,dst）。供内存图算法复用。
pub fn load_edges(conn: &Connection) -> Result<Vec<(String, String)>> {
    ensure_table(conn)?;
    let mut stmt = conn.prepare("SELECT src_key,dst_key FROM rel_edge")?;
    let rows: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .filter_map(|x| x.ok())
        .collect();
    Ok(rows)
}

/// 邻接表（出边）。
pub fn adjacency(edges: &[(String, String)]) -> HashMap<String, Vec<String>> {
    let mut m: HashMap<String, Vec<String>> = HashMap::new();
    for (s, d) in edges {
        m.entry(s.clone()).or_default().push(d.clone());
    }
    for v in m.values_mut() {
        v.sort();
        v.dedup();
    }
    m
}

/// 孤立卡：库内 L4/AUX 卡中，无任何入边者（含**完全不参与引用**的卡）。
/// 定义唯一：`summary` 与 `orphans` 命令共用，保证两处数字一致。
fn orphan_keys(conn: &Connection, edges: &[(String, String)]) -> Result<Vec<String>> {
    let known = known_keys(conn)?;
    let layers = layer_of(conn)?;
    let mut in_deg: HashSet<String> = HashSet::new();
    for (_, d) in edges {
        in_deg.insert(d.clone());
    }
    let mut items: Vec<String> = known
        .iter()
        .filter(|k| !in_deg.contains(*k))
        .filter(|k| matches!(layers.get(*k).map(|s| s.as_str()), Some("L4") | Some("AUX")))
        .cloned()
        .collect();
    items.sort();
    Ok(items)
}

/// 统计摘要（节点/边/悬空/孤立/最大出入度 top）。
pub fn summary(conn: &Connection, top: usize) -> Result<Value> {
    let edges = load_edges(conn)?;
    let known: HashSet<String> = known_keys(conn)?;
    let mut out_deg: BTreeMap<String, usize> = BTreeMap::new();
    let mut in_deg: BTreeMap<String, usize> = BTreeMap::new();
    let mut dangling: BTreeSet<String> = BTreeSet::new();
    for (s, d) in &edges {
        *out_deg.entry(s.clone()).or_default() += 1;
        *in_deg.entry(d.clone()).or_default() += 1;
        if !known.contains(d) {
            dangling.insert(d.clone());
        }
    }
    let nodes: BTreeSet<String> = edges.iter().flat_map(|(s, d)| [s.clone(), d.clone()]).collect();
    // 孤立卡：与 `orphans` 命令同定义（全库 L4/AUX 卡中无入边者）
    let orphans = orphan_keys(conn, &edges)?;

    let top_n = |m: &BTreeMap<String, usize>| -> Vec<Value> {
        let mut v: Vec<(&String, &usize)> = m.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        v.into_iter().take(top).map(|(k, n)| json!({"key": k, "degree": n})).collect()
    };

    Ok(json!({
        "command": "world-gov graph summary",
        "meta": meta(conn),
        "nodes": nodes.len(),
        "edges": edges.len(),
        "dangling_unique": dangling.len(),
        "orphans": orphans.len(),
        "top_out_degree": top_n(&out_deg),
        "top_in_degree": top_n(&in_deg),
        "note": "只读统计；悬空=指向不存在的卡；孤立=L4/AUX 卡无入边（与 graph orphans 同定义）。",
    }))
}

fn known_keys(conn: &Connection) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare("SELECT key FROM library_index")?;
    let s: HashSet<String> = stmt.query_map([], |r| r.get::<_, String>(0))?.filter_map(|x| x.ok()).collect();
    Ok(s)
}
fn layer_of(conn: &Connection) -> Result<HashMap<String, String>> {
    let mut stmt = conn.prepare("SELECT key,layer FROM library_index")?;
    let m: HashMap<String, String> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .filter_map(|x| x.ok())
        .collect();
    Ok(m)
}

/// 悬空引用清单（指向不存在的卡，附来源卡）。
pub fn dangling(conn: &Connection, limit: usize) -> Result<Value> {
    let edges = load_edges(conn)?;
    let known = known_keys(conn)?;
    let mut by_dst: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (s, d) in &edges {
        if !known.contains(d) {
            by_dst.entry(d.clone()).or_default().insert(s.clone());
        }
    }
    let items: Vec<Value> = by_dst
        .iter()
        .take(limit)
        .map(|(d, srcs)| json!({"missing_key": d, "referenced_by": srcs.iter().cloned().collect::<Vec<_>>()}))
        .collect();
    Ok(json!({
        "command": "world-gov graph dangling",
        "total": by_dst.len(),
        "shown": items.len(),
        "items": items,
        "note": "这些编号被别的卡引用，但 library_index 无此卡（可能未建卡或编号笔误）。",
    }))
}

/// 孤立卡清单（无入边）。
pub fn orphans(conn: &Connection, limit: usize) -> Result<Value> {
    let edges = load_edges(conn)?;
    let mut items = orphan_keys(conn, &edges)?;
    let total = items.len();
    items.truncate(limit);
    Ok(json!({
        "command": "world-gov graph orphans",
        "total": total, "shown": items.len(), "items": items,
        "note": "无任何其他卡引用它们（L4/AUX 卡）；是否为问题由人判断。",
    }))
}

/// 某卡的上/下游邻居。
pub fn neighbors(conn: &Connection, key: &str, direction: &str) -> Result<Value> {
    let edges = load_edges(conn)?;
    let dir = direction.trim().to_ascii_lowercase();
    let mut out: BTreeSet<String> = BTreeSet::new();
    let mut inc: BTreeSet<String> = BTreeSet::new();
    for (s, d) in &edges {
        if s == key { out.insert(d.clone()); }
        if d == key { inc.insert(s.clone()); }
    }
    let known = known_keys(conn)?;
    let mark = |v: &BTreeSet<String>| -> Vec<Value> {
        v.iter().map(|k| json!({"key": k, "exists": known.contains(k)})).collect()
    };
    let show_out = dir.is_empty() || dir == "out" || dir == "down";
    let show_in = dir.is_empty() || dir == "in" || dir == "up";
    Ok(json!({
        "command": "world-gov graph neighbors",
        "key": key,
        "downstream_refs": if show_out { json!(mark(&out)) } else { json!(null) },
        "upstream_refs": if show_in { json!(mark(&inc)) } else { json!(null) },
        "note": "downstream=该卡引用的卡；upstream=引用该卡的卡。exists=false 即悬空。",
    }))
}

/// 最短引用链（沿出边 BFS）：from → … → to。max_depth 限制（默认 6）。
pub fn path(conn: &Connection, from: &str, to: &str, max_depth: usize) -> Result<Value> {
    let edges = load_edges(conn)?;
    let adj = adjacency(&edges);
    let mut prev: HashMap<String, String> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut q: VecDeque<(String, usize)> = VecDeque::new();
    q.push_back((from.to_string(), 0));
    seen.insert(from.to_string());
    let mut found = false;
    while let Some((cur, d)) = q.pop_front() {
        if cur == to { found = true; break; }
        if d >= max_depth { continue; }
        if let Some(nexts) = adj.get(&cur) {
            for n in nexts {
                if seen.insert(n.clone()) {
                    prev.insert(n.clone(), cur.clone());
                    q.push_back((n.clone(), d + 1));
                }
            }
        }
    }
    if !found {
        return Ok(json!({
            "command": "world-gov graph path", "from": from, "to": to,
            "found": false, "max_depth": max_depth,
            "note": "在给定深度内无引用链（或需先 graph build）。",
        }));
    }
    // 回溯
    let mut chain = vec![to.to_string()];
    let mut cur = to.to_string();
    while let Some(p) = prev.get(&cur) {
        chain.push(p.clone());
        cur = p.clone();
        if cur == from { break; }
    }
    chain.reverse();
    Ok(json!({
        "command": "world-gov graph path", "from": from, "to": to,
        "found": true, "len": chain.len(), "chain": chain,
    }))
}

/// 循环引用检测（有向环，长度 ≤ max_len；去重规范化）。
pub fn cycles(conn: &Connection, max_len: usize, limit: usize) -> Result<Value> {
    let edges = load_edges(conn)?;
    let adj = adjacency(&edges);
    let mut found: BTreeSet<Vec<String>> = BTreeSet::new();
    let nodes: Vec<String> = adj.keys().cloned().collect();
    for start in &nodes {
        // DFS 从 start 出发，找回到 start 的简单环，长度 ≤ max_len
        let mut stack: Vec<(String, Vec<String>)> = vec![(start.clone(), vec![start.clone()])];
        while let Some((cur, pathv)) = stack.pop() {
            if pathv.len() > max_len { continue; }
            if let Some(nexts) = adj.get(&cur) {
                for n in nexts {
                    if n == start && pathv.len() >= 2 {
                        found.insert(canonical_cycle(&pathv));
                        continue;
                    }
                    if !pathv.contains(n) {
                        let mut np = pathv.clone();
                        np.push(n.clone());
                        stack.push((n.clone(), np));
                    }
                }
            }
        }
        if found.len() >= limit { break; }
    }
    let items: Vec<Value> = found.iter().take(limit).map(|c| json!(c)).collect();
    Ok(json!({
        "command": "world-gov graph cycles",
        "total": found.len(), "shown": items.len(), "max_len": max_len,
        "items": items,
        "note": "引用关系出现环（可能互为法理依据或循环依赖）；是否问题由人判断。",
    }))
}

/// 规范化环：旋转到字典序最小的起点（保证同一环只记一次）。
pub fn canonical_cycle(cycle: &[String]) -> Vec<String> {
    if cycle.is_empty() { return Vec::new(); }
    let min_pos = cycle.iter().enumerate().min_by_key(|(_, v)| *v).map(|(i, _)| i).unwrap_or(0);
    let mut out: Vec<String> = Vec::with_capacity(cycle.len());
    for i in 0..cycle.len() {
        out.push(cycle[(min_pos + i) % cycle.len()].clone());
    }
    out
}

/// 列出边（可按 src 过滤）。
pub fn list(conn: &Connection, key: Option<&str>, limit: usize) -> Result<Value> {
    ensure_table(conn)?;
    let (sql, binds): (&str, Vec<String>) = match key {
        Some(k) if !k.trim().is_empty() => (
            "SELECT src_key,dst_key,observed_in FROM rel_edge WHERE src_key=?1 ORDER BY dst_key LIMIT ?2",
            vec![k.trim().to_string(), limit.to_string()],
        ),
        _ => (
            "SELECT src_key,dst_key,observed_in FROM rel_edge ORDER BY src_key,dst_key LIMIT ?1",
            vec![limit.to_string()],
        ),
    };
    let mut stmt = conn.prepare(sql)?;
    let rows: Vec<Value> = if binds.len() == 2 {
        stmt.query_map(params![binds[0], limit as i64], |r| {
            Ok(json!({"src": r.get::<_, String>(0)?, "dst": r.get::<_, String>(1)?, "observed_in": r.get::<_, String>(2)?}))
        })?.filter_map(|x| x.ok()).collect()
    } else {
        stmt.query_map(params![limit as i64], |r| {
            Ok(json!({"src": r.get::<_, String>(0)?, "dst": r.get::<_, String>(1)?, "observed_in": r.get::<_, String>(2)?}))
        })?.filter_map(|x| x.ok()).collect()
    };
    Ok(json!({"command": "world-gov graph list", "shown": rows.len(), "edges": rows}))
}

/// 供 self-check：结构 + 脏检查（只读）。
pub fn table_check(conn: &Connection) -> Result<Value> {
    let exists: i64 = conn
        .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='rel_edge'", [], |r| r.get(0))
        .unwrap_or(0);
    if exists == 0 {
        return Ok(json!({"ok": false, "missing_table": "rel_edge", "hint": "跑任意 world-gov 命令（open_gov 幂等建表）即可创建"}));
    }
    let edges: i64 = conn.query_row("SELECT COUNT(*) FROM rel_edge", [], |r| r.get(0)).unwrap_or(0);
    let mut dirty: Vec<Value> = Vec::new();
    let mut stmt = conn.prepare("SELECT src_key,dst_key FROM rel_edge WHERE src_key=dst_key LIMIT 20")?;
    for x in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.filter_map(|v| v.ok()) {
        dirty.push(json!({"src": x.0, "dst": x.1, "why": "self_loop"}));
    }
    // R-04：叙事层节点统计（不是脏数据，但要能看见，才不会被误当悬空）
    let mut narrative: Vec<String> = Vec::new();
    let mut st2 = conn.prepare("SELECT DISTINCT src_key FROM rel_edge")?;
    for k in st2.query_map([], |r| r.get::<_, String>(0))?.filter_map(|v| v.ok()) {
        if is_narrative_node(&k) {
            narrative.push(k);
        }
    }
    narrative.sort();
    Ok(json!({
        "ok": dirty.is_empty(),
        "edges": edges,
        "dirty": dirty,
        "built": meta(conn)["built_ts"].is_i64(),
        // 26 个叙事层节点（卷01…卷10 / L0-L2 规划件），设计如此，已白名单
        "narrative_nodes": {"count": narrative.len(), "items": narrative},
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        ensure_table(&c).unwrap();
        c
    }

    #[test]
    fn table_isolated_and_idempotent() {
        let c = Connection::open_in_memory().unwrap();
        ensure_table(&c).unwrap();
        ensure_table(&c).unwrap();
        let names: Vec<String> = c
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .filter_map(|v| v.ok())
            .collect();
        assert!(names.contains(&"rel_edge".to_string()));
        assert!(!names.contains(&"library_index".to_string()));
    }

    #[test]
    fn refs_of_card_drops_self_and_dedups() {
        let t = "见 CHAR-90-038 与 CHAR-90-038；再看 LOCA-99-001。本卡 CHAR-99-001 自引用。";
        let r = refs_of_card("CHAR-99-001", t);
        assert_eq!(r, vec!["CHAR-90-038".to_string(), "LOCA-99-001".to_string()]);
    }

    #[test]
    fn adjacency_dedups_and_sorts() {
        let e = vec![
            ("A".to_string(), "B".to_string()),
            ("A".to_string(), "B".to_string()),
            ("A".to_string(), "C".to_string()),
        ];
        let adj = adjacency(&e);
        assert_eq!(adj.get("A").unwrap(), &vec!["B".to_string(), "C".to_string()]);
    }

    #[test]
    fn canonical_cycle_rotation() {
        let c = vec!["B".to_string(), "C".to_string(), "A".to_string()];
        assert_eq!(canonical_cycle(&c), vec!["A".to_string(), "B".to_string(), "C".to_string()]);
        let c2 = vec!["C".to_string(), "A".to_string(), "B".to_string()];
        assert_eq!(canonical_cycle(&c2), vec!["A".to_string(), "B".to_string(), "C".to_string()]);
    }

    fn seed(c: &Connection) {
        c.execute_batch("CREATE TABLE library_index(key TEXT, layer TEXT, source TEXT);").unwrap();
        for (k, l) in [("A", "L4"), ("B", "L4"), ("C", "L4")] {
            c.execute("INSERT INTO library_index(key,layer,source) VALUES(?1,?2,'x.md')", params![k, l]).unwrap();
        }
        let ts = 1i64;
        for (s, d) in [("A", "B"), ("B", "C"), ("C", "B"), ("A", "ZZZ")] {
            c.execute("INSERT INTO rel_edge(src_key,dst_key,observed_in,ts) VALUES(?1,?2,'x.md',?3)", params![s, d, ts]).unwrap();
        }
    }

    #[test]
    fn dangling_and_orphans_and_summary() {
        let c = mem();
        seed(&c);
        let d = dangling(&c, 50).unwrap();
        assert_eq!(d["total"], 1);
        assert_eq!(d["items"][0]["missing_key"], "ZZZ");
        let o = orphans(&c, 50).unwrap();
        // A 无入边 → 孤立；B/C 有入边
        assert!(o["items"].as_array().unwrap().iter().any(|x| x == "A"));
        let s = summary(&c, 5).unwrap();
        assert_eq!(s["dangling_unique"], 1);
        assert!(s["nodes"].as_u64().unwrap() >= 3);
        // 一致性：summary.orphans 与 orphans.total 必须同定义、同数字
        assert_eq!(s["orphans"].as_u64().unwrap(), o["total"].as_u64().unwrap());
    }

    #[test]
    fn neighbors_marks_existence() {
        let c = mem();
        seed(&c);
        let n = neighbors(&c, "A", "out").unwrap();
        let out = n["downstream_refs"].as_array().unwrap();
        assert!(out.iter().any(|x| x["key"] == "B" && x["exists"] == true));
        assert!(out.iter().any(|x| x["key"] == "ZZZ" && x["exists"] == false));
        let n2 = neighbors(&c, "A", "in").unwrap();
        assert!(n2["upstream_refs"].as_array().unwrap().is_empty());
    }

    #[test]
    fn path_bfs_finds_shortest() {
        let c = mem();
        seed(&c);
        let p = path(&c, "A", "C", 6).unwrap();
        assert_eq!(p["found"], true);
        assert_eq!(p["chain"].as_array().unwrap().len(), 3); // A→B→C
        let p2 = path(&c, "C", "A", 6).unwrap();
        assert_eq!(p2["found"], false);
    }

    #[test]
    fn cycles_detects_b_c_loop() {
        let c = mem();
        seed(&c);
        let cy = cycles(&c, 6, 50).unwrap();
        let items = cy["items"].as_array().unwrap();
        assert!(items.iter().any(|x| x.as_array().unwrap() == &vec![json!("B"), json!("C")]));
    }

    #[test]
    fn table_check_missing_then_present() {
        let c = Connection::open_in_memory().unwrap();
        assert_eq!(table_check(&c).unwrap()["ok"], false);
        let c2 = mem();
        assert_eq!(table_check(&c2).unwrap()["ok"], true);
    }

    #[test]
    fn summary_missing_meta_is_graceful() {
        let c = mem();
        c.execute_batch("CREATE TABLE library_index(key TEXT, layer TEXT, source TEXT);").unwrap();
        let s = summary(&c, 3).unwrap();
        assert_eq!(s["edges"], 0);
    }

    /// R-04 回归：叙事层节点必须被白名单识别，卡号必须**不被**误判。
    #[test]
    fn narrative_node_whitelist_precise() {
        // 显式清单
        for k in [
            "00_整本概括", "00_故事脊梁", "00_宏大对抗总规划", "00_叙事总纲",
            "_全局角色档案库", "护栏·规划约束", "跨卷线锚点表",
        ] {
            assert!(is_narrative_node(k), "应识别为叙事层：{k}");
        }
        // 卷号两种形态（含最短的「卷01」，曾因按字节判长而漏掉）
        for v in 1..=10 {
            assert!(is_narrative_node(&format!("卷{v:02}")), "应识别：卷{v:02}");
            assert!(is_narrative_node(&format!("卷{v:02}·卷主体故事")), "应识别：卷{v:02}·卷主体故事");
        }
        // 反例：卡号、以及其他以「卷」开头但不是卷号的串，一律不得误判
        for k in [
            "CHAR-99-001", "CIVI-99-002", "卷01·第3章", "卷1", "卷0A", "卷一", "卷",
            "卷01x", "00_叙事总纲·副本",
        ] {
            assert!(!is_narrative_node(k), "不得识别为叙事层：{k}");
        }
    }
}
