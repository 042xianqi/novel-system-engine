//! 内置任务 —— 每个任务都是 Runtime **天生知道怎么跑**的，不是动态加载的插件。
//!
//! M1 只有 `chat`。`memory` 在 M2，`novel` 在 M3。
//!
//! 任务只做编排：拿到 [`HarnessCtx`] 里的公共能力（模型 / 工具 / 会话 / 提示词）
//! 去干活；这些能力本身不属于任何任务，聊天与写小说共享同一套底层。

use crate::config::RuntimeConfig;
use crate::harness::{self, HarnessCtx, StopReason};
use crate::prompt::TurnPrefix;
use crate::session::SessionStore;
use crate::tool::Tool;
use anyhow::Result;
use std::io::{BufRead, Write};
use std::path::PathBuf;

/// 构造 HarnessCtx（消除三个入口的重复）。
#[allow(clippy::too_many_arguments)]
fn make_ctx<'a>(
    store: &'a SessionStore,
    session: &str,
    cfg: &'a RuntimeConfig,
    llm: ne_engine::llm::LlmParams,
    work_root: PathBuf,
    prompt_dir: PathBuf,
    enabled: Vec<Tool>,
) -> HarnessCtx<'a> {
    HarnessCtx {
        store,
        session: session.to_string(),
        cfg,
        llm,
        work_root,
        prompt_dir,
        enabled,
        max_tool_calls_per_turn: 0,
        max_rounds: 0,
        max_consecutive_failures: 0,
    }
}

/// 跑一轮（落 user → 多轮工具循环 → 落最终回复）。
fn run_one(h: &HarnessCtx, input: &str) -> Result<String> {
    // M1：无记忆系统，TurnPrefix 为空（M2 在这里注入 recall）
    let prefix = TurnPrefix::default();
    // ⚠ 先落 user 再跑
    harness::persist_user(h, input)?;
    let outcome = harness::run_session(h, &prefix)?;
    harness::persist_final(h, &outcome)?;

    let mut out = outcome.final_text.clone();
    if let Some(w) = &outcome.warning {
        out.push_str(&format!("\n⚠ {w}"));
    }
    if outcome.stop == StopReason::BudgetExhausted || outcome.stop == StopReason::FailureCircuit {
        out.push_str(&format!(
            "\n（共 {} 轮 · {} 次工具调用）",
            outcome.rounds,
            outcome.tool_results.len()
        ));
    }
    Ok(out)
}

/// 交互式对话。
pub fn chat_interactive(
    store: &SessionStore,
    session: &str,
    cfg: &RuntimeConfig,
    llm: ne_engine::llm::LlmParams,
    work_root: PathBuf,
    prompt_dir: PathBuf,
    enabled: Vec<Tool>,
) -> Result<()> {
    let h = make_ctx(store, session, cfg, llm, work_root, prompt_dir, enabled);
    let hist = store.history(session)?;
    println!("ne-agent 对话 · 会话 {session}");
    println!("  已有 {} 条历史 · /exit 退出 · /history 查看 · /tools 列出工具", hist.len());
    println!();

    let stdin = std::io::stdin();
    loop {
        print!("> ");
        std::io::stdout().flush().ok();
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            println!();
            break;
        }
        let input = line.trim();
        if input.is_empty() {
            continue;
        }
        match input {
            "/exit" | "/quit" => break,
            "/history" => {
                for m in store.history(session)? {
                    println!("  [{}] {}", m.role, first_line(&m.content));
                }
                continue;
            }
            "/tools" => {
                for t in &h.enabled {
                    println!("  {:<8} {:<6?} {}", t.name(), t.tier(), t.description());
                }
                continue;
            }
            _ => {}
        }
        match run_one(&h, input) {
            Ok(text) => {
                if !text.trim().is_empty() {
                    println!("{text}");
                }
            }
            Err(e) => eprintln!("  ✗ 本轮失败：{e:#}"),
        }
    }
    Ok(())
}

/// 单次（非交互）对话：脚本 / 调试用。
#[allow(clippy::too_many_arguments)]
pub fn chat_once(
    store: &SessionStore,
    session: &str,
    cfg: &RuntimeConfig,
    llm: ne_engine::llm::LlmParams,
    work_root: PathBuf,
    prompt_dir: PathBuf,
    enabled: Vec<Tool>,
    message: &str,
) -> Result<String> {
    let h = make_ctx(store, session, cfg, llm, work_root, prompt_dir, enabled);
    run_one(&h, message)
}

fn first_line(s: &str) -> String {
    let l = s.lines().next().unwrap_or("");
    if l.chars().count() > 60 {
        format!("{}…", l.chars().take(60).collect::<String>())
    } else {
        l.to_string()
    }
}
