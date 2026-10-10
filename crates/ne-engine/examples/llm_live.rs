use ne_engine::{llm, providers};
use std::time::Duration;

fn main() {
    let dir = providers::agent_dir();
    let (provs, roles) = providers::load_from_dir(&dir).expect("load providers");
    let mut p = providers::build_params(&provs, &roles, Some(&["agens"]));
    p.max_tokens = 512;
    p.timeout = Duration::from_secs(60);
    p.retries = 1;
    println!("model={} base_url={} keys={}", p.model, p.base_url, p.keys.len());

    let msgs = vec![
        llm::Message::system("你只回复一个词。"),
        llm::Message::user("回复：连通"),
    ];
    match llm::call(&p, &msgs) {
        Ok(out) => {
            println!("✓ LLM 调用成功");
            println!("  content   = {:?}", out.content.trim());
            println!("  reasoning = {:?}", out.reasoning.chars().take(40).collect::<String>());
        }
        Err(e) => {
            println!("✗ LLM 调用失败：{e}");
            std::process::exit(1);
        }
    }
}
