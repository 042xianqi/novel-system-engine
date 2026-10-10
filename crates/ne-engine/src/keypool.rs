//! Key 池调度 —— **替代已删除的 omp-rs-7k**。
//!
//! 为什么不直接删旧实现：7k 提供 4 个能力被 5 个 crate 消费（lane 轮转 / 探针排名 /
//! 权重分派 / 双约束限速）。这里用**零 tokio、零 reqwest** 的方式重实现，
//! 语义与 7k 一致（lane_key 轮转、fastest-first rank、重任务给慢 key）。
//!
//! 与 7k 的差异（如实声明）：
//!   * 探针不再用 reqwest async，改为走 `crate::http` 阻塞式（调用方可自行包线程池）。
//!   * 不再暴露 `#[cfg(feature="http")]` 双版本——统一一套实现。

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

/// 并发 lane 上限（沿用 7k 的 7）。
pub const MAX_PARALLEL_KEYS: usize = 7;
/// 单 key 每分钟请求上限（Agnes 免费账号实测）。
pub const DEFAULT_RATE_LIMIT_RPM: u32 = 10;
/// 单 key 同时在途请求上限（实测并发顶 ≈ 5）。
pub const DEFAULT_MAX_INFLIGHT: u32 = 5;
/// 探针 max_tokens / 超时。
const PROBE_MAX_TOKENS: u32 = 16;
const PROBE_TIMEOUT_SECS: u64 = 8;

/// 双约束限速器（按 key 独立计，与 7k 语义一致）。
#[derive(Debug, Clone)]
pub struct RateLimiter {
    rpm: u32,
    max_inflight: u32,
    window: std::sync::Arc<std::sync::Mutex<VecDeque<Instant>>>,
    state: std::sync::Arc<std::sync::Mutex<u32>>,
    cv: std::sync::Arc<std::sync::Condvar>,
}

impl RateLimiter {
    pub fn new(rpm: u32) -> Self {
        Self::with(rpm, DEFAULT_MAX_INFLIGHT)
    }
    pub fn with(rpm: u32, max_inflight: u32) -> Self {
        RateLimiter {
            rpm,
            max_inflight,
            window: Default::default(),
            state: Default::default(),
            cv: Default::default(),
        }
    }
    pub fn default_rpm() -> Self {
        Self::with(DEFAULT_RATE_LIMIT_RPM, DEFAULT_MAX_INFLIGHT)
    }
    /// 阻塞式获取 1 个请求名额（先占并发槽，再等 RPM 窗口）。
    pub fn acquire(&self) {
        let mut slot = self.state.lock().unwrap();
        while *slot >= self.max_inflight {
            slot = self.cv.wait(slot).unwrap();
        }
        *slot += 1;
        drop(slot);

        let now = Instant::now();
        let mut q = self.window.lock().unwrap();
        while q.len() >= self.rpm as usize {
            if let Some(front) = q.front() {
                if now.duration_since(*front) >= Duration::from_secs(60) {
                    q.pop_front();
                } else {
                    let wait_ms = 60000u64.saturating_sub(front.elapsed().as_millis() as u64);
                    drop(q);
                    std::thread::sleep(Duration::from_millis(wait_ms.saturating_sub(50)));
                    q = self.window.lock().unwrap();
                    break;
                }
            } else {
                break;
            }
        }
        q.push_back(Instant::now());
    }
    pub fn release(&self) {
        let mut slot = self.state.lock().unwrap();
        if *slot > 0 {
            *slot -= 1;
        }
        self.cv.notify_one();
    }
    pub fn in_flight(&self) -> u32 {
        *self.state.lock().unwrap()
    }
    pub fn remaining(&self) -> u32 {
        let q = self.window.lock().unwrap();
        let now = Instant::now();
        let active = q
            .iter()
            .filter(|t| now.duration_since(**t) < Duration::from_secs(60))
            .count();
        self.rpm.saturating_sub(active as u32)
    }
}
impl Default for RateLimiter {
    fn default() -> Self {
        Self::default_rpm()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyProbeResult {
    pub key: String,
    pub ok: bool,
    pub latency_ms: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankedPool {
    pub provider: String,
    pub keys: Vec<String>,
    pub ranked_at: u128,
}

/// 稳定排名（ok 在前；同 ok 按 latency 升序）。
fn rank_inplace(results: &mut [KeyProbeResult]) {
    results.sort_by(|a, b| match (a.ok, b.ok) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.latency_ms.cmp(&b.latency_ms),
    });
}

/// 单 key 探针（阻塞式，用 crate::http）。失败/超时记 ok=false。
pub fn probe_key(endpoint: &str, model: &str, key: &str) -> KeyProbeResult {
    let url = format!("{}/chat/completions", endpoint.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": "hi"}],
        "max_tokens": PROBE_MAX_TOKENS,
    });
    let headers = vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Authorization".to_string(), format!("Bearer {key}")),
    ];
    let t = Instant::now();
    let ok = crate::http::post_json(
        &url,
        &headers,
        &serde_json::to_vec(&body).unwrap_or_default(),
        Duration::from_secs(PROBE_TIMEOUT_SECS),
    )
    .map(|r| (200..300).contains(&r.status))
    .unwrap_or(false);
    KeyProbeResult { key: key.to_string(), ok, latency_ms: t.elapsed().as_millis() }
}

