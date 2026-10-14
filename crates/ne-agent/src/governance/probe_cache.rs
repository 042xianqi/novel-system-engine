//! 探针缓存层（world-gov）—— `LIB/07_勘误工坊/probe_cache/`。
//!
//! # 定位
//! 避免 scan → prompt-preview → inspect-batch 反复重算**一模一样**的探针。
//!
//! - 缓存 key = `hash(layer + key + src_mtime + baseline_fingerprint)`
//! - 每项：`<key>.json`（探针本体，**只存结构化探针，不存资料库原始正文**）+ `<key>.meta`（元信息）
//! - 命中条件：条目 `src_mtime ≤ 缓存记录 mtime` 且基线指纹一致
//!
//! # 铁则
//! - ❌ 不缓存大文件原始正文；只缓存 world-gov 生成的结构化探针。
//! - ✅ 文件名 hash 化（hex），严格防路径逃逸。
//! - ✅ 不自动清理；GC/clear 需显式命令。

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// 缓存子目录（相对库根）。
pub const CACHE_SUBDIR: &str = "07_勘误工坊/probe_cache";

/// 缓存项元信息。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CacheMeta {
    pub cache_key: String,
    pub layer: String,
    pub key: String,
    pub source: String,
    /// 生成时刻（Unix 秒）
    pub generated_at: i64,
    /// 素材 mtime（Unix 秒）——判断是否失效
    pub src_mtime: i64,
    pub baseline_fingerprint: String,
}

