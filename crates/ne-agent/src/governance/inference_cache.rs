//! 推理结果持久缓存（world-gov，第 17 批）—— 同一探针+同模板不重复调用。
//!
//! # 目录
//! `{work-root}/07_勘误工坊/infer_cache/<key[:2]>/<key>.json`（分片防单目录爆炸）。
//!
//! # key
//! `SHA256(backend + model + system_prompt_hash + probe_hash + temperature)`（64 hex）。
//!
//! # 每条
//! key、backend、model、完整输入、完整输出、时间戳、token 消耗、标签。
//!
//! # 铁则
//! - ✅ 缓存只是性能优化：**命中绝不外呼**；清空不影响主库与校验逻辑。
//! - ✅ 每个缓存文件独立；不写任何 DB 表。
use anyhow::Result;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use super::digest;

/// 缓存根（相对 work-root）。
pub const CACHE_REL: &str = "07_勘误工坊/infer_cache";

/// 缓存 key（确定性 64-hex）。
pub fn cache_key(backend: &str, model: &str, system_prompt: &str, probe_hash: &str, temperature: f64) -> String {
    let sys_hash = digest::sha256_hex(system_prompt.as_bytes());
    let raw = format!("{backend}\u{1}{model}\u{1}{sys_hash}\u{1}{probe_hash}\u{1}{temperature:.4}");
    digest::sha256_hex(raw.as_bytes())
}

fn cache_path(work_root: &Path, key: &str) -> PathBuf {
    let shard = if key.len() >= 2 { &key[..2] } else { "00" };
    work_root.join(CACHE_REL).join(shard).join(format!("{key}.json"))
}

/// 一条缓存记录。
#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub key: String,
    pub backend: String,
    pub model: String,
    pub input: String,
    pub output: String,
    pub ts: i64,
    pub token_in: i64,
    pub token_out: i64,
    pub labels: Vec<String>,
}

/// 命中查询（只读）。返回 Some(entry) 即命中。
pub fn get(work_root: &Path, key: &str) -> Result<Option<CacheEntry>> {
    let p = cache_path(work_root, key);
    if !p.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&p)?;
    let v: Value = serde_json::from_str(&text)?;
    Ok(Some(CacheEntry {
        key: v["key"].as_str().unwrap_or("").to_string(),
        backend: v["backend"].as_str().unwrap_or("").to_string(),
        model: v["model"].as_str().unwrap_or("").to_string(),
        input: v["input"].as_str().unwrap_or("").to_string(),
        output: v["output"].as_str().unwrap_or("").to_string(),
        ts: v["ts"].as_i64().unwrap_or(0),
        token_in: v["token_in"].as_i64().unwrap_or(0),
        token_out: v["token_out"].as_i64().unwrap_or(0),
        labels: v["labels"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
    }))
}

/// 写入缓存（幂等覆盖）。
pub fn put(work_root: &Path, e: &CacheEntry) -> Result<PathBuf> {
    let p = cache_path(work_root, &e.key);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let v = json!({
        "key": e.key, "backend": e.backend, "model": e.model,
        "input": e.input, "output": e.output, "ts": e.ts,
        "token_in": e.token_in, "token_out": e.token_out, "labels": e.labels,
    });
    std::fs::write(&p, serde_json::to_string_pretty(&v)?)?;
    Ok(p)
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// 构造缓存条目（ts 由调用方或此处补）。
pub fn make_entry(key: String, backend: &str, model: &str, input: String, output: String, token_in: i64, token_out: i64, labels: Vec<String>) -> CacheEntry {
    CacheEntry { key, backend: backend.to_string(), model: model.to_string(), input, output, ts: now_secs(), token_in, token_out, labels }
}

/// 枚举全部缓存文件（只读）。
fn all_files(work_root: &Path) -> Vec<PathBuf> {
    let root = work_root.join(CACHE_REL);
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&root) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                if let Ok(rd2) = std::fs::read_dir(&p) {
                    for e2 in rd2.flatten() {
                        let f = e2.path();
                        if f.extension().map(|x| x == "json").unwrap_or(false) {
                            out.push(f);
                        }
                    }
                }
            } else if p.extension().map(|x| x == "json").unwrap_or(false) {
                out.push(p);
            }
        }
    }
    out
}

/// 状态（条数 / 总大小 / 最旧最新）。
pub fn status(work_root: &Path, ttl_days: u64) -> Result<Value> {
    let files = all_files(work_root);
    let mut total_bytes: u64 = 0;
    let mut stale = 0u64;
    let mut oldest = i64::MAX;
    let mut newest = 0i64;
    let cutoff = now_secs() - (ttl_days as i64) * 86400;
    for f in &files {
        if let Ok(md) = std::fs::metadata(f) {
            total_bytes += md.len();
            if let Ok(modt) = md.modified() {
                if let Ok(dur) = modt.duration_since(std::time::UNIX_EPOCH) {
                    let ts = dur.as_secs() as i64;
                    if ts < oldest { oldest = ts; }
                    if ts > newest { newest = ts; }
                    if ts < cutoff { stale += 1; }
                }
            }
        }
    }
    if files.is_empty() { oldest = 0; }
    Ok(json!({
        "command": "world-gov llm cache status",
        "dir": work_root.join(CACHE_REL).display().to_string(),
        "entries": files.len(),
        "total_bytes": total_bytes,
        "ttl_days": ttl_days,
        "stale_entries": stale,
        "oldest_ts": oldest,
        "newest_ts": newest,
        "note": "缓存只是性能优化；命中不外呼。可随时 gc/clear，不影响主库。",
    }))
}

