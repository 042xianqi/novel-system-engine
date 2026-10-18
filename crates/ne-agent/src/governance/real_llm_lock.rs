//! 真实 LLM 授权锁（文件锁的面板侧视图 + 操作）—— 2026-10-17
//!
//! # 为什么要有这个模块
//! `cardgen.py` / `l5.py` 的真实出网由一个**文件锁**控制：
//! `/sdcard/wogov_real_llm.allow` 存在 = 解锁，`rm` = 上锁。
//!
//! 这个设计本身是对的（运行时即时生效，比环境变量适合"手动授权"），
//! 但用户体验差：每次都要去终端 `touch` / `rm`。
//! 本模块把文件锁暴露成面板可点的开关，**底层语义一字不改**：
//! 面板点「解锁」= `touch`，点「上锁」/ 任务结束 = `rm`。
//!
//! # 铁则（不可让步）
//! - 这个文件**只能由人显式点开关来创建**；任何任务流程不得自动 `touch` 解锁。
//! - 真跑结束（无论成功失败）**必须自动 `rm` 上锁**——这是 fail-safe 方向。
//! - 解锁/上锁都写 `webui_op_log` 审计（谁、何时、从哪个来源）。
//! - 真实外呼的最终判断仍在 Python 侧（`call_llm()` 二次校验），本模块不构成替代。
use anyhow::Result;
use serde_json::{json, Value};
use std::path::Path;

/// 与 `ne-mcp::tools::ALLOW_FILE` / `cardgen.py::ALLOW` **必须同一路径**。
///
/// 三处硬编码同一字符串是刻意的：它们分属三个 crate/语言，
/// 任一处改动必须同步全部三处，否则会出现"面板说解锁了但脚本仍拒绝"的鬼故事。
/// 测试 `lock_path_matches_expected` 会锁死这个值。
pub const ALLOW_FILE: &str = "/sdcard/wogov_real_llm.allow";

/// 锁的当前状态。
///
/// 返回结构刻意做成前端可直接渲染的扁平形状：
/// - `locked`：true = 已上锁（不能真跑），false = 已解锁
/// - `label` / `color`：前端直接用，避免前端再写一遍判断
pub fn status() -> Value {
    let p = Path::new(ALLOW_FILE);
    let exists = p.exists();
    json!({
        "file": ALLOW_FILE,
        "locked": !exists,
        "unlocked": exists,
        // 方便前端直接绑定（红色=锁、绿色=开）
        "label": if exists { "🔓 真实 LLM 授权锁（当前：已解锁）" } else { "🔒 真实 LLM 授权锁（当前：已锁定）" },
        "color": if exists { "green" } else { "red" },
        "exists": exists,
        "note": if exists {
            "已解锁：真实调 LLM 的写类操作（cardgen / L5 正文）可以外呼。跑完请立刻上锁。"
        } else {
            "已锁定：真跑会被拒绝；预演（real=false）不受影响，可随时跑。"
        },
    })
}

/// 解锁：创建 allow 文件。幂等（已存在则返回已经解锁）。
///
/// `actor` 仅用于审计日志（如 `"webui:unlock-button"` / `"exec_after_run"`）。
pub fn unlock(conn: &rusqlite::Connection, actor: &str) -> Result<Value> {
    let p = Path::new(ALLOW_FILE);
    let was_locked = !p.exists();
    if was_locked {
        // 写一行说明进去（便于事后追溯是谁/何时开的），内容本身不参与判断
        let ts = now_secs();
        let body = format!(
            "unlocked_by={actor}\nunlocked_at={ts}\nnote=由 WebUI 面板解锁；真实外呼结束后应自动 rm\n"
        );
        std::fs::write(p, body.as_bytes())
            .map_err(|e| anyhow::anyhow!("创建授权锁失败（{}）：{e}", ALLOW_FILE))?;
    }
    let _ = super::category_store::log_op(
        conn,
        if was_locked { "llm.unlock" } else { "llm.unlock.idempotent" },
        ALLOW_FILE,
        &format!("actor={actor}"),
        "",
    );
    let mut out = status();
    out["changed"] = json!(was_locked);
    out["ok"] = json!(true);
    out["actor"] = json!(actor);
    out["note"] = json!(if was_locked {
        "已解锁。真实外呼完成后系统会自动上锁（或你可手动点开关）。"
    } else {
        "本来就是解锁状态（幂等，无变化）。"
    });
    Ok(out)
}

/// 上锁：删除 allow 文件。幂等（不存在则返回已锁）。
pub fn lock(conn: &rusqlite::Connection, actor: &str) -> Result<Value> {
    let p = Path::new(ALLOW_FILE);
    let was_unlocked = p.exists();
    if was_unlocked {
        std::fs::remove_file(p)
            .map_err(|e| anyhow::anyhow!("删除授权锁失败（{}）：{e}", ALLOW_FILE))?;
    }
    let _ = super::category_store::log_op(
        conn,
        if was_unlocked { "llm.lock" } else { "llm.lock.idempotent" },
        ALLOW_FILE,
        &format!("actor={actor}"),
        "",
    );
    let mut out = status();
    out["changed"] = json!(was_unlocked);
    out["ok"] = json!(true);
    out["actor"] = json!(actor);
    out["note"] = json!(if was_unlocked {
        "已上锁。真实外呼现在会被拒绝（预演不受影响）。"
    } else {
        "本来就是锁定状态（幂等，无变化）。"
    });
    Ok(out)
}

/// 静默上锁（供任务流程自动收尾用，失败不返回 Err、不刷日志）。
///
/// 刻意的设计：任务收尾阶段即使上锁失败，也**不应该**把整个任务判为失败
/// —— 那不是任务本身的问题。失败仅写 op_log 供排查。
pub fn lock_quietly(conn: &rusqlite::Connection, actor: &str) {
    let p = Path::new(ALLOW_FILE);
    if !p.exists() {
        return;
    }
    match std::fs::remove_file(p) {
        Ok(()) => {
            let _ = super::category_store::log_op(conn, "llm.lock.auto", ALLOW_FILE,
                &format!("actor={actor}"), "");
        }
        Err(e) => {
            let _ = super::category_store::log_op(conn, "llm.lock.auto.failed", ALLOW_FILE,
                &format!("actor={actor} err={e}"), "");
        }
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_path_matches_expected() {
        // 三处硬编码必须一致：本模块 / ne-mcp::tools::ALLOW_FILE / cardgen.py::ALLOW
        assert_eq!(ALLOW_FILE, "/sdcard/wogov_real_llm.allow");
    }

    #[test]
    fn status_shape_without_file() {
        // 真机上 /sdcard 可能不存在，只校验结构不校验值
        let s = status();
        assert!(s.get("locked").is_some());
        assert!(s.get("unlocked").is_some());
        assert!(s.get("file").is_some());
        // locked 与 unlocked 必须互斥
        let l = s["locked"].as_bool().unwrap();
        let u = s["unlocked"].as_bool().unwrap();
        assert_ne!(l, u, "locked 与 unlocked 必须互斥");
    }
}