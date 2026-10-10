//! 对比：① 单 key 并发 4 请求  ② 4 把 key 各 1 请求
//! 目的：判断该服务是「按 key 限流」还是「按并发限流」——决定架构。
use ne_engine::{http, providers};
use std::time::{Duration, Instant};

fn one(base: &str, key: &str, tag: usize) -> (usize, u16, u128, String) {
    let body = serde_json::json!({
        "model": "agnes-3.0-flash",
        "messages": [{"role":"user","content": format!("请写一句关于第{tag}天的话，不超过10字")}],
        "max_tokens": 64, "temperature": 1.0, "enable_thinking": false,
    });
    let headers = vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Authorization".to_string(), format!("Bearer {key}")),
    ];
    let url = format!("{}/chat/completions", base.trim_end_matches('/'));
    let t = Instant::now();
    let r = http::post_json(&url, &headers, &serde_json::to_vec(&body).unwrap(), Duration::from_secs(90));
    let ms = t.elapsed().as_millis();
    match r {
        Ok(resp) => {
            let snippet: String = resp.body.chars().take(0).collect();
            (tag, resp.status, ms, snippet)
        }
        Err(e) => (tag, 0, ms, e.to_string()),
    }
}

fn run(label: &str, base: &str, keys: &[String], n: usize) {
    let t0 = Instant::now();
    let mut hs = vec![];
    for i in 0..n {
        let b = base.to_string();
        let k = keys[i % keys.len()].clone();
        hs.push(std::thread::spawn(move || one(&b, &k, i)));
    }
    let mut rs: Vec<_> = hs.into_iter().map(|h| h.join().unwrap()).collect();
    rs.sort_by_key(|r| r.0);
    let ok = rs.iter().filter(|r| r.1 == 200).count();
    let slowest = rs.iter().map(|r| r.2).max().unwrap_or(0);
    println!("{label}: {ok}/{n} 成功  墙钟={}ms  最慢单请求={slowest}ms", t0.elapsed().as_millis());
    for (i, st, ms, note) in &rs {
        if *st != 200 {
            println!("    ✗ #{i} status={st} {ms}ms {note}");
        }
    }
}

fn main() {
    let (provs, _r) = providers::load_from_dir(&providers::agent_dir()).unwrap();
    let p = provs.map.get("agens").unwrap();
    let base = p.base_url.clone();
    let mut keys: Vec<String> = vec![];
    for k in &p.keys { if !keys.contains(k) { keys.push(k.clone()); } }
    let k0 = vec![keys[0].clone()];
    let k4: Vec<String> = keys.iter().take(4).cloned().collect();

    println!("唯一 key = {}, 取 4 把做对比\n", keys.len());
    run("① 单 key × 4 并发 ", &base, &k0, 4);
    run("② 4 key × 1 各并发", &base, &k4, 4);
    run("③ 4 key × 7 全并发", &base, &keys, 7);
}
