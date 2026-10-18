//! 增量扫描快照（world-gov）—— `07_勘误工坊/.scan_snapshot.json`。
//!
//! # 定位
//! - **纯小 JSON，不是数据库**；只存**时间锚点 + 统计 + 指纹**，绝不存任何业务内容/原文。
//! - 严格限定在 `07_勘误工坊/` 内，绝不外溢。
//! - 供 `scan-library --since last` 读取"上次成功扫描时刻"。

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 快照相对路径（**固定**，不接受外部路径，防外溢）。
pub const SNAPSHOT_REL: &str = "07_勘误工坊/.scan_snapshot.json";

/// 扫描快照内容（**只做时间锚点**，不含业务内容）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScanSnapshot {
    /// 上次成功扫描时刻（Unix 秒）。
    pub last_scan_at: i64,
    /// 上次扫描域条目数。
    pub last_used_count: usize,
    /// 扫描域轻量指纹（对 key 集合取 FNV；不含原文）。
    pub fingerprint: String,
}

/// 快照绝对路径（恒在 `07_勘误工坊/` 内）。
pub fn snapshot_path(lib_root: &Path) -> PathBuf {
    lib_root.join("07_勘误工坊").join(".scan_snapshot.json")
}

/// 读取快照；不存在/损坏返回 `None`（**不报错、不自动创建**）。
pub fn read_snapshot(lib_root: &Path) -> Option<ScanSnapshot> {
    let raw = std::fs::read_to_string(snapshot_path(lib_root)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// 写入快照（只在 `07_勘误工坊/` 内建目录）。
pub fn write_snapshot(lib_root: &Path, snap: &ScanSnapshot) -> Result<PathBuf> {
    let p = snapshot_path(lib_root);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&p, serde_json::to_string_pretty(snap)?)?;
    Ok(p)
}

/// 解析 `--since` 取值：
/// - `last`（大小写不敏感）→ 读快照 `last_scan_at`；**无快照时降级为 `None`（全扫）**；
/// - 纯数字 → 当作 Unix 时间戳；
/// - 其余 → 报错。
///
/// 返回 `(since_ts, 说明)`。
pub fn parse_since(val: &str, lib_root: &Path) -> Result<(Option<i64>, String)> {
    let v = val.trim();
    if v.eq_ignore_ascii_case("last") {
        match read_snapshot(lib_root) {
            Some(s) => Ok((
                Some(s.last_scan_at),
                format!("last → {}（快照 last_scan_at）", s.last_scan_at),
            )),
            None => Ok((None, "last → 无快照，降级为全量扫描（warn）".to_string())),
        }
    } else {
        match v.parse::<i64>() {
            Ok(ts) => Ok((Some(ts), format!("timestamp {ts}"))),
            Err(_) => anyhow::bail!("--since 取值非法：'{val}'（应为 Unix 时间戳数字或 last）"),
        }
    }
}

/// 扫描域指纹：对条目 key 集合取 FNV-1a（**不含原文**）。
pub fn keys_fingerprint(keys: &[String]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for k in keys {
        for b in k.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h ^= b'/' as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_roundtrip_and_missing_is_none() {
        let dir = std::env::temp_dir().join(format!("wg_snap_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(read_snapshot(&dir).is_none(), "无快照 → None，不 panic");
        let s = ScanSnapshot { last_scan_at: 12345, last_used_count: 7, fingerprint: "ab".into() };
        let p = write_snapshot(&dir, &s).unwrap();
        assert!(p.to_string_lossy().contains("07_勘误工坊"));
        assert_eq!(read_snapshot(&dir).unwrap(), s, "读写往返一致");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_since_direct_timestamp() {
        let dir = std::env::temp_dir().join("wg_snap_parse_ts");
        let (v, msg) = parse_since("1700000000", &dir).unwrap();
        assert_eq!(v, Some(1700000000));
        assert!(msg.contains("1700000000"));
    }

    #[test]
    fn parse_since_last_without_snapshot_degrades_to_full() {
        let dir = std::env::temp_dir().join("wg_snap_parse_none_zzz");
        let (v, msg) = parse_since("last", &dir).unwrap();
        assert_eq!(v, None, "无快照 → 降级全扫");
        assert!(msg.contains("降级"));
    }

    #[test]
    fn parse_since_last_with_snapshot_reads_anchor() {
        let dir = std::env::temp_dir().join(format!("wg_snap_last_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write_snapshot(&dir, &ScanSnapshot { last_scan_at: 999, last_used_count: 1, fingerprint: "x".into() }).unwrap();
        let (v, _) = parse_since("LAST", &dir).unwrap();
        assert_eq!(v, Some(999), "大小写不敏感；读快照锚点");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_since_rejects_garbage() {
        let dir = std::env::temp_dir().join("wg_snap_parse_bad");
        assert!(parse_since("yesterday", &dir).is_err());
        assert!(parse_since("12.5", &dir).is_err());
    }

    #[test]
    fn fingerprint_stable_and_sensitive() {
        let a = vec!["A".to_string(), "B".to_string()];
        let b = vec!["A".to_string(), "B".to_string()];
        let c = vec!["A".to_string(), "C".to_string()];
        assert_eq!(keys_fingerprint(&a), keys_fingerprint(&b));
        assert_ne!(keys_fingerprint(&a), keys_fingerprint(&c));
        assert_eq!(keys_fingerprint(&a).len(), 16);
    }
}