//! `world-gov trace` 子命令 —— S2 拆分试点（23 臂中第一个被迁出的臂）。
//!
//! # 拆分方法（可复用于其余 22 臂）
//! 1. **同文件提取 `fn`**：先在 `main.rs` 里把臂体抽成函数并让臂改为调用它，
//!    编译通过 → 跑全量测试（884 通过）——此时借用关系已被证明可行；
//! 2. **搬家**：把函数整体移到本文件，`main.rs` 调用点加 `cmds::` 前缀；
//! 3. 再编译 + 全量测试。
//!
//! 两步之间各跑一次测试，是这套流程的安全底线：一旦目标模块有隐藏耦合，
//! 会在第 1 步就暴露，而不是等到搬家后才发现。

use anyhow::Result;
use std::path::Path;

/// `world-gov trace` —— 法理溯源链（S2 试点：从 23 臂 `match` 中提取的第一个臂）。
///
/// # 为什么先拿它开刀
/// 12 行、零 `let mut`、只依赖 `&conn` 与 `key`——是 23 臂里耦合最小的一个。
/// 用它验证「同文件提取 → 编译 → 全量测试 → 搬家」这套闭环本身是否可靠，
/// 确认无风险后再按由易到难的顺序推进其余 22 臂。
///
/// # 借用说明
/// `conn` 以 `&Connection` 传入（与 `match` 前那处共享借用同源），函数内不再
/// 持有任何可变状态，因此提取后借用关系与原臂完全一致。
pub(crate) fn run_wg_trace(
    conn: &rusqlite::Connection,
    lib_root: &Path,
    key: &str,
    json: bool,
) -> Result<()> {
    use ne_agent::world_gov;
    let r = world_gov::trace(conn, lib_root, key)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&r)?);
    } else if r["status"] == "NOT_FOUND" {
        println!("未找到 key={key}");
    } else {
        println!("== 法理溯源链 ==");
        println!("{}", r["visual"].as_str().unwrap_or(""));
        println!("（共 {} 层上层约束；详见 --json）", r["chain_len"]);
    }
    Ok(())
}
