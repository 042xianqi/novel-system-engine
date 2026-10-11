//! 内置任务 —— 每个任务都是 Runtime **天生知道怎么跑**的，不是动态加载的插件。
//!
//! M1 只有 `chat`。`memory` 在 M2，`novel` 在 M3。
//!
//! 任务只做编排：拿到 [`HarnessCtx`] 里的公共能力（模型 / 工具 / 会话 / 提示词）
//! 去干活；这些能力本身不属于任何任务，聊天与写小说共享同一套底层。

use crate::config::RuntimeConfig;
use crate::harness::{self, HarnessCtx, StopReason};
use crate::memory::{self, MemoryStore};
use crate::prompt::TurnPrefix;
use crate::session::SessionStore;
use crate::tool::Tool;
use anyhow::Result;
use std::io::{BufRead, Write};
use std::path::PathBuf;

/// 构造 HarnessCtx（消除多个入口的重复）。
#[allow(clippy::too_many_arguments)]
fn make_ctx<'a>(
    store: &'a SessionStore,
    session: &str,
    cfg: &'a RuntimeConfig,
    llm: ne_engine::llm::LlmParams,
    work_root: PathBuf,
    prompt_dir: PathBuf,
    enabled: Vec<Tool>,
    pinned_section: String,
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
        pinned_section,
        mcp_servers: if cfg.mcp.servers.is_empty() {
            None
        } else {
            Some(cfg.mcp.servers.clone())
        },
        approval_mode: crate::approval::ApprovalMode::from_id(&cfg.approval.mode),
        plan_only: cfg.approval.plan_only,
        // 默认无审批回调 = 非交互式 → 需审批的操作会被拒（安全默认）。
        // 交互式入口（chat）会自行 .with_approver(...) 覆盖。
        approver: None,
    }
}

impl<'a> HarnessCtx<'a> {
    /// 挂上审批回调（交互式入口用）。
    pub fn with_approver(
        mut self,
        f: Box<dyn Fn(&crate::approval::Decision) -> bool + 'a>,
    ) -> Self {
        self.approver = Some(f);
        self
    }
}

/// 取 pinned 记忆段（进 system 稳定前缀）。
fn pinned_snapshot(mem: &MemoryStore, workspace: &str, session: &str) -> String {
    match mem.pinned(workspace, session) {
        Ok(ms) => memory::render_pinned_section(&ms),
        Err(_) => String::new(), // 记忆故障不得阻断对话
    }
}

/// 构造本轮的逐轮前缀（recall 召回，挂最新 user 轮）。
fn turn_prefix(mem: &MemoryStore, cfg: &RuntimeConfig, workspace: &str, session: &str, input: &str) -> TurnPrefix {
    let mut tp = TurnPrefix::default();
    let cands = match mem.candidates(workspace, session, 64) {
        Ok(c) => c,
        Err(_) => return tp,
    };
    if cands.is_empty() {
        return tp;
    }
    let picked = memory::select_recall(input, &cands, memory::now_ms());
    if picked.is_empty() {
        return tp;
    }
    let block = memory::render_recall_block(&picked);
    if !block.is_empty() {
        tp.blocks.push(block);
    }
    let _ = cfg; // 预算参数 M5 用
    tp
}