/// 并发探全部 key + rank。并发用标准库线程（无 tokio）。
pub fn probe_all_keys(endpoint: &str, model: &str, keys: &[String]) -> Vec<KeyProbeResult> {
    let mut handles = Vec::new();
    for k in keys {
        let (e, m, k) = (endpoint.to_string(), model.to_string(), k.clone());
        handles.push(std::thread::spawn(move || probe_key(&e, &m, &k)));
    }
    let mut results: Vec<KeyProbeResult> =
        handles.into_iter().filter_map(|h| h.join().ok()).collect();
    rank_inplace(&mut results);
    results
}

/// 权重分派（语义照搬 7k：任务重→给慢 key，超 key 数 round-robin）。
pub fn assign_keys_by_weight(ranked_keys: &[String], task_weights: &[f64]) -> BTreeMap<usize, usize> {
    let mut assignment = BTreeMap::new();
    if ranked_keys.is_empty() || task_weights.is_empty() {
        return assignment;
    }
    let mut order: Vec<(f64, usize)> =
        task_weights.iter().enumerate().map(|(i, w)| (*w, i)).collect();
    order.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let key_order: Vec<usize> = (0..ranked_keys.len()).rev().collect();
    let key_len = key_order.len();
    for (rank, entry) in order.iter().enumerate() {
        assignment.insert(entry.1, key_order[rank % key_len]);
    }
    assignment
}

/// 最快 key 钉成 brain（lane0）。
pub fn bind_fastest_key_as_brain(ranked_pool: &RankedPool) -> Option<&str> {
    ranked_pool.keys.first().map(|s| s.as_str())
}

/// lane 轮转：lane = agent_index-1；pool[lane]，超上限 round-robin。
pub fn lane_key(pool: &[String], agent_index_1based: usize) -> Option<String> {
    if pool.is_empty() {
        return None;
    }
    let lane = agent_index_1based.saturating_sub(1);
    let max_lanes = pool.len().min(MAX_PARALLEL_KEYS);
    if lane >= max_lanes {
        Some(pool[lane % pool.len()].clone())
    } else {
        Some(pool[lane].clone())
    }
}

/// 组装 ranked pool。
pub fn build_ranked_pool(provider: &str, probed: &[KeyProbeResult]) -> Option<RankedPool> {
    let mut sorted: Vec<KeyProbeResult> = probed.to_vec();
    rank_inplace(&mut sorted);
    let ordered: Vec<String> = sorted.iter().map(|p| p.key.clone()).collect();
    if ordered.is_empty() {
        return None;
    }
    Some(RankedPool {
        provider: provider.to_string(),
        keys: ordered,
        ranked_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lane0_is_first_key() {
        let pool = vec!["A".to_string(), "B".to_string(), "C".to_string()];
        assert_eq!(lane_key(&pool, 1), Some("A".to_string()));
        assert_eq!(lane_key(&pool, 3), Some("C".to_string()));
        assert_eq!(lane_key(&[], 1), None);
    }

    #[test]
    fn lane_beyond_pool_round_robins() {
        let pool = vec!["A".to_string(), "B".to_string()];
        assert_eq!(lane_key(&pool, 3), Some("A".to_string()));
        assert_eq!(lane_key(&pool, 4), Some("B".to_string()));
    }

    #[test]
    fn weight_assign_heavy_gets_slow_key() {
        let keys = vec!["fast".into(), "mid".into(), "slow".into()];
        let a = assign_keys_by_weight(&keys, &[3.0, 2.0, 1.0]);
        assert_eq!(a.get(&0), Some(&2));
        assert_eq!(a.get(&1), Some(&1));
        assert_eq!(a.get(&2), Some(&0));
    }

    #[test]
    fn rank_failed_last() {
        let probed = vec![
            KeyProbeResult { key: "x".into(), ok: false, latency_ms: 10 },
            KeyProbeResult { key: "y".into(), ok: true, latency_ms: 50 },
            KeyProbeResult { key: "z".into(), ok: true, latency_ms: 20 },
        ];
        let pool = build_ranked_pool("agens", &probed).unwrap();
        assert_eq!(pool.keys, vec!["z", "y", "x"]);
        assert_eq!(bind_fastest_key_as_brain(&pool), Some("z"));
    }

    #[test]
    fn rate_limiter_inflight_cap() {
        let rl = RateLimiter::with(60, 2);
        rl.acquire();
        rl.acquire();
        assert_eq!(rl.in_flight(), 2);
        rl.release();
        assert_eq!(rl.in_flight(), 1);
    }
}
