//! M9 CLI：上下文设置（`context`）、压缩（`compress`）、历史回读（`history`）。
//!
//! 从 `cmds.rs` 再拆一层（架构门禁：单文件 ≤ 700 行）。
//! 这些命令的共同点是"以**投影**为中心"——压缩状态、上下文用量、历史原文，
//! 都是同一份投影数据的不同切面，UI 将来直接消费这里的 JSON 形状。

use anyhow::{Context as _, Result};
use ne_agent::Vault;

use crate::cli::HistoryCmd;

/// 所有命令共用的"打开会话库 + 定会话"。
fn open_and_pick(
    vault: &Vault,
    id: Option<&str>,
) -> Result<(ne_agent::session::SessionStore, String)> {
    if !vault.is_initialized() {
        anyhow::bail!("vault 未初始化：{}（跑 `ne-agent vault init`）", vault.root().display());
    }
    let store = ne_agent::session::SessionStore::open(vault.agent_db())?;
    let sid = match id {
        Some(i) => i.to_string(),
        None => store
            .latest_session()?
            .map(|s| s.id)
            .ok_or_else(|| anyhow::anyhow!("没有会话；先 `chat` 一次或用 --id 指定"))?,
    };
    Ok((store, sid))
}

/// `context`：查看 / 设置上下文预算（写回 runtime.yaml）。
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_context(
    vault: &Vault,
    window: Option<u32>,
    reserve: Option<u32>,
    auto_compact: Option<bool>,
    auto_threshold: Option<u32>,
    keep_recent: Option<usize>,
    dry_run: bool,
    json: bool,
) -> Result<()> {
    let mut cfg = ne_agent::config::RuntimeConfig::load(vault.root())?;
    let mut changed = Vec::new();

    if let Some(v) = window {
        if v < 1024 {
            anyhow::bail!("--window 至少 1024 tokens（当前 {v}）");
        }
        cfg.context.max_prompt_tokens = v;
        changed.push(format!("max_prompt_tokens = {v}"));
    }
    if let Some(v) = reserve {
        cfg.context.reserve_tokens = v;
        changed.push(format!("reserve_tokens = {v}"));
    }
    if let Some(v) = auto_compact {
        cfg.context.auto_compaction = v;
        changed.push(format!("auto_compaction = {v}"));
    }
    if let Some(v) = auto_threshold {
        let v = v.clamp(1, 100);
        cfg.context.auto_threshold_percent = v;
        changed.push(format!("auto_threshold_percent = {v}"));
    }
    if let Some(v) = keep_recent {
        cfg.context.keep_recent_tool_results = v;
        changed.push(format!("keep_recent_tool_results = {v}"));
    }

    // 校验一致性
    let eff = cfg
        .context
        .max_prompt_tokens
        .saturating_sub(cfg.context.reserve_tokens)
        .saturating_sub(ne_agent::context::TOOL_SCHEMA_RESERVE_TOKENS);
    if eff < 2048 {
        anyhow::bail!(
            "扣除 reserve({}) 与工具 schema 预留({}) 后有效窗口仅 {} tokens，过小",
            cfg.context.reserve_tokens,
            ne_agent::context::TOOL_SCHEMA_RESERVE_TOKENS,
            eff
        );
    }

    let wrote = !changed.is_empty() && !dry_run;
    if wrote {
        write_context_yaml(vault, &cfg.context)?;
    }

    let ctx = &cfg.context;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "path": vault.root().join("runtime.yaml"),
                "changed": changed,
                "written": wrote,
                "context": {
                    "maxPromptTokens": ctx.max_prompt_tokens,
                    "reserveTokens": ctx.reserve_tokens,
                    "autoCompaction": ctx.auto_compaction,
                    "autoThresholdPercent": ctx.auto_threshold_percent,
                    "keepRecentToolResults": ctx.keep_recent_tool_results,
                    "foldingLine": folding_line(ctx),
                }
            }))?
        );
    } else {
        println!("上下文预算（runtime.yaml）：");
        println!("  窗口上限      max_prompt_tokens       = {}", ctx.max_prompt_tokens);
        println!("  输出预留      reserve_tokens          = {}", ctx.reserve_tokens);
        println!("  工具预留      (schema 固定)           = {}", ne_agent::context::TOOL_SCHEMA_RESERVE_TOKENS);
        println!("  折叠线        (窗口-预留-工具)        = {}", folding_line(ctx));
        println!("  自动压缩      auto_compaction         = {}", ctx.auto_compaction);
        println!("  自动阈值      auto_threshold_percent  = {}%", ctx.auto_threshold_percent);
        println!("  保留工具结果  keep_recent_tool_results = {}", ctx.keep_recent_tool_results);
        if !changed.is_empty() {
            println!(
                "\n{}：{}",
                if dry_run { "将写入（--dry-run 未落盘）" } else { "已写入" },
                changed.join("；")
            );
        } else {
            println!("\n（未修改）用 --window/--reserve/--auto-compact/--auto-threshold/--keep-recent 设置");
        }
    }
    Ok(())
}

