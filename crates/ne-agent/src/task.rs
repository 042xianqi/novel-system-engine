//! 内置任务 —— 每个任务都是 Runtime **天生知道怎么跑**的，不是动态加载的插件。
//!
//! M1 只有 `chat`。`memory` 在 M2，`novel` 在 M3。
//!
//! 任务只做编排：拿到 [`HarnessCtx`] 里的公共能力（模型 / 工具 / 会话 / 提示词）
//! 去干活；这些能力本身不属于任何任务，聊天与写小说共享同一套底层。

use crate::config::RuntimeConfig;
use crate::governance::governed_call::ne_agent_gov_call;
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
    vault_root: PathBuf,
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
        system_override: None,
        vault_root,
        asker: None,
        // 生产路径：不注入传输层 → 必经三重锁
        llm_transport: None,
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
///
/// 返回 `(正文, 思考过程)` —— 思考过程供宿主 UI 以「思考」气泡展示。
fn run_one(h: &HarnessCtx, mem: Option<&MemoryStore>, input: &str) -> Result<(String, String)> {
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
    Ok((out, outcome.reasoning))
}

/// 思考过程标记（宿主 App 用它把思考与正文分开渲染）。
///
/// <p>内容含换行，必须用**显式标记**定界，不能靠"前缀行"判断。</p>
pub const REASONING_OPEN: &str = "<<<NE_REASONING>>>";
pub const REASONING_CLOSE: &str = "<<<END_NE_REASONING>>>";

/// 流式转发状态（供调用方判断是否需要兜底整段打印）。
#[derive(Default)]
pub struct StreamSinkFlags {
    pub saw_reasoning: std::sync::atomic::AtomicBool,
    pub saw_content: std::sync::atomic::AtomicBool,
    /// 思考段当前是否处于"已开未闭"状态（决定要不要写 CLOSE）。
    reasoning_open: std::sync::atomic::AtomicBool,
}

/// 安装「实时转发」回调：模型每吐一段就立刻写到**对应的**输出流。
///
/// <p>这是「真流式」的关键：思考 → stderr（带标记），正文 → stdout（原样）。</p>
/// <p>若等 `run_one` 跑完再整段打印，思考与答案会同一瞬间出现 —— 用户看到的
/// 正是这种"一块出来"的假象。</p>
pub fn install_stream_sink() -> std::sync::Arc<StreamSinkFlags> {
    use std::io::Write;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;

    let f = Arc::new(StreamSinkFlags::default());
    let f2 = f.clone();
    ne_engine::llm::set_delta_sink(Some(Box::new(move |kind: &str, text: &str| {
        let open = f2.reasoning_open.load(Ordering::SeqCst);
        match kind {
            "reasoning" => {
                let mut e = std::io::stderr();
                if !open {
                    let _ = e.write_all(REASONING_OPEN.as_bytes());
                    f2.reasoning_open.store(true, Ordering::SeqCst);
                }
                let _ = e.write_all(text.as_bytes());
                let _ = e.flush();
                f2.saw_reasoning.store(true, Ordering::SeqCst);
            }
            _ => {
                let mut o = std::io::stdout();
                // 正文开始 → 立刻闭合思考段（保证思考先于答案出现）
                if open {
                    let mut e = std::io::stderr();
                    let _ = e.write_all(REASONING_CLOSE.as_bytes());
                    let _ = e.flush();
                    f2.reasoning_open.store(false, Ordering::SeqCst);
                }
                let _ = o.write_all(text.as_bytes());
                let _ = o.flush();
                f2.saw_content.store(true, Ordering::SeqCst);
            }
        }
    })));
    f
}

