//! 推进引擎：依赖解析、阻塞检测、下一层规划、状态推进。
//!
//! 对齐 rules.md §2（L0→L5 不跳级）与 funnel.yaml `executor`：
//!   * order = [L0, L1, L1x, L2, L4, L3, L5]（L4 在 L3 前）
//!   * skip_done = true（已完成层跳过）
//!   * on_blocked = abort_with_report（缺依赖即停）

use anyhow::Result;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

use crate::model::{Funnel, Layer};
use crate::{funnel, gates};

/// 一层在规划中的状态快照。
#[derive(Debug, Clone, Serialize)]
pub struct LayerPlan {
    pub id: String,
    pub name: String,
    pub state_raw: String,
    pub is_done: bool,
    /// 产物是否真实存在于磁盘
    pub artifact_present: Option<bool>,
    /// 未满足的依赖
    pub missing_deps: Vec<String>,
    /// 是否可执行（依赖齐 + 未 done + 产物无效）
    pub runnable: bool,
    /// 不可执行原因
    pub blocked_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub root: String,
    pub funnel_path: String,
    pub exec_order: Vec<String>,
    pub layers: Vec<LayerPlan>,
    /// 按执行序，下一个应推进的层
    pub next: Option<String>,
    /// 首个阻塞层（依赖缺失）
    pub blocked: Option<String>,
    /// 一致性告警（不阻断，但需人看）
    pub warnings: Vec<String>,
}

/// 规划：基于 funnel.yaml + 磁盘实况，算出每层可执行性与下一个推进目标。
pub fn plan(loaded: &funnel::Loaded) -> Plan {
    let f: &Funnel = &loaded.funnel;
    let order = f.exec_order();
    let mut warnings = Vec::new();

    // order 与 layers 对不上 → 明确告警（不静默）
    let declared: BTreeSet<&str> = f.layers.iter().map(|l| l.id.as_str()).collect();
    for id in &order {
        if !declared.contains(id.as_str()) {
            warnings.push(format!("executor.order 里的 `{id}` 在 layers 中不存在"));
        }
    }
    for id in &declared {
        if !order.iter().any(|o| o == id) {
            warnings.push(format!("layers 里的 `{id}` 未出现在 executor.order（将按声明序兜底）"));
        }
    }

    // 依赖满足性：dep 要么是已 done 的层，要么是外部锚（如 L4_worldview / L4_xichao）
    let done: BTreeSet<String> = f
        .layers
        .iter()
        .filter(|l| l.state().is_done())
        .map(|l| l.id.0.clone())
        .collect();

    let mut plans: Vec<LayerPlan> = Vec::new();
    let mut next: Option<String> = None;
    let mut blocked: Option<String> = None;

    // 按 order 生成（order 未覆盖的层追加在后，保证不丢）
    let mut seq: Vec<String> = order.clone();
    for l in &f.layers {
        if !seq.iter().any(|s| s == l.id.as_str()) {
            seq.push(l.id.0.clone());
        }
    }

    for id in &seq {
        let Some(layer) = f.layer(id) else { continue };
        let state = layer.state();
        let is_done = state.is_done();
        let artifact_present = loaded.artifact_exists(layer);

        let missing: Vec<String> = layer
            .deps
            .iter()
            .filter(|d| !dep_satisfied(d, &done))
            .cloned()
            .collect();

        let mut runnable = false;
        let mut reason: Option<String> = None;
        if is_done {
            reason = Some(format!("已完成（status={}）", state.raw()));
        } else if !missing.is_empty() {
            reason = Some(format!("依赖未满足：{}", missing.join(", ")));
            if blocked.is_none() {
                blocked = Some(id.clone());
            }
        } else if artifact_present == Some(false) {
            // 依赖齐但产物缺失 → 需要重建该层
            runnable = true;
            reason = Some("产物缺失，需重建".into());
        } else {
            runnable = true;
        }

        if runnable && next.is_none() {
            next = Some(id.clone());
        }

        plans.push(LayerPlan {
            id: id.clone(),
            name: layer.name.clone(),
            state_raw: state.raw(),
            is_done,
            artifact_present,
            missing_deps: missing,
            runnable,
            blocked_reason: reason,
        });
    }

    Plan {
        root: loaded.root.display().to_string(),
        funnel_path: loaded.path.display().to_string(),
        exec_order: order,
        layers: plans,
        next,
        blocked,
        warnings,
    }
}

/// 依赖是否满足。
/// 规则：`Lx` 形式 → 查是否为已 done 的层；`Lx_suffix`（如 L4_worldview、L4_xichao）
/// → 视为「L4 下某个子集锚点」，只要 L4 未 done 就不满足（保守，不臆造）。
fn dep_satisfied(dep: &str, done: &BTreeSet<String>) -> bool {
    if done.contains(dep) {
        return true;
    }
    if let Some((base, _sub)) = dep.split_once('_') {
        // L4_worldview / L4_xichao 之类：以基层是否 done 为准
        if done.contains(base) {
            return true;
        }
    }
    false
}

/// 推进：把某层的 status 改为新值（**仅支持 L5 章节区间**，其余层不做自动改写）。
///
/// 理由：funnel.yaml 是「单一事实源」，L0–L4 的 status 由人/上层工具拍板，
/// 程序自动改写会掩盖真实进度。只有 L5 章数是机械推进量，允许自动改。
pub fn advance_l5(root: &std::path::Path, chapter: u32) -> Result<bool> {
    let p = root.join("05_管线框架").join("funnel.yaml");
    funnel::push_l5_chapter(&p, chapter)
}

/// 按 rules.md §5 汇总一次质检结果。
pub fn verdict_of(p0: usize, p1: usize) -> gates::Verdict {
    gates::Verdict::from_counts(p0, p1)
}

/// 层状态统计（便于 CLI 概览）。
#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub total: usize,
    pub done: usize,
    pub by_state: BTreeMap<String, usize>,
}

pub fn summarize(plan: &Plan) -> Summary {
    let mut by_state: BTreeMap<String, usize> = BTreeMap::new();
    let mut done = 0;
    for l in &plan.layers {
        let k = if l.is_done {
            "done".to_string()
        } else if l
            .state_raw
            .starts_with("chapters_")
        {
            "in_progress".to_string()
        } else {
            "pending".to_string()
        };
        *by_state.entry(k).or_insert(0) += 1;
        if l.is_done {
            done += 1;
        }
    }
    Summary { total: plan.layers.len(), done, by_state }
}

/// 辅助：取所有层的 Layer 引用（按 exec order）。
pub fn ordered_layers<'a>(f: &'a Funnel) -> Vec<&'a Layer> {
    f.exec_order()
        .iter()
        .filter_map(|id| f.layer(id))
        .collect()
}
