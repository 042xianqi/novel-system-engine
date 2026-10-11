//! 联网探测：直接调 web 模块，验证多引擎搜索与网页抓取。
//! 用法：cargo run --example webprobe -- [查询] [引擎]

fn main() {
    let query = std::env::args().nth(1).unwrap_or_else(|| "残响之河".into());
    let only = std::env::args().nth(2);

    let engines: Vec<&str> = match only.as_deref() {
        Some(e) => vec![e],
        None => vec!["bing", "baidu", "duckduckgo"],
    };

    for e in engines {
        let t = std::time::Instant::now();
        // 立即打印（行缓冲/无缓冲），避免被 timeout 杀掉时丢输出
        eprintln!("-- {e} --");
        match ne_agent::web::search(&query, e, 3) {
            Ok(h) => {
                eprintln!("  OK {} 条 ({:?})", h.len(), t.elapsed());
                for x in h {
                    eprintln!("  - {} | {}", x.title, x.url);
                }
            }
            Err(er) => eprintln!("  ERR {er} ({:?})", t.elapsed()),
        }
    }

    // 顺带验证抓取
    eprintln!("-- fetch example.com --");
    match ne_agent::web::fetch_text("https://example.com/", 500) {
        Ok(t) => eprintln!("  OK {} 字", t.chars().count()),
        Err(e) => eprintln!("  ERR {e}"),
    }
}