/// 跑一轮（落 user → 多轮工具循环 → 落最终回复）。
fn run_one(h: &HarnessCtx, mem: Option<&MemoryStore>, input: &str) -> Result<String> {
    // 先落 user 再跑
    harness::persist_user(h, input)?;
    // recall 在 user 落盘后计算（其内容只影响本轮的 provider 投影）
    let prefix = match mem {
        Some(m) => turn_prefix(m, h.cfg, &h.work_root.display().to_string(), &h.session, input),
        None => TurnPrefix::default(),
    };
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
#[allow(clippy::too_many_arguments)]
pub fn chat_interactive(
    store: &SessionStore,
    mem: &MemoryStore,
    session: &str,
    cfg: &RuntimeConfig,
    llm: ne_engine::llm::LlmParams,
    work_root: PathBuf,
    prompt_dir: PathBuf,
    enabled: Vec<Tool>,
) -> Result<()> {
    let workspace = work_root.display().to_string();
    let pinned = pinned_snapshot(mem, &workspace, session);
    let h = make_ctx(store, session, cfg, llm, work_root, prompt_dir, enabled, pinned);
    // 交互式 → 挂审批回调（在 stdin 上问 y/N）
    let h = h.with_approver(Box::new(interactive_approver));
    let hist = store.history(session)?;
    println!("ne-agent 对话 · 会话 {session}");
    println!("  已有 {} 条历史 · /exit 退出 · /history 历史 · /tools 工具 · /memory 记忆", hist.len());
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
            "/memory" => {
                let (total, npin) = mem.count().unwrap_or((0, 0));
                println!("  记忆共 {total} 条（pinned {npin}）");
                for m in mem.all().unwrap_or_default() {
                    println!("  [{}/{}]{} {} = {}", m.scope, m.kind, if m.pinned { " 📌" } else { "" }, m.key, first_line(&m.value));
                }
                continue;
            }
            _ => {}
        }
        match run_one(&h, Some(mem), input) {
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
/// 交互式审批：在 stdin 上问 y/N（默认否）。
///
/// 安全默认：无法读取输入（EOF）→ 拒绝。
fn interactive_approver(d: &crate::approval::Decision) -> bool {
    eprint!(
        "\n⚠ 需要批准（风险 {}）\n  {}\n  原因：{}\n批准执行？[y/N] ",
        d.risk, d.summary, d.reason
    );
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).unwrap_or(0) == 0 {
        eprintln!("（无法读取输入 → 拒绝）");
        return false;
    }
    let a = line.trim().to_ascii_lowercase();
    a == "y" || a == "yes" || a == "是"
}

pub fn chat_once(
    store: &SessionStore,
    mem: &MemoryStore,
    session: &str,
    cfg: &RuntimeConfig,
    llm: ne_engine::llm::LlmParams,
    work_root: PathBuf,
    prompt_dir: PathBuf,
    enabled: Vec<Tool>,
    message: &str,
) -> Result<String> {
    let workspace = work_root.display().to_string();
    let pinned = pinned_snapshot(mem, &workspace, session);
    let h = make_ctx(store, session, cfg, llm, work_root, prompt_dir, enabled, pinned);
    run_one(&h, Some(mem), message)
}

fn first_line(s: &str) -> String {
    let l = s.lines().next().unwrap_or("");
    if l.chars().count() > 60 {
        format!("{}…", l.chars().take(60).collect::<String>())
    } else {
        l.to_string()
    }
}

// ───────────────────────────── 小说任务（M3）

/// 一章生成的产物与结果。
#[derive(Debug, Clone)]
pub struct NovelOutcome {
    pub chapter: u32,
    pub volume: String,
    /// 正文文本
    pub text: String,
    /// 落盘路径（dry-run 时为空）
    pub written_to: Option<String>,
    /// 门禁判决：pass | fix | fail
    pub verdict: String,
    /// P0/P1 计数
    pub p0: usize,
    pub p1: usize,
    /// 门禁明细（P1 前若干条）
    pub notes: Vec<String>,
}

impl NovelOutcome {
    pub fn render(&self) -> String {
        let mut s = format!(
            "第 {} 章（{}） {} 字 · 门禁 {}\n",
            self.chapter,
            self.volume,
            self.text.chars().count(),
            self.verdict.to_uppercase()
        );
        if let Some(p) = &self.written_to {
            s.push_str(&format!("  已落盘：{p}\n"));
        } else {
            s.push_str("  （dry-run，未落盘）\n");
        }
        s.push_str(&format!("  P0 {} · P1 {}\n", self.p0, self.p1));
        for n in &self.notes {
            s.push_str(&format!("  · {n}\n"));
        }
        s
    }
}

/// 生成一章。**这是 M3 的主干：把既有 ne-engine 能力接成一个任务。**
///
/// 流程（每一步都用现成能力，不重写业务）：
///   1. `NovelCtx::open(lib_root)` —— 只读绑定库
///   2. `chapter::build_prompt` —— 细纲 + 注入包 + 模板 → 完整正文 prompt
///   3. `ne_engine::llm::call` —— 生成正文
///   4. `chapter::write_chapter` —— 落盘（`dry_run` 时跳过）
///   5. `gates::run_all` —— 16 门禁校验
///
/// 与既有口径一致：`RepoCtx` 负责 GateCtx（与 `ne-engine check-file` 同源），
/// `NovelCtx` 负责 BudgetCtx/ChapterCtx（生成侧）。
#[allow(clippy::too_many_arguments)]
pub fn novel_chapter(
    lib_root: &std::path::Path,
    volume: &str,
    chapter: u32,
    llm: &ne_engine::llm::LlmParams,
    work_root: &std::path::Path,
    prompt_dir: &std::path::Path,
    dry_run: bool,
) -> Result<NovelOutcome> {
    if !lib_root.join("novel.db").exists() && !lib_root.join("03_叙事规划").exists() {
        anyhow::bail!(
            "库根不像小说资料库（既无 novel.db 也无 03_叙事规划）：{}",
            lib_root.display()
        );
    }

    let ctx = crate::novel_ctx::NovelCtx::open(lib_root)?;
    // prompt 模板可被 vault 的 prompts/ 覆盖（双层资源）
    let ctx = ctx.with_prompt_fallback(prompt_dir);

    // 1) 组装 prompt
    let prompt = ne_engine::chapter::build_prompt(&ctx, chapter, volume)
        .map_err(|e| anyhow::anyhow!("组装正文 prompt 失败：{e}"))?;

    // 2) 调模型
    let msgs = vec![
        ne_engine::llm::Message {
            role: "system".into(),
            content: "你是《残响之河》的小说撰稿者。严格按用户给出的写作指令输出正文，不要复述指令。".into(),
        },
        ne_engine::llm::Message { role: "user".into(), content: prompt },
    ];
    let out = ne_engine::llm::call(llm, &msgs)
        .map_err(|e| anyhow::anyhow!("模型调用失败：{e}"))?;
    let text = out.content.trim().to_string();
    if text.is_empty() {
        anyhow::bail!("模型返回空正文");
    }

    // 3) 落盘
    let written_to = if dry_run {
        None
    } else {
        Some(
            ne_engine::chapter::write_chapter(lib_root, &text, chapter, volume)
                .map_err(|e| anyhow::anyhow!("落盘失败：{e}"))?,
        )
    };

    // 4) 门禁（与 ne-engine check-file 同源：RepoCtx 实现 GateCtx）
    let cfg = ne_engine::config::load(lib_root)
        .map_err(|e| anyhow::anyhow!("读库配置失败（库根 {}）：{e}", lib_root.display()))?;
    let repo_ctx = ne_engine::ctx::RepoCtx::open(lib_root)
        .map_err(|e| anyhow::anyhow!("打开门禁上下文失败：{e}"))?;
    let report = ne_engine::gates::run_all(&text, chapter, volume, &cfg, &repo_ctx);

    let mut notes = Vec::new();
    for f in report.p0.iter().take(5) {
        notes.push(format!("P0 {}: {}", f.gate, first_line(&f.issue)));
    }
    for f in report.p1.iter().take(5) {
        notes.push(format!("P1 {}: {}", f.gate, first_line(&f.issue)));
    }

    let _ = work_root;
    Ok(NovelOutcome {
        chapter,
        volume: volume.to_string(),
        text,
        written_to,
        verdict: report.verdict,
        p0: report.p0.len(),
        p1: report.p1.len(),
        notes,
    })
}

// ───────────────────────────── 测试辅助（供 tests/approval_e2e.rs 用）
//
// 这些是 `pub` 的薄包装，仅用于端到端测试装配 HarnessCtx + 跑一轮，
// 不改变生产路径。放在 task.rs（已依赖 harness/session/config）避免测试重复装配逻辑。

/// 装配一个 HarnessCtx（测试用，可挂审批回调）。
#[allow(clippy::too_many_arguments)]
pub fn make_ctx_for_test<'a>(
    store: &'a SessionStore,
    session: &str,
    cfg: &'a RuntimeConfig,
    llm: ne_engine::llm::LlmParams,
    work_root: PathBuf,
    prompt_dir: PathBuf,
    enabled: Vec<Tool>,
    pinned: String,
    approver: Option<Box<dyn Fn(&crate::approval::Decision) -> bool + 'a>>,
) -> HarnessCtx<'a> {
    let mut h = make_ctx(store, session, cfg, llm, work_root, prompt_dir, enabled, pinned);
    h.approver = approver;
    h
}

/// 跑一轮（测试用，不带记忆召回）。
pub fn run_one_for_test(h: &HarnessCtx, input: &str) -> Result<String> {
    run_one(h, None, input)
}
