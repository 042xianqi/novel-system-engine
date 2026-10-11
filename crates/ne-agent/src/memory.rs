//! 记忆系统 —— **pinned（常驻）+ recall（按需）正交分层**。
//!
//! 参考（逐条对照太墟，见 docs/ref_taixu_harness_notes.md §3.2）：
//!   * `AgentContextEntities.kt:29` —— `AgentMemoryEntity` 数据模型
//!   * `AgentContextDao.kt:37` —— `getPinnedMemories`（**排序必须内容确定**）
//!   * `MemoryRecallSelector.kt` —— BM25 + CJK bigram 召回选择（纯函数）
//!   * `SystemPromptBuilder.kt:178` —— pinned 常驻 system 稳定前缀，recall 挂 user 轮
//!
//! # 为什么 pinned 排序不能用 updatedAt
//!
//! 太墟在 DAO 注释里专门警告过（`AgentContextDao.kt:31`）：pinned 直接决定 system prompt
//! 的字节，而"保存"会刷新 `updatedAt`（**哪怕内容一字未改**）。若按 updatedAt 排序，
//! 模型"重复保存同一条 pinned"这种无实质变化的行为也会重排列表、改变提示词字节，
//! **击穿其后全部对话的 prefix cache**。所以按 `(scope 权威序, createdAt, id)`——
//! createdAt 在覆盖时保持不变，重写既有条目不会移动位置。
//!
//! # 与召回的关系
//!
//!   * pinned：**总是**注入，走 system prompt 稳定前缀，最高权威
//!   * recall：**按需**注入，BM25 检索，挂最新 user 轮前缀，低权威
//!   * 二者正交：pinned 不参与召回（`pinnedIds` 排除），避免重复占预算
//!
//! # 与旧方案的本质区别
//!
//! 不做"无命中就注入最近记忆"——不相关记忆是噪声兼 token 浪费。
//! 必须常驻的指令请用 pinned。

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// 记忆库 schema。字段对齐太墟 `AgentMemoryEntity`。
pub const MEMORY_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS agent_memories (
    id              TEXT PRIMARY KEY,
    scope           TEXT NOT NULL,            -- global | project | session
    owner_id        TEXT NOT NULL DEFAULT '', -- global 空；project=workspace；session=sessionId
    kind            TEXT NOT NULL,            -- preference | rule | fact | project_info
    key             TEXT NOT NULL,
    value           TEXT NOT NULL,
    subject_key     TEXT NOT NULL DEFAULT '', -- 主题冲突去重键（dotted）
    revision        INTEGER NOT NULL DEFAULT 1,
    pinned          INTEGER NOT NULL DEFAULT 0,
    expires_at      INTEGER,                  -- null = 永不过期
    last_verified_at INTEGER NOT NULL DEFAULT 0,
    volatility      TEXT NOT NULL DEFAULT 'reference',
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_mem_scope_key
    ON agent_memories(scope, owner_id, key);
CREATE INDEX IF NOT EXISTS idx_mem_pinned ON agent_memories(pinned);
"#;

/// 一条记忆。
#[derive(Debug, Clone, PartialEq)]
pub struct Memory {
    pub id: String,
    pub scope: String,
    pub owner_id: String,
    pub kind: String,
    pub key: String,
    pub value: String,
    pub pinned: bool,
    pub expires_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// scope 取值。
pub const SCOPE_GLOBAL: &str = "global";
pub const SCOPE_PROJECT: &str = "project";
pub const SCOPE_SESSION: &str = "session";

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub struct MemoryStore {
    con: Connection,
    path: PathBuf,
}

impl MemoryStore {
    /// 打开（或创建）记忆库。`path` 与会话库同一个 `agent.db`。
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let con = Connection::open(&path)
            .with_context(|| format!("打开记忆库失败：{}", path.display()))?;
        // WAL：与会话库共享同一文件时降低锁竞争
        con.pragma_update(None, "journal_mode", "WAL").ok();
        con.execute_batch(MEMORY_SCHEMA).context("建记忆库 schema 失败")?;
        Ok(MemoryStore { con, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 保存/更新一条记忆（按 `(scope, owner_id, key)` 唯一）。
    ///
    /// 已存在则更新 value/revision/updated_at，**但 createdAt 不变**——
    /// 这是 pinned 排序稳定的前提。
    pub fn save(&self, m: &Memory) -> Result<()> {
        let t = now_ms();
        let existing: Option<i64> = self
            .con
            .query_row(
                "SELECT created_at FROM agent_memories WHERE scope=?1 AND owner_id=?2 AND key=?3",
                params![m.scope, m.owner_id, m.key],
                |r| r.get(0),
            )
            .optional()?;
        match existing {
            Some(created) => {
                self.con.execute(
                    "UPDATE agent_memories
                     SET value=?1, kind=?2, pinned=?3, expires_at=?4, updated_at=?5,
                         revision=revision+1
                     WHERE scope=?6 AND owner_id=?7 AND key=?8",
                    params![
                        m.value, m.kind, m.pinned as i32, m.expires_at, t,
                        m.scope, m.owner_id, m.key
                    ],
                )?;
                let _ = created; // createdAt 保持不动
            }
            None => {
                let id = if m.id.is_empty() {
                    format!("m{:x}-{}", t, short_hash(&format!("{}:{}:{}", m.scope, m.owner_id, m.key)))
                } else {
                    m.id.clone()
                };
                self.con.execute(
                    "INSERT INTO agent_memories
                     (id, scope, owner_id, kind, key, value, pinned, expires_at, created_at, updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?9)",
                    params![
                        id, m.scope, m.owner_id, m.kind, m.key, m.value,
                        m.pinned as i32, m.expires_at, t
                    ],
                )?;
            }
        }
        Ok(())
    }

    /// pinned 记忆：**注入 system prompt 稳定前缀**。
    ///
    /// ⚠ 排序必须**内容确定**（scope 权威序 → createdAt → id）。
    /// 见模块头注释：按 updatedAt 会让"重复保存同一 pinned"击穿 prefix cache。
    pub fn pinned(&self, project: &str, session: &str) -> Result<Vec<Memory>> {
        let mut st = self.con.prepare(
            "SELECT id,scope,owner_id,kind,key,value,pinned,expires_at,created_at,updated_at
             FROM agent_memories
             WHERE pinned=1 AND (
                   (scope='global'  AND owner_id='')
                OR (?1 != '' AND scope='project' AND owner_id=?1)
                OR (?2 != '' AND scope='session' AND owner_id=?2))
             ORDER BY CASE scope WHEN 'global' THEN 0 WHEN 'project' THEN 1 ELSE 2 END,
                      created_at ASC, id ASC",
        )?;
        let rows = st.query_map(params![project, session], row_to_memory)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// 召回候选：未过期、**非 pinned**（pinned 已常驻，不重复进召回）。
    pub fn candidates(&self, project: &str, session: &str, limit: usize) -> Result<Vec<Memory>> {
        let now = now_ms();
        let mut st = self.con.prepare(
            "SELECT id,scope,owner_id,kind,key,value,pinned,expires_at,created_at,updated_at
             FROM agent_memories
             WHERE (expires_at IS NULL OR expires_at > ?3)
               AND pinned=0
               AND ((scope='global' AND owner_id='')
                OR (?1 != '' AND scope='project' AND owner_id=?1)
                OR (?2 != '' AND scope='session' AND owner_id=?2))
             ORDER BY updated_at DESC, id ASC
             LIMIT ?4",
        )?;
        let rows = st.query_map(params![project, session, now, limit as i64], row_to_memory)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn all(&self) -> Result<Vec<Memory>> {
        let mut st = self.con.prepare(
            "SELECT id,scope,owner_id,kind,key,value,pinned,expires_at,created_at,updated_at
             FROM agent_memories ORDER BY scope, created_at, id",
        )?;
        let rows = st.query_map([], row_to_memory)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// 续期：确认仍然有效，刷新 `last_verified_at`（**新鲜度信号，不删除**）。
    pub fn touch(&self, id: &str) -> Result<()> {
        let now = now_ms();
        self.con.execute(
            "UPDATE agent_memories SET last_verified_at=?1
             WHERE id=?2 AND (expires_at IS NULL OR expires_at > ?1)",
            params![now, id],
        )?;
        Ok(())
    }

    pub fn delete(&self, key: &str, scope: &str, owner: &str) -> Result<bool> {
        let n = self.con.execute(
            "DELETE FROM agent_memories WHERE key=?1 AND scope=?2 AND owner_id=?3",
            params![key, scope, owner],
        )?;
        Ok(n > 0)
    }

    pub fn count(&self) -> Result<(usize, usize)> {
        let total: i64 = self
            .con
            .query_row("SELECT COUNT(*) FROM agent_memories", [], |r| r.get(0))?;
        let pinned: i64 = self.con.query_row(
            "SELECT COUNT(*) FROM agent_memories WHERE pinned=1",
            [],
            |r| r.get(0),
        )?;
        Ok((total as usize, pinned as usize))
    }
}

fn row_to_memory(r: &rusqlite::Row) -> rusqlite::Result<Memory> {
    Ok(Memory {
        id: r.get(0)?,
        scope: r.get(1)?,
        owner_id: r.get(2)?,
        kind: r.get(3)?,
        key: r.get(4)?,
        value: r.get(5)?,
        pinned: r.get::<_, i32>(6)? != 0,
        expires_at: r.get(7)?,
        created_at: r.get(8)?,
        updated_at: r.get(9)?,
    })
}

fn short_hash(s: &str) -> String {
    // 简单 FNV-1a，够用且无依赖
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:08x}", (h & 0xffff_ffff) as u32)
}

// ───────────────────────────── BM25 召回（纯函数，照太墟移植）

const K1: f64 = 1.2;
const B: f64 = 0.75;
/// 注入条数预算（对齐太墟 `MAX_RECALL_FACTS`）。
pub const MAX_RECALL_FACTS: usize = 5;
/// 单块字符预算（太墟 `MAX_RECALL_BLOCK_CHARS`）。
pub const MAX_RECALL_BLOCK_CHARS: usize = 4000;
/// 召回候选上限（太墟 `MAX_RECALL_CANDIDATES`）。
pub const MAX_CANDIDATES: usize = 64;

/// 泛化短语：短消息整体命中即不触发召回（"继续帮我改代码"不受影响）。
const GENERIC_PHRASES: &[&str] = &[
    "继续", "接着来", "然后呢", "好的", "收到", "可以", "开始吧", "行", "嗯", "哦",
    "continue", "go on", "keep going", "ok", "okay", "next", "yes",
];

/// 泛化 token：分词后逐个剔除，剩下的信息词才是有效查询。
const GENERIC_TERMS: &[&str] = &[
    "继续", "好的", "收到", "可以", "开始", "然后", "一下", "这个", "那个", "怎么", "什么",
    "帮忙", "请", "我", "你", "的", "了", "吗", "吧", "呢", "看", "讲", "说",
    "continue", "please", "the", "and", "what", "how",
];

/// 分词：拉丁/数字按整词小写；CJK 连续段切 **2-gram**（单字段保留单字）。
///
/// bigram 让"调用链"命中"调用链路分析"这类**真实词重叠**，
/// 同时避免 `LIKE '%测试%'` 被"测试服"这种散字误命中。
pub fn tokenize(text: &str) -> Vec<String> {
    let mut terms = Vec::new();
    for w in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
        if !w.is_empty() {
            terms.push(w.to_ascii_lowercase());
        }
    }
    // CJK 连续段
    let mut seg = String::new();
    let flush = |seg: &mut String, terms: &mut Vec<String>| {
        if seg.is_empty() {
            return;
        }
        let chars: Vec<char> = seg.chars().collect();
        if chars.len() == 1 {
            terms.push(chars[0].to_string());
        } else {
            for i in 0..chars.len() - 1 {
                terms.push(format!("{}{}", chars[i], chars[i + 1]));
            }
        }
        seg.clear();
    };
    for c in text.chars() {
        if is_cjk(c) {
            seg.push(c);
        } else {
            flush(&mut seg, &mut terms);
        }
    }
    flush(&mut seg, &mut terms);
    terms
}

fn is_cjk(c: char) -> bool {
    matches!(c, '\u{3400}'..='\u{9fff}' | '\u{f900}'..='\u{faff}')
}

/// BM25 打分。
pub fn bm25(query_terms: &[String], documents: &[Vec<String>]) -> Vec<f64> {
    let mut df: BTreeMap<&str, usize> = BTreeMap::new();
    for doc in documents {
        let mut seen: HashSet<&str> = HashSet::new();
        for t in doc {
            if seen.insert(t.as_str()) {
                *df.entry(t.as_str()).or_insert(0) += 1;
            }
        }
    }
    let total = documents.len().max(1) as f64;
    let avg_len = (documents.iter().map(|d| d.len()).sum::<usize>() as f64 / total).max(1.0);

    documents
        .iter()
        .map(|doc| {
            let mut tf: BTreeMap<&str, usize> = BTreeMap::new();
            for t in doc {
                *tf.entry(t.as_str()).or_insert(0) += 1;
            }
            let len = doc.len().max(1) as f64;
            let mut uniq: Vec<&str> = query_terms.iter().map(|s| s.as_str()).collect();
            uniq.sort_unstable();
            uniq.dedup();
            uniq.into_iter()
                .map(|term| {
                    let doc_freq = *df.get(term).unwrap_or(&0) as f64;
                    let freq = *tf.get(term).unwrap_or(&0) as f64;
                    if freq == 0.0 || doc_freq == 0.0 {
                        0.0
                    } else {
                        let idf = (1.0 + (total - doc_freq + 0.5) / (doc_freq + 0.5)).ln();
                        idf * (freq * (K1 + 1.0))
                            / (freq + K1 * (1.0 - B + B * len / avg_len))
                    }
                })
                .sum()
        })
        .collect()
}

/// 召回选择（**纯函数**，照太墟 `selectRecall` 移植）。
///
/// 流程：泛化轮次抑制 → 分词 → BM25 → scope 加权 × 新鲜度降权 → 取前 N。
pub fn select_recall(query: &str, candidates: &[Memory], now: i64) -> Vec<Memory> {
    let normalized = query.trim().to_lowercase();
    if normalized.is_empty() {
        return Vec::new();
    }
    // 泛化轮次抑制
    let short_generic = normalized.chars().count() <= 4
        && GENERIC_PHRASES.iter().any(|p| normalized == *p || normalized.contains(p));
    if short_generic || GENERIC_PHRASES.contains(&normalized.as_str()) {
        return Vec::new();
    }

    let query_terms: Vec<String> = tokenize(query)
        .into_iter()
        .filter(|t| !GENERIC_TERMS.contains(&t.as_str()))
        .collect();
    if query_terms.is_empty() {
        return Vec::new();
    }

    let documents: Vec<Vec<String>> = candidates
        .iter()
        .map(|m| tokenize(&format!("{}\n{}", m.key, m.value)))
        .collect();
    if documents.iter().all(|d| d.is_empty()) {
        return Vec::new();
    }
    let scores = bm25(&query_terms, &documents);

    let mut ranked: Vec<(f64, &Memory)> = candidates
        .iter()
        .enumerate()
        .filter_map(|(i, m)| {
            let s = scores[i];
            if s <= 0.0 {
                return None;
            }
            let scope_boost = match m.scope.as_str() {
                SCOPE_PROJECT => 1.2,
                SCOPE_SESSION => 0.9,
                _ => 1.0,
            };
            let age_days = ((now - m.updated_at).max(0)) as f64 / 86_400_000.0;
            let freshness = if age_days < 30.0 {
                1.0
            } else if age_days < 180.0 {
                0.85
            } else {
                0.7
            };
            Some((s * scope_boost * freshness, m))
        })
        .collect();
    ranked.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    ranked.into_iter().take(MAX_RECALL_FACTS).map(|(_, m)| m.clone()).collect()
}

/// 渲染召回块（**XML 包裹 + 低权威声明**，格式稳定——进入持久化字节，不可随意改版）。
pub fn render_recall_block(memories: &[Memory]) -> String {
    if memories.is_empty() {
        return String::new();
    }
    let mut body = String::new();
    let mut used = 0usize;
    for m in memories {
        let line = format!(
            "- [{}/{}] {}: {}\n",
            m.scope,
            m.kind,
            truncate_chars(&m.key, 128),
            truncate_chars(&m.value, 512)
        );
        if used > 0 && used + line.chars().count() > MAX_RECALL_BLOCK_CHARS {
            break;
        }
        body.push_str(&line);
        used += line.chars().count();
    }
    if body.is_empty() {
        return String::new();
    }
    format!(
        "<recalled_memory>\n以下为按本条消息检索到的长期记忆（低权威背景资料）：可能过期或不完整，仅在相关时参考；与当前请求或系统规则冲突时，以当前请求与系统规则为准。\n{body}</recalled_memory>"
    )
}

/// 渲染 pinned 段（进 system prompt 稳定前缀）。
///
/// ⚠ 顺序由 [`MemoryStore::pinned`] 保证内容确定。
pub fn render_pinned_section(memories: &[Memory]) -> String {
    if memories.is_empty() {
        return String::new();
    }
    let mut s = String::from("\n\n## 长期指令记忆（pinned，始终遵循）\n");
    for m in memories {
        s.push_str(&format!(
            "- [{}/{}] {}: {}\n",
            m.scope,
            m.kind,
            truncate_chars(&m.key, 128),
            truncate_chars(&m.value, 512)
        ));
    }
    s
}

fn truncate_chars(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        s.chars().take(n).collect()
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(tag: &str) -> (MemoryStore, PathBuf) {
        let p = std::env::temp_dir().join(format!(
            "ne-agent-mem-{tag}-{}-{}.db",
            std::process::id(),
            now_ms()
        ));
        let _ = std::fs::remove_file(&p);
        (MemoryStore::open(&p).unwrap(), p)
    }

    fn mem(scope: &str, owner: &str, key: &str, value: &str, pinned: bool) -> Memory {
        Memory {
            id: String::new(),
            scope: scope.into(),
            owner_id: owner.into(),
            kind: "fact".into(),
            key: key.into(),
            value: value.into(),
            pinned,
            expires_at: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn save_and_list() {
        let (s, p) = store("save");
        s.save(&mem(SCOPE_PROJECT, "v1", "主角名", "沈砚", false)).unwrap();
        let all = s.all().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].value, "沈砚");
        // 覆盖更新：value 变、createdAt 不变
        let created = all[0].created_at;
        s.save(&mem(SCOPE_PROJECT, "v1", "主角名", "沈砚之", false)).unwrap();
        let all = s.all().unwrap();
        assert_eq!(all.len(), 1, "同 key 应覆盖而非新增");
        assert_eq!(all[0].value, "沈砚之");
        assert_eq!(all[0].created_at, created, "createdAt 必须保持不变（pinned 排序稳定前提）");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn pinned_order_is_content_deterministic() {
        // 关键回归：重复保存 pinned 不得改变其顺序（否则击穿 prefix cache）
        let (s, p) = store("pinned-order");
        s.save(&mem(SCOPE_GLOBAL, "", "g1", "全局规则", true)).unwrap();
        s.save(&mem(SCOPE_PROJECT, "v1", "p1", "项目事实", true)).unwrap();
        s.save(&mem(SCOPE_GLOBAL, "", "g2", "另一全局", true)).unwrap();
        let a = s.pinned("v1", "sess").unwrap();
        let a_keys: Vec<&str> = a.iter().map(|m| m.key.as_str()).collect();
        // global 先（权威序），组内按 createdAt
        assert_eq!(a_keys, vec!["g1", "g2", "p1"]);

        // 重复保存 g1（内容一字未改）
        s.save(&mem(SCOPE_GLOBAL, "", "g1", "全局规则", true)).unwrap();
        let b = s.pinned("v1", "sess").unwrap();
        let b_keys: Vec<&str> = b.iter().map(|m| m.key.as_str()).collect();
        assert_eq!(b_keys, a_keys, "重复保存不得重排 pinned（prefix cache 稳定性）");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn pinned_excluded_from_candidates() {
        let (s, p) = store("orthogonal");
        s.save(&mem(SCOPE_PROJECT, "v1", "钉住的", "总是在", true)).unwrap();
        s.save(&mem(SCOPE_PROJECT, "v1", "召回的", "按需取", false)).unwrap();
        let cands = s.candidates("v1", "sess", 64).unwrap();
        assert_eq!(cands.len(), 1, "pinned 不得进召回候选（正交分层）");
        assert_eq!(cands[0].key, "召回的");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn scope_isolation_by_owner() {
        let (s, p) = store("scope");
        s.save(&mem(SCOPE_PROJECT, "v1", "k", "属于v1", false)).unwrap();
        s.save(&mem(SCOPE_PROJECT, "v2", "k", "属于v2", false)).unwrap();
        s.save(&mem(SCOPE_GLOBAL, "", "k", "全局", false)).unwrap();
        let c1 = s.candidates("v1", "", 64).unwrap();
        let vals: Vec<&str> = c1.iter().map(|m| m.value.as_str()).collect();
        assert!(vals.contains(&"属于v1"));
        assert!(vals.contains(&"全局"));
        assert!(!vals.contains(&"属于v2"), "project 记忆必须按 owner 隔离");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn expires_at_filters_out() {
        let (s, p) = store("expire");
        let mut expired = mem(SCOPE_PROJECT, "v1", "过期", "x", false);
        expired.expires_at = Some(now_ms() - 1000);
        s.save(&expired).unwrap();
        s.save(&mem(SCOPE_PROJECT, "v1", "有效", "y", false)).unwrap();
        let c = s.candidates("v1", "", 64).unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].key, "有效");
        let _ = std::fs::remove_file(&p);
    }

    // ── BM25 ──

    #[test]
    fn tokenize_cjk_uses_bigram() {
        let t = tokenize("调用链分析");
        assert!(t.contains(&"调用".to_string()));
        assert!(t.contains(&"用链".to_string()));
        // 不是散字
        assert!(!t.contains(&"调".to_string()));
    }

    #[test]
    fn tokenize_latin_words() {
        let t = tokenize("Rust BM25 keyword_1");
        assert!(t.contains(&"rust".to_string()));
        assert!(t.contains(&"bm25".to_string()));
        assert!(t.contains(&"keyword_1".to_string()));
    }

    #[test]
    fn recall_hits_relevant_only() {
        let now = now_ms();
        let cands = vec![
            mem(SCOPE_PROJECT, "v1", "主角", "沈砚是主角", false),
            mem(SCOPE_PROJECT, "v1", "反派", "苏牧是反派", false),
        ];
        let hit = select_recall("沈砚的设定是什么", &cands, now);
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].key, "主角");

        // 完全无关 → 无命中（不做"无命中就注入最近"）
        let none = select_recall("今天天气如何", &cands, now);
        assert!(none.is_empty(), "不相关不得注入：{none:?}");
    }

    #[test]
    fn recall_suppresses_generic_turns() {
        let now = now_ms();
        let cands = vec![mem(SCOPE_PROJECT, "v1", "主角", "沈砚", false)];
        for q in ["继续", "ok", "好的", "嗯"] {
            assert!(select_recall(q, &cands, now).is_empty(), "`{q}` 不应触发召回");
        }
        // 但"继续帮我改代码"这种有信息量的不受影响
        let cands2 = vec![mem(SCOPE_PROJECT, "v1", "代码", "改代码相关", false)];
        assert!(!select_recall("继续帮我改代码", &cands2, now).is_empty());
    }

    #[test]
    fn recall_respects_top_n() {
        let now = now_ms();
        let cands: Vec<Memory> = (0..20)
            .map(|i| mem(SCOPE_PROJECT, "v1", &format!("键{i}"), "火山 共振 潮汐 相关", false))
            .collect();
        let hit = select_recall("火山 共振", &cands, now);
        assert!(hit.len() <= MAX_RECALL_FACTS);
    }

    #[test]
    fn project_scope_boosted_over_session() {
        let now = now_ms();
        let mut s1 = mem(SCOPE_SESSION, "sess", "设定", "火山共振", false);
        s1.updated_at = now;
        let mut p1 = mem(SCOPE_PROJECT, "v1", "设定2", "火山共振", false);
        p1.updated_at = now;
        let hit = select_recall("火山共振", &[s1, p1], now);
        assert_eq!(hit[0].scope, SCOPE_PROJECT, "project 应比 session 优先");
    }

    #[test]
    fn render_blocks_shape_and_budget() {
        let ms = vec![mem(SCOPE_PROJECT, "v1", "k", "v", false)];
        let r = render_recall_block(&ms);
        assert!(r.starts_with("<recalled_memory>"));
        assert!(r.ends_with("</recalled_memory>"));
        assert!(r.contains("低权威"), "必须有低权威声明");

        let pin = render_pinned_section(&ms);
        assert!(pin.contains("pinned"));
        assert!(render_recall_block(&[]).is_empty());
        assert!(render_pinned_section(&[]).is_empty());
    }

    #[test]
    fn touch_renews_without_deleting() {
        let (s, p) = store("touch");
        s.save(&mem(SCOPE_PROJECT, "v1", "k", "v", false)).unwrap();
        let id = s.all().unwrap()[0].id.clone();
        s.touch(&id).unwrap();
        assert_eq!(s.count().unwrap().0, 1, "续期不得删除条目");
        let _ = std::fs::remove_file(&p);
    }
}