fn folding_line(ctx: &ne_agent::config::ContextSection) -> u32 {
    ctx.max_prompt_tokens
        .saturating_sub(ctx.reserve_tokens)
        .saturating_sub(ne_agent::context::TOOL_SCHEMA_RESERVE_TOKENS)
}

/// 把 context 段写回 `runtime.yaml`。
///
/// **保留注释**：只做"按键逐行替换"——已有键就地改值，缺失键追加到 context 段末尾。
/// 刻意不做整体序列化（那会抹掉用户手写的注释与注释性说明）。
fn write_context_yaml(vault: &Vault, ctx: &ne_agent::config::ContextSection) -> Result<()> {
    let p = vault.root().join("runtime.yaml");
    let text = std::fs::read_to_string(&p)
        .with_context(|| format!("读 runtime.yaml 失败：{}", p.display()))?;

    let updates: [(&str, String); 5] = [
        ("max_prompt_tokens", ctx.max_prompt_tokens.to_string()),
        ("reserve_tokens", ctx.reserve_tokens.to_string()),
        ("auto_compaction", ctx.auto_compaction.to_string()),
        ("auto_threshold_percent", ctx.auto_threshold_percent.to_string()),
        ("keep_recent_tool_results", ctx.keep_recent_tool_results.to_string()),
    ];

    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    // 找 context: 段的范围（到下一条顶层键或文件末）
    let start = lines.iter().position(|l| l.trim_end() == "context:");
    let Some(start) = start else {
        anyhow::bail!("runtime.yaml 没有 context: 段，无法写入（请先 `vault init` 或手动补上）");
    };
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, l)| !l.starts_with(' ') && !l.trim().is_empty() && !l.starts_with('#'))
        .map(|(i, _)| i)
        .unwrap_or(lines.len());

    for (key, val) in &updates {
        let prefix = format!("{key}:");
        let hit = (start + 1..end).find(|&i| lines[i].trim_start().starts_with(&prefix));
        match hit {
            Some(i) => {
                let indent: String =
                    lines[i].chars().take_while(|c| *c == ' ' || *c == '-').collect();
                lines[i] = format!("{indent}{key}: {val}");
            }
            None => lines.insert(end, format!("  {key}: {val}")),
        }
    }

    let mut out = lines.join("\n");
    if text.ends_with('\n') || !out.ends_with('\n') {
        out.push('\n');
    }
    std::fs::write(&p, out).with_context(|| format!("写 runtime.yaml 失败：{}", p.display()))?;
    Ok(())
}