fn fnv1a_hex(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// 缓存 key（**纯函数**）：hash(layer + key + src_mtime + baseline_fingerprint)。
/// 输出 16 hex → 天然无路径分隔，防逃逸。
pub fn cache_key(layer: &str, key: &str, src_mtime: i64, baseline_fp: &str) -> String {
    fnv1a_hex(&format!("{layer}\u{1}{key}\u{1}{src_mtime}\u{1}{baseline_fp}"))
}

/// 缓存目录。
pub fn cache_dir(lib_root: &Path) -> PathBuf {
    lib_root.join("07_勘误工坊").join("probe_cache")
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// 缓存命中检查：返回探针本体（若 meta 存在且 `src_mtime`/基线指纹匹配）。
pub fn get(
    lib_root: &Path,
    layer: &str,
    key: &str,
    src_mtime: i64,
    baseline_fp: &str,
) -> Option<Value> {
    let ck = cache_key(layer, key, src_mtime, baseline_fp);
    let meta_p = cache_dir(lib_root).join(format!("{ck}.meta"));
    let body_p = cache_dir(lib_root).join(format!("{ck}.json"));
    let meta_raw = std::fs::read_to_string(&meta_p).ok()?;
    let meta: CacheMeta = serde_json::from_str(&meta_raw).ok()?;
    // 失效校验：素材更新（src_mtime 变大）或基线指纹变化 → 不命中
    if meta.src_mtime != src_mtime || meta.baseline_fingerprint != baseline_fp {
        return None;
    }
    let body = std::fs::read_to_string(&body_p).ok()?;
    serde_json::from_str(&body).ok()
}

/// 写入缓存（探针本体 + meta）。返回 (json 路径, meta 路径)。
pub fn put(
    lib_root: &Path,
    layer: &str,
    key: &str,
    src_mtime: i64,
    baseline_fp: &str,
    source: &str,
    probe: &Value,
) -> Result<(PathBuf, PathBuf)> {
    let dir = cache_dir(lib_root);
    std::fs::create_dir_all(&dir)?;
    let ck = cache_key(layer, key, src_mtime, baseline_fp);
    // 双保险：ck 必须为纯 hex（防任何注入）
    if !ck.chars().all(|c| c.is_ascii_hexdigit()) {
        anyhow::bail!("缓存 key 非法（应为 hex）：{ck}");
    }
    let meta = CacheMeta {
        cache_key: ck.clone(),
        layer: layer.to_string(),
        key: key.to_string(),
        source: source.to_string(),
        generated_at: now_secs(),
        src_mtime,
        baseline_fingerprint: baseline_fp.to_string(),
    };
    let body_p = dir.join(format!("{ck}.json"));
    let meta_p = dir.join(format!("{ck}.meta"));
    std::fs::write(&body_p, serde_json::to_string_pretty(probe)?)?;
    std::fs::write(&meta_p, serde_json::to_string_pretty(&meta)?)?;
    Ok((body_p, meta_p))
}

/// 枚举缓存项（(key, mtime_secs, size)）。
fn entries(lib_root: &Path) -> Result<Vec<(String, i64, u64)>> {
    let dir = cache_dir(lib_root);
    let mut out = Vec::new();
    if !dir.is_dir() {
        return Ok(out);
    }
    for e in std::fs::read_dir(&dir)? {
        let e = e?;
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
        if !name.ends_with(".json") {
            continue;
        }
        let md = std::fs::metadata(&p)?;
        let mtime = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        out.push((name, mtime, md.len()));
    }
    Ok(out)
}

/// 缓存状态：数量、总大小、旧于 N 天的条目数。
pub fn status(lib_root: &Path, older_than_days: u64) -> Result<Value> {
    let es = entries(lib_root)?;
    let cutoff = now_secs() - (older_than_days as i64) * 86400;
    let total_bytes: u64 = es.iter().map(|(_, _, s)| *s).sum();
    let stale = es.iter().filter(|(_, m, _)| *m < cutoff).count();
    // meta 文件也计入总量（粗略）
    let meta_bytes: u64 = {
        let dir = cache_dir(lib_root);
        let mut n = 0u64;
        if dir.is_dir() {
            for e in std::fs::read_dir(&dir)? {
                let p = e?.path();
                if p.extension().map(|x| x == "meta").unwrap_or(false) {
                    n += std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                }
            }
        }
        n
    };
    Ok(json!({
        "command": "world-gov cache status",
        "dir": cache_dir(lib_root).display().to_string(),
        "entries": es.len(),
        "total_bytes": total_bytes + meta_bytes,
        "older_than_days": older_than_days,
        "stale_entries": stale,
        "note": "缓存只存结构化探针，不存资料库原始正文。",
    }))
}

/// GC：清理旧于 `older_than_days` 天的缓存项（json + meta）。
pub fn gc(lib_root: &Path, older_than_days: u64) -> Result<usize> {
    let es = entries(lib_root)?;
    let cutoff = now_secs() - (older_than_days as i64) * 86400;
    let dir = cache_dir(lib_root);
    let mut removed = 0usize;
    for (name, m, _) in es {
        if m < cutoff {
            let stem = name.trim_end_matches(".json");
            let _ = std::fs::remove_file(dir.join(format!("{stem}.json")));
            let _ = std::fs::remove_file(dir.join(format!("{stem}.meta")));
            removed += 1;
        }
    }
    Ok(removed)
}

/// 清空全部缓存。
pub fn clear(lib_root: &Path) -> Result<usize> {
    let es = entries(lib_root)?;
    let dir = cache_dir(lib_root);
    let mut removed = 0usize;
    for (name, _, _) in es {
        let stem = name.trim_end_matches(".json");
        if std::fs::remove_file(dir.join(format!("{stem}.json"))).is_ok() {
            removed += 1;
        }
        let _ = std::fs::remove_file(dir.join(format!("{stem}.meta")));
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_lib(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wg_cache_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn cache_key_is_hex_and_param_sensitive() {
        let a = cache_key("L4", "A-1", 100, "fp1");
        let b = cache_key("L4", "A-1", 100, "fp1");
        assert_eq!(a, b, "同参数同 key");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()), "必须纯 hex");
        assert_eq!(a.len(), 16);
        assert_ne!(a, cache_key("L4", "A-1", 101, "fp1"), "mtime 变 → key 变");
        assert_ne!(a, cache_key("L4", "A-1", 100, "fp2"), "基线变 → key 变");
        assert_ne!(a, cache_key("L3", "A-1", 100, "fp1"), "layer 变 → key 变");
        assert_ne!(a, cache_key("L4", "A-2", 100, "fp1"), "key 变 → key 变");
    }

    #[test]
    fn put_then_hit_then_miss_on_change() {
        let lib = tmp_lib("hit");
        let probe = json!({"k":"v"});
        put(&lib, "L4", "A-1", 100, "fp1", "04/A-1.md", &probe).unwrap();
        // 命中
        assert_eq!(get(&lib, "L4", "A-1", 100, "fp1").unwrap(), probe);
        // 素材更新（mtime 变大）→ 失效
        assert!(get(&lib, "L4", "A-1", 200, "fp1").is_none(), "mtime 变 → miss");
        // 基线变 → 失效
        assert!(get(&lib, "L4", "A-1", 100, "fp2").is_none(), "基线变 → miss");
        // 从未缓存 → None
        assert!(get(&lib, "L4", "NOPE", 100, "fp1").is_none());
        let _ = std::fs::remove_dir_all(&lib);
    }

    #[test]
    fn files_are_confined_to_cache_dir() {
        let lib = tmp_lib("confine");
        let (jp, mp) = put(&lib, "L4", "../evil", 1, "fp", "s", &json!({})).unwrap();
        assert!(jp.starts_with(cache_dir(&lib)), "json 必须落在 cache 目录内");
        assert!(mp.starts_with(cache_dir(&lib)), "meta 必须落在 cache 目录内");
        // 文件名不含路径分隔
        assert!(!jp.file_name().unwrap().to_string_lossy().contains('/'));
        let _ = std::fs::remove_dir_all(&lib);
    }

    #[test]
    fn status_gc_clear() {
        let lib = tmp_lib("gc");
        for i in 0..3 {
            put(&lib, "L4", &format!("K{i}"), 100, "fp", "s", &json!({"i":i})).unwrap();
        }
        let st = status(&lib, 30).unwrap();
        assert_eq!(st["entries"], 3);
        assert!(st["total_bytes"].as_u64().unwrap() > 0);
        // gc 用 0 天 → 刚生成的（mtime≈now）不算 stale（< cutoff 才清）；这里断言不 panic 且清 0
        let n0 = gc(&lib, 0).unwrap();
        assert_eq!(n0, 0, "刚生成的不会被 0 天 GC 清掉");
        let cleared = clear(&lib).unwrap();
        assert_eq!(cleared, 3);
        assert_eq!(status(&lib, 30).unwrap()["entries"], 0);
        let _ = std::fs::remove_dir_all(&lib);
    }

    #[test]
    fn missing_dir_is_empty_not_error() {
        let lib = tmp_lib("empty");
        assert_eq!(status(&lib, 30).unwrap()["entries"], 0);
        assert_eq!(gc(&lib, 30).unwrap(), 0);
        assert_eq!(clear(&lib).unwrap(), 0);
    }
}