/// 卸载回调，并确保思考段被闭合。
pub fn uninstall_stream_sink(f: &StreamSinkFlags) {
    use std::io::Write;
    use std::sync::atomic::Ordering;
    ne_engine::llm::set_delta_sink(None);
    if f.reasoning_open.swap(false, Ordering::SeqCst) {
        let mut e = std::io::stderr();
        let _ = e.write_all(REASONING_CLOSE.as_bytes());
        let _ = e.flush();
    }
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
    vault_root: PathBuf,
) -> Result<()> {
    let workspace = work_root.display().to_string();
    let pinned = pinned_snapshot(mem, &workspace, session);
    let h = make_ctx(store, session, cfg, llm, work_root, prompt_dir, enabled, pinned, vault_root);
    // 交互式 → 挂审批回调（在 stdin 上问 y/N）+ ask_user 回调
    let mut h = h.with_approver(Box::new(interactive_approver));
    h.asker = Some(interactive_asker);
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
        let sink = install_stream_sink();
        let r = run_one(&h, Some(mem), input);
        uninstall_stream_sink(&sink);
        match r {
            Ok((text, reasoning)) => {
                use std::sync::atomic::Ordering;
                // 兜底 1：模型没走流式回调（如纯机械摘要 / 网关不支持流式）→ 整段打印思考。
                if !sink.saw_reasoning.load(Ordering::SeqCst) && !reasoning.trim().is_empty() {
                    eprintln!("{REASONING_OPEN}{reasoning}{REASONING_CLOSE}");
                }
                // 兜底 2：正文也不流式时，整段补打（否则用户什么都看不到）。
                if !sink.saw_content.load(Ordering::SeqCst) && !text.trim().is_empty() {
                    println!("{text}");
                }
            }
            Err(e) => eprintln!("  ✗ 本轮失败：{e:#}"),
        }
        // 与本轮回答分隔：回答是流式写出的（末尾无换行），若不补换行，下一次
        // 提示符会紧贴正文出现（形如 "…）。> "），宿主 App 便无法按 "\n> " 判定
        // 轮次边界 → 第二轮会追加进第一轮气泡。这里补一个换行，保证提示符恒为
        // "\n> "（首个提示符前有 banner 的换行，同样满足）。
        println!();
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

/// 交互式 ask_user：把问题打到 stderr，从 stdin 读一行回答。
///
/// 与审批同通道（都是 stdin 交互）。非交互环境（EOF）→ 返回 Err，
/// 工具层会把错误明确回写模型（而非静默返回空答案）。
fn interactive_asker(questions: &str) -> Result<String, String> {
    eprintln!("\n❓ 需要你的回答：\n{questions}");
    eprint!("回答 > ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    match std::io::stdin().read_line(&mut line) {
        Ok(0) => Err("无法读取输入（EOF）：当前为非交互环境".into()),
        Ok(_) => Ok(line.trim().to_string()),
        Err(e) => Err(format!("读取输入失败：{e}")),
    }
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
    vault_root: PathBuf,
) -> Result<(String, String)> {
    let workspace = work_root.display().to_string();
    let pinned = pinned_snapshot(mem, &workspace, session);
    let h = make_ctx(store, session, cfg, llm, work_root, prompt_dir, enabled, pinned, vault_root);
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
    /// P1-PARAGRAPH 实际修改的段落数（0=未改排版）
    pub para_fixed: usize,
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
        if self.para_fixed > 0 {
            s.push_str(&format!("  P1-PARAGRAPH_FIX 修改段落数：{}\n", self.para_fixed));
        }
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
///
/// `use_memory=true` 时启用记忆子系统（preprocess/三层提示词/post_update）；
/// `false` 时逐字节等于历史行为（见 [`novel_chapter`]）。
/// 记忆子系统 pre 阶段的结果（供 system prompt 组装与 token 校验）。
pub(crate) struct MemPre {
    pub cfg: crate::memory_system::MemConfig,
    pub memory_inject: String,
    pub compressed_outline: String,
    pub conn: rusqlite::Connection,
}

/// 记忆子系统预处理（§一）：状态冲突 → Err（不调模型）；否则返回注入片段 + 压缩细纲。
fn memory_pipeline_pre(
    lib_root: &std::path::Path,
    volume: &str,
    chapter: u32,
) -> Result<MemPre> {
    let conn = crate::memory_system::open(&lib_root.join("novel.db"))?;
    let cfg = crate::memory_system::MemConfig::load(&conn);
    // 读原始细纲（本轮：直接从 03_叙事规划/细纲/<卷>.md 取该章）
    let outline_path = lib_root.join("03_叙事规划/细纲").join(format!("{volume}.md"));
    let raw = std::fs::read_to_string(&outline_path).unwrap_or_default();
    let out = crate::memory_system::preprocess(&conn, volume, chapter, &raw)?;
    // 状态冲突校验：冲突直接 Err，不进入模型调用
    crate::api::check_state_conflict(&conn, volume, chapter, &out.compressed_outline)?;
    Ok(MemPre {
        cfg,
        memory_inject: out.memory_inject,
        compressed_outline: out.compressed_outline,
        conn,
    })
}

/// 组装 system prompt：有记忆 → 三层分层；无记忆 → 旧单行（逐字节不变）。
fn build_system_prompt(
    lib_root: &std::path::Path,
    volume: &str,
    chapter: u32,
    mem: &Option<MemPre>,
) -> Result<String> {
    match mem {
        Some(m) => {
            let sp = crate::api::build_layered_system(
                &m.conn,
                volume,
                chapter,
                &m.memory_inject,
                &m.compressed_outline,
            );
            Ok(sp.assemble())
        }
        None => {
            let book_title = ne_engine::config::load(lib_root)
                .map(|c| c.book_profile.title().to_string())
                .unwrap_or_else(|_| ne_engine::gates::BOOK_TITLE_DEFAULT.to_string());
            Ok(format!(
                "你是《{book_title}》的小说撰稿者。严格按用户给出的写作指令输出正文，不要复述指令。"
            ))
        }
    }
}

/// 清洗模型输出的正文文本（2026-10-15）。
///
/// 背景：模型常在正文末尾追加「元评论块」——如 `---` 分隔线 +「**写完自查**：…」
/// 或「【技法计划】…」。模板已明令禁止，但模型仍偶发。此处做**保守的尾部裁剪**：
/// 只在明确命中元评论标记时，从该标记处截断（连同其前的分隔线一并去掉）；
/// 不命中则原样返回，不误伤正常正文。
pub fn sanitize_story_text(raw: &str) -> String {
    // 1) 剥离 [[CANON_TEXT_START]]/[[CANON_TEXT_END]] 标记行，保留内部定版文本。
    //    标记只属于指令层约定，不该落进正文文件（第2/3章均无标记，此处兜底）。
    let mut raw = raw
        .lines()
        .filter(|l| {
            let t = l.trim();
            t != "[[CANON_TEXT_START]]" && t != "[[CANON_TEXT_END]]"
        })
        .collect::<Vec<_>>()
        .join("\n");
    // 若标记与其包裹文本同行（如「……。[[CANON_TEXT_START]]滑倒了……」），兜底做字符级剥离
    for m in ["[[CANON_TEXT_START]]", "[[CANON_TEXT_END]]"] {
        raw = raw.replace(m, "");
    }
    // 2) 截断模型尾部元评论（既有行为）
    const MARKERS: [&str; 5] = ["**写完自查**", "写完自查", "【技法计划】", "【自查】", "【自检】"];
    let mut cut_at: Option<usize> = None;
    for m in MARKERS {
        if let Some(i) = raw.find(m) {
            cut_at = Some(match cut_at {
                Some(c) => c.min(i),
                None => i,
            });
        }
    }
    let body = match cut_at {
        Some(i) => raw[..i]
            .trim_end_matches(|c: char| c == '-' || c == '*' || c == '—' || c.is_whitespace()),
        None => raw.trim_end_matches(char::is_whitespace),
    };
    format!("{}\n", body.trim_end())
}

/// 旧签名兼容入口（`use_memory = false`，行为与历史逐字节一致）。
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
    novel_chapter_with_memory(
        lib_root, volume, chapter, llm, work_root, prompt_dir, dry_run, false,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn novel_chapter_with_memory(
    lib_root: &std::path::Path,
    volume: &str,
    chapter: u32,
    llm: &ne_engine::llm::LlmParams,
    work_root: &std::path::Path,
    prompt_dir: &std::path::Path,
    dry_run: bool,
    use_memory: bool,
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

    // 1) 组装 prompt（原有逻辑零改动）
    let prompt = ne_engine::chapter::build_prompt(&ctx, chapter, volume)
        .map_err(|e| anyhow::anyhow!("组装正文 prompt 失败：{e}"))?;

    // 1.5) 记忆子系统（仅 use_memory=true 时启用；默认关 = 旧行为逐字节不变）
    //      严格按照文档 §一：preprocess 在调模型前，post_update 在门禁后。
    let mem = if use_memory {
        Some(memory_pipeline_pre(lib_root, volume, chapter)?)
    } else {
        None
    };
    let msgs = {
        let sys = build_system_prompt(lib_root, volume, chapter, &mem)?;
        vec![
            ne_engine::llm::Message::system(sys),
            ne_engine::llm::Message::user(prompt),
        ]
    };
    // 上下文膨胀监控（§：只告警，不截断、不阻断、不改裁剪逻辑）。
    // 位置：build_prompt 组装完毕、即将送出生成之前。
    {
        const WARN_CHARS: usize = 85_000;
        let sys_chars = msgs[0].content.chars().count();
        let user_chars = msgs[1].content.chars().count();
        let total_chars = sys_chars + user_chars;
        if total_chars >= WARN_CHARS {
            eprintln!(
                "WARN[context-bloat] 第{chapter}章 总输入 {total_chars} 字                  （system={sys_chars} + user={user_chars}）≥ 告警线 {WARN_CHARS} 字；                 仅告警，未截断，继续生成。"
            );
        }
    }
    // 总输入 token 校验（§一：超限直接报错终止，禁止调用模型）
    if let Some(m) = &mem {
        let cfg = m.cfg.clone();
        let total = crate::memory_system::est_tokens(&msgs[0].content)
            + crate::memory_system::est_tokens(&msgs[1].content);
        if total > cfg.total_input_max_token {
            anyhow::bail!(
                "总输入 token={total} 超上限 {}（记忆子系统拒绝调用模型）",
                cfg.total_input_max_token
            );
        }
    }
    let out = ne_agent_gov_call(llm, &msgs)
        .map_err(|e| anyhow::anyhow!("模型调用失败：{e}"))?;
    let text = sanitize_story_text(out.content.trim());
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
// 4.5) P1-PARAGRAPH 排版修复回写：仅调分段，一字不改（不落盘时只报数）
    let para_fixed = report
        .gates
        .get("P1_PARAGRAPH")
        .map(|g| {
            g.detail
                .split("段落修复=")
                .nth(1)
                .and_then(|s| s.split('处').next())
                .and_then(|s| s.trim().parse::<usize>().ok())
                .unwrap_or(0)
        })
        .unwrap_or(0);
    if let Some(fixed) = &report.fixed_text {
        if !dry_run {
            if let Some(p) = &written_to {
                std::fs::write(p, format!("{}\n", fixed.trim()))
                    .map_err(|e| anyhow::anyhow!("P1-PARAGRAPH 回写失败：{e}"))?;
            }
        }
        notes.push(format!("P1-PARAGRAPH_FIX 修改段落数：{para_fixed}（仅排版，文字未改）"));
    }


    // 5) 接线回写：情绪引擎递推 -> char_emotion / char_memory / dynamics_events
    if !dry_run {
        let vol_codes: Vec<String> = {
            let inj = ne_engine::chapter::ChapterCtx::injection(&ctx, volume, chapter).unwrap_or(serde_json::json!({}));
            inj.get("chars")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|c| c.get("code").and_then(|x| x.as_str()).map(|s| s.to_string())).collect())
                .unwrap_or_default()
        };
        let vol_events: Vec<String> = {
            let inj = ne_engine::chapter::ChapterCtx::injection(&ctx, volume, chapter).unwrap_or(serde_json::json!({}));
            inj.get("events")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|e| e.get("id").and_then(|x| x.as_str()).map(|s| s.to_string())).collect())
                .unwrap_or_default()
        };
        match ne_engine::wire::commit_chapter(
            &lib_root.join("novel.db"),
            lib_root,
            volume,
            chapter,
            &vol_codes,
            &vol_events,
        ) {
            Ok(done) => {
                for (code, msg) in done.iter().take(3) {
                    notes.push(format!("情绪回写 {code}: {msg}"));
                }
            }
            Err(e) => notes.push(format!("情绪回写失败（不阻断正文）：{e}")),
        }
    }
    // 5.5) 记忆子系统 post 阶段（§一）：保守实体解析 + 短期记忆 + 30 章快照。
    //      放在门禁与回写之后、返回之前；失败只记 note，不阻断正文。
    if let Some(m) = &mem {
        if !dry_run {
            match crate::memory_system::post_update(&m.conn, volume, chapter, &text) {
                Ok(rep) => {
                    notes.push(format!(
                        "记忆回写：提取 {} 项，警告 {}，裁旧 {} 章{}",
                        rep.extracted.len(),
                        rep.warnings.len(),
                        rep.short_term_pruned,
                        rep.snapshot_created
                            .as_ref()
                            .map(|s| format!("，快照 {s}"))
                            .unwrap_or_default()
                    ));
                    for w in rep.warnings.iter().take(3) {
                        notes.push(format!("记忆警告：{w}"));
                    }
                    // 章节元数据存档（原始细纲 + 压缩细纲 + 正文 + token 统计）
                    let outline_path = lib_root
                        .join("03_叙事规划/细纲")
                        .join(format!("{volume}.md"));
                    let raw = std::fs::read_to_string(&outline_path).unwrap_or_default();
                    let in_tok = crate::memory_system::est_tokens(&m.memory_inject)
                        + crate::memory_system::est_tokens(&m.compressed_outline);
                    let out_tok = crate::memory_system::est_tokens(&text);
                    if let Err(e) = crate::memory_system::save_chapter_meta(
                        &m.conn,
                        volume,
                        chapter,
                        &raw,
                        &m.compressed_outline,
                        &text,
                        rep.snapshot_created.as_deref(),
                        in_tok,
                        out_tok,
                        &rep.warnings,
                    ) {
                        notes.push(format!("chapter_meta 存档失败（不阻断）：{e}"));
                    } else {
                        notes.push(format!(
                            "chapter_meta 已存档（in≈{in_tok} / out≈{out_tok} token）"
                        ));
                    }
                }
                Err(e) => notes.push(format!("记忆回写失败（不阻断正文）：{e}")),
            }
        }
    }
    let _ = work_root;
    // 返回最终文本：若发生排版修复，text 也同步为修复后（与落盘一致）
    let final_text = report.fixed_text.clone().unwrap_or_else(|| text.clone());
    Ok(NovelOutcome {
        chapter,
        volume: volume.to_string(),
        text: final_text,
        written_to,
        verdict: report.verdict,
        p0: report.p0.len(),
        p1: report.p1.len(),
        notes,
        para_fixed,
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
    let vault_root = work_root.clone();
    let mut h = make_ctx(store, session, cfg, llm, work_root, prompt_dir, enabled, pinned, vault_root);
    h.approver = approver;
    h
}

/// 跑一轮（测试用，不带记忆召回）。返回正文（思考过程由 `run_one` 返回的第二项）。
pub fn run_one_for_test(h: &HarnessCtx, input: &str) -> Result<String> {
    Ok(run_one(h, None, input)?.0)
}

/// **绕过三重锁的测试装配**（2026-10-18）。
///
/// 与 [`make_ctx_for_test`] 唯一区别：把 `llm_transport` 换成一个
/// **直接调 `ne_engine::llm::call`** 的闭包，从而不受 `governed_call` 拦截。
///
/// # 为什么不能只靠环境变量放开锁
///
/// 三重锁的第 1 条是**编译期 feature**，测试跑在默认构建下必然不满足。
/// 若为此改 `compile_feature_enabled()` 的行为，等于让所有生产二进制也解除锁
/// —— 那是把安全边界拆了去迁就测试。
///
/// # 安全保证
///
/// 1. 本函数在 `task.rs`，是 `pub` 但**只在 `tests/` 里被引用**
///    （`grep make_ctx_for_test_unlocked` 可验证生产代码零调用）。
/// 2. 名字带 `_for_test`，任何人 review 时一眼看出用途。
/// 3. 它仍然走真实的 HTTP 栈（`llm::call`），所以测的是真链路，
///    只是目标地址由测试指定（`127.0.0.1` 假 LLM）。
#[allow(clippy::too_many_arguments)]
pub fn make_ctx_for_test_unlocked<'a>(
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
    let mut h = make_ctx_for_test(
        store, session, cfg, llm, work_root, prompt_dir, enabled, pinned, approver,
    );
    h.llm_transport = Some(std::sync::Arc::new(|p, msgs| ne_engine::llm::call(p, msgs)));
    h
}

