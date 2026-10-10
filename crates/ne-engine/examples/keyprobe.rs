//! 探测 7 把 key 各自可用性（并发），并测同一把 key 的并发承载。
use ne_engine::{http, providers};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn main() {
    let dir = providers::agent_dir();
    let (provs, _r) = providers::load_from_dir(&dir).unwrap();
    let p = provs.map.get("agens").unwrap();
    let base = p.base_url.clone();

    // 去重
    let mut keys: Vec<String> = Vec::new();
    for k in &p.keys {
        if !keys.contains(k) { keys.push(k.clone()); }
    }
    println!("唯一 key 数 = {}", keys.len());
    let keys = Arc::new(keys);

    // 并发探测每把 key
    let t0 = Instant::now();
    let mut handles = vec![];
    for (i, k) in keys.iter().enumerate() {
        let k = k.clone();
        let base = base.clone();
        handles.push(std::thread::spawn(move || {
            let body = serde_json::json!({
                "model": "agnes-3.0-flash",
                "messages": [{"role":"user","content":"hi"}],
                "max_tokens": 8, "temperature": 1.0, "enable_thinking": false,
            });
            let headers = vec![
                ("Content-Type".to_string(), "application/json".to_string()),
                ("Authorization".to_string(), format!("Bearer {k}")),
            ];
            let url = format!("{}/chat/completions", base.trim_end_matches('/'));
            let t = Instant::now();
            let r = http::post_json(&url, &headers, &serde_json::to_vec(&body).unwrap(), Duration::from_secs(60));
            let ms = t.elapsed().as_millis();
            let tail: String = k.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
            match r {
                Ok(resp) => (i, tail, resp.status, format!("{ms}ms")),
                Err(e) => (i, tail, 0, format!("{} ({e})", format!("{ms}ms"))),
            }
        }));
    }
    let mut results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    results.sort_by_key(|r| r.0);
    for (i, tail, st, note) in &results {
        let mark = if *st == 200 { "✓" } else { "✗" };
        println!("  {mark} key#{i} …{tail}  status={st}  {note}");
    }
    let ok = results.iter().filter(|r| r.2 == 200).count();
    println!("\n并发探测 {} 把 key 总耗时 {} ms（串行需累加）", results.len(), t0.elapsed().as_millis());
    println!("可用 {ok}/{} 把", results.len());
}