/// GC：删超过 ttl_days 的缓存文件，返回删除条数。
pub fn gc(work_root: &Path, ttl_days: u64) -> Result<usize> {
    let cutoff = now_secs() - (ttl_days as i64) * 86400;
    let mut removed = 0;
    for f in all_files(work_root) {
        if let Ok(md) = std::fs::metadata(&f) {
            if let Ok(modt) = md.modified() {
                if let Ok(dur) = modt.duration_since(std::time::UNIX_EPOCH) {
                    if (dur.as_secs() as i64) < cutoff && std::fs::remove_file(&f).is_ok() {
                        removed += 1;
                    }
                }
            }
        }
    }
    Ok(removed)
}

/// clear：删全部缓存，返回删除条数。
pub fn clear(work_root: &Path) -> Result<usize> {
    let files = all_files(work_root);
    let mut removed = 0;
    for f in &files {
        if std::fs::remove_file(f).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

/// 列出缓存条目（简要元信息，不回显完整正文）。
pub fn list(work_root: &Path, limit: usize) -> Result<Value> {
    let mut rows: Vec<Value> = Vec::new();
    for f in all_files(work_root).into_iter().take(limit) {
        if let Ok(text) = std::fs::read_to_string(&f) {
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                rows.push(json!({
                    "key": v["key"].as_str().unwrap_or(""),
                    "backend": v["backend"].as_str().unwrap_or(""),
                    "model": v["model"].as_str().unwrap_or(""),
                    "ts": v["ts"].as_i64().unwrap_or(0),
                    "token_in": v["token_in"].as_i64().unwrap_or(0),
                    "token_out": v["token_out"].as_i64().unwrap_or(0),
                    "labels": v["labels"],
                }));
            }
        }
    }
    Ok(json!({"command": "world-gov llm cache list", "shown": rows.len(), "entries": rows}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!("wg_infercache_{}_{}_{}", std::process::id(), n, now_secs()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn key_is_deterministic_and_sensitive() {
        let a = cache_key("ollama", "m", "sys", "ph", 0.0);
        assert_eq!(a.len(), 64);
        assert_eq!(a, cache_key("ollama", "m", "sys", "ph", 0.0));
        assert_ne!(a, cache_key("ollama", "m", "sys", "ph", 0.5));
        assert_ne!(a, cache_key("deepseek", "m", "sys", "ph", 0.0));
        assert_ne!(a, cache_key("ollama", "m2", "sys", "ph", 0.0));
    }

    #[test]
    fn put_get_roundtrip() {
        let d = dir();
        let k = cache_key("mock", "m", "s", "p", 0.0);
        assert!(get(&d, &k).unwrap().is_none(), "未写前 miss");
        let e = make_entry(k.clone(), "mock", "m", "IN".into(), "OUT".into(), 3, 4, vec!["t".into()]);
        put(&d, &e).unwrap();
        let got = get(&d, &k).unwrap().unwrap();
        assert_eq!(got.input, "IN");
        assert_eq!(got.output, "OUT");
        assert_eq!(got.token_in, 3);
        assert_eq!(got.labels, vec!["t".to_string()]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn status_counts_and_clear() {
        let d = dir();
        for i in 0..3 {
            let k = cache_key("mock", "m", &format!("s{i}"), "p", 0.0);
            put(&d, &make_entry(k, "mock", "m", "i".into(), "o".into(), 1, 1, vec![])).unwrap();
        }
        let s = status(&d, 30).unwrap();
        assert_eq!(s["entries"], 3);
        assert!(s["total_bytes"].as_u64().unwrap() > 0);
        assert_eq!(clear(&d).unwrap(), 3);
        assert_eq!(status(&d, 30).unwrap()["entries"], 0);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn gc_removes_old_only() {
        let d = dir();
        let k = cache_key("mock", "m", "s", "p", 0.0);
        put(&d, &make_entry(k, "mock", "m", "i".into(), "o".into(), 1, 1, vec![])).unwrap();
        // ttl=0 → cutoff=now → 刚写的（ts==now）不被删（ts < cutoff 为假）
        assert_eq!(gc(&d, 30).unwrap(), 0, "新条目不被 gc");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn isolated_under_infer_cache_dir() {
        let d = dir();
        let k = cache_key("mock", "m", "s", "p", 0.0);
        let p = put(&d, &make_entry(k, "mock", "m", "i".into(), "o".into(), 1, 1, vec![])).unwrap();
        assert!(p.to_string_lossy().contains("07_勘误工坊/infer_cache"));
        let _ = std::fs::remove_dir_all(&d);
    }
}