// ───────────────────────────── 子智能体 Lane（M7）

/// 构造一个"父会话"的 HarnessCtx（供 CLI 的 `subagent` 命令调用）。
///
/// 与 `chat` 用的同一套装配；`pinned` 传空字符串即可（子任务不依赖父的召回快照）。
#[allow(clippy::too_many_arguments)]
pub fn parent_ctx<'a>(
    store: &'a SessionStore,
    session: &str,
    cfg: &'a RuntimeConfig,
    llm: ne_engine::llm::LlmParams,
    work_root: PathBuf,
    prompt_dir: PathBuf,
    enabled: Vec<Tool>,
) -> HarnessCtx<'a> {
    let vault_root = work_root.clone();
    make_ctx(store, session, cfg, llm, work_root, prompt_dir, enabled, String::new(), vault_root)
}

/// 执行一批子任务 Lane（**顺序**，每个子任务一个隔离子会话）。
///
/// 契约对照太墟 `invoke_subagent`（`SubagentArgsParser` + `SubagentTaskSpec`）。
///
/// # 为什么是顺序而非并发（如实说明）
///
/// 太墟 Lane 真并发（Android 宿主、多面板）。ne-agent 运行时是 **std 阻塞单线程**，
/// 且写小说是长链顺序任务——并发既无收益又会让"同一章被两个 Lane 写"。
/// 所以这里**顺序执行**，但保留太墟的**写租约校验**（提前发现配置冲突）与
/// **独立子会话隔离**（上下文不串）。
///
/// # 隔离
///
/// 每个子任务在自己的会话里跑（`parent_session_id` 指向父会话），
/// 记忆召回按子会话走 → 父会话与其它 Lane 都看不到这个 Lane 的中间过程。
pub fn run_subagents(
    parent: &HarnessCtx,
    tasks: &[crate::subagent::SubagentTask],
    max_tasks: usize,
) -> Result<Vec<crate::subagent::LaneOutcome>> {
    use crate::subagent::LaneOutcome;
    if tasks.is_empty() {
        return Ok(Vec::new());
    }
    // 写租约冲突检查（提前发现，不进入执行）
    crate::subagent::check_write_leases(tasks)?;

    let mut outcomes = Vec::new();
    for (i, t) in tasks.iter().take(max_tasks).enumerate() {
        let sid = crate::subagent::lane_session_id(&parent.session, i, &t.task_name);
        // 建/复用子会话（隔离单元）
        if parent.store.get_session(&sid)?.is_none() {
            parent.store.create_child_session(&sid, &parent.session, &t.task_name)?;
        }
        // 组装子任务的 HarnessCtx：同配置、同工作区，但**换会话**
        let mut sub = build_sub_ctx(parent, &sid);
        // 子任务不带审批回调（继承父的策略；非交互 → 需审批的默认拒绝，安全）
        let input = if t.write_paths.is_empty() {
            t.prompt.clone()
        } else {
            format!(
                "{}\n\n（本子任务声明的写目标：{}）",
                t.prompt,
                t.write_paths.join(", ")
            )
        };
        match run_one(&sub, None, &input) {
            Ok((text, _reasoning)) => outcomes.push(LaneOutcome {
                task_name: t.task_name.clone(),
                session_id: sid,
                ok: true,
                output: text,
            }),
            Err(e) => outcomes.push(LaneOutcome {
                task_name: t.task_name.clone(),
                session_id: sid,
                ok: false,
                output: format!("执行失败：{e}"),
            }),
        }
        // sub 借用结束
        let _ = &mut sub;
    }
    Ok(outcomes)
}