/// `compress`：压缩 / 查看 / 清除。
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_compress(
    vault: &Vault,
    id: Option<&str>,
    mode: &str,
    anchor: Option<&str>,
    mechanical: bool,
    show: bool,
    clear: bool,
    json: bool,
) -> Result<()> {
    let (store, sid) = open_and_pick(vault, id)?;

    // --clear：删除压缩记录（还原原始上下文；不删消息）
    if clear {
        let n = store.clear_compactions(&sid)?;
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "session": sid, "cleared": n,
                }))?
            );
        } else {
            println!("已清除会话 {sid} 的 {n} 条压缩记录（消息未删，上下文回到全量）");
        }
        return Ok(());
    }

    // --show：只看投影，不压缩
    if show {
        let proj = ne_agent::compaction::project(&store, &sid)?;
        if json {
            println!("{}", serde_json::to_string_pretty(&proj_json(&sid, &proj))?);
        } else {
            print!("会话 {sid}：");
            print!("{}", proj.render());
            if let Some(s) = &proj.summary {
                println!("\n当前摘要：\n{s}");
            }
        }
        return Ok(());
    }

    let Ok(m) = ne_agent::compaction::CompressMode::parse(mode).ok_or_else(|| {
        anyhow::anyhow!("mode 必须是 before 或 after（当前 {mode}）")
    }) else {
        unreachable!()
    };
    let anchor = anchor.unwrap_or("");

    // 摘要器：非 --mechanical 且有模型配置 → LLM 摘要，否则机械降级
    let h = if mechanical { None } else { Some(crate::build_runtime(vault)?) };
    let outcome = match &h {
        Some(rt) => {
            let s = ne_agent::compaction::LlmSummarizer { llm: rt.llm.clone() };
            ne_agent::compaction::compact(&store, &sid, m, anchor, Some(&s))?
        }
        None => ne_agent::compaction::compact(&store, &sid, m, anchor, None)?,
    };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "session": sid,
                "mode": m.as_str(),
                "summary": outcome.summary,
                "foldedMessageCount": outcome.folded_message_count,
                "retainedMessageCount": outcome.retained_message_count,
                "estTokensBefore": outcome.est_tokens_before,
                "estTokensAfter": outcome.est_tokens_after,
                "llmSummarized": outcome.llm_summarized,
            }))?
        );
    } else {
        println!("{}", outcome.render(m));
    }
    Ok(())
}

/// `history`：按 id 读原文 / 模糊搜索（含被折叠内容）。
pub(crate) fn run_history(vault: &Vault, cmd: &HistoryCmd, json: bool) -> Result<()> {
    match cmd {
        HistoryCmd::Read { id, session } => {
            let (store, _sid) = open_and_pick(vault, session.as_deref())?;
            match store.message_by_id(*id)? {
                Some(m) => {
                    if json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "id": m.id, "role": m.role,
                                "content": m.content, "createdAt": m.created_at,
                                "parentId": m.parent_id,
                            }))?
                        );
                    } else {
                        println!("【消息 #{}】role={} 时间={}", m.id, m.role, m.created_at);
                        println!("{}", m.content);
                    }
                }
                None => anyhow::bail!("消息 #{id} 不存在"),
            }
            Ok(())
        }
        HistoryCmd::Search { query, session, limit } => {
            let (store, sid) = open_and_pick(vault, session.as_deref())?;
            let hits = store.search_messages(&sid, query, *limit)?;
            if json {
                let v: Vec<serde_json::Value> = hits
                    .iter()
                    .map(|(id, role, snip)| serde_json::json!({"id": id, "role": role, "snippet": snip}))
                    .collect();
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else if hits.is_empty() {
                println!("没有匹配「{query}」的消息");
            } else {
                println!("会话 {sid} 中匹配「{query}」共 {} 条：", hits.len());
                for (id, role, snip) in hits {
                    println!("  #{id} [{role}] {snip}");
                }
            }
            Ok(())
        }
    }
}

/// 投影的 JSON 形状（UI 直接消费）。
fn proj_json(sid: &str, p: &ne_agent::compaction::Projection) -> serde_json::Value {
    serde_json::json!({
        "session": sid,
        "summary": p.summary,
        "foldedCount": p.folded_count,
        "retainedCount": p.retained.len(),
        "estTokens": p.est_tokens,
        "estTokensFull": p.est_tokens_full,
        "retained": p.retained.iter().map(|m| serde_json::json!({
            "id": m.id, "role": m.role, "contentPreview": crate::truncate(&m.content, 120),
        })).collect::<Vec<_>>(),
    })
}