/// 从父 ctx 派生一个"换会话"的子 ctx（其余配置继承）。
fn build_sub_ctx<'a>(parent: &HarnessCtx<'a>, session: &str) -> HarnessCtx<'a> {
    HarnessCtx {
        store: parent.store,
        session: session.to_string(),
        cfg: parent.cfg,
        llm: parent.llm.clone(),
        work_root: parent.work_root.clone(),
        prompt_dir: parent.prompt_dir.clone(),
        enabled: parent.enabled.clone(),
        max_tool_calls_per_turn: parent.max_tool_calls_per_turn,
        max_rounds: parent.max_rounds,
        max_consecutive_failures: parent.max_consecutive_failures,
        pinned_section: parent.pinned_section.clone(),
        mcp_servers: parent.mcp_servers.clone(),
        approval_mode: parent.approval_mode,
        plan_only: parent.plan_only,
        approver: None, // 子任务不带交互回调（非交互 → 需审批默认拒绝）
        system_override: parent.system_override.clone(),
        vault_root: parent.vault_root.clone(),
        asker: None, // 子任务无交互 → ask_user 会明确报错并交回主智能体
        // 【2026-10-18】注入**随父 ctx 继承**。
        // 注意与 `approver` 的区别：审批回调刻意不继承（子任务不该弹审批），
        // 但传输层必须继承——否则测试注入到父 ctx 却传不到子 Lane，
        // m8_tools_e2e 里 subagents 工具链就永远跑不通。
        // 生产路径父 ctx 恒为 None，所以继承 None 仍是受锁的。
        llm_transport: parent.llm_transport.clone(),
    }
}

#[cfg(test)]
mod sanitize_tests {
    use super::sanitize_story_text;

    #[test]
    fn strips_trailing_selfcheck_block() {
        let raw = "他喝完了粥。\n\n---\n\n**写完自查**：全文约 2900 字，无「锚点已建立」。";
        assert_eq!(sanitize_story_text(raw), "他喝完了粥。\n");
    }

    #[test]
    fn strips_technique_plan_block() {
        let raw = "正文结束。\n\n【技法计划】用 R4 节奏。";
        assert_eq!(sanitize_story_text(raw), "正文结束。\n");
    }

    #[test]
    fn keeps_clean_text_intact() {
        let raw = "他喝完了粥，想着明天还得进山。\n";
        assert_eq!(sanitize_story_text(raw), raw);
    }

    #[test]
    fn trims_trailing_whitespace_only() {
        assert_eq!(sanitize_story_text("正文。   \n\n"), "正文。\n");
    }

    #[test]
    fn strips_canon_text_markers_keeps_inner_text() {
        // 标记独占行：剥离标记、保留定版文本
        let raw = "他喉头动了动，说滑倒了，摔了一跤。[[CANON_TEXT_START]]\n滑倒了，摔了一跤。\n[[CANON_TEXT_END]]\n\n话出口他就觉得欠妥。";
        let out = sanitize_story_text(raw);
        assert!(!out.contains("CANON_TEXT"), "{out}");
        assert!(out.contains("滑倒了，摔了一跤。"), "{out}");
        assert!(out.contains("话出口他就觉得欠妥。"), "{out}");
    }
}
