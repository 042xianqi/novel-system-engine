//! L0–L5 推进的依赖图与门禁注册表。
//!
//! 事实源：`05_管线框架/rules.md §5`（门禁表）+ `engine/gate14.py`（16 门禁实现）。
//! 本模块**只描述「门禁叫什么、属于哪层、判什么」，不实现判定**——
//! 判定逻辑在 Python gate14.py（661 行，含 67 维评分换算）与 LLM 解释层，
//! Rust 侧复刻它属于重复实现且必然漂移，故以「注册表 + 调用契约」形式对齐。

use serde::Serialize;

/// 门禁严重级：P0 = 打回重出，P1 = 提示/可修。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    P0,
    P1,
}

/// 一条门禁的元信息。
#[derive(Debug, Clone, Serialize)]
pub struct GateSpec {
    /// 门禁 ID（gate14.py 里的键名）
    pub id: &'static str,
    /// 中文名
    pub name: &'static str,
    /// 确定性 / 需 LLM
    pub kind: GateKind,
    /// 未通过时的严重级
    pub severity: Severity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GateKind {
    /// 纯确定性（0 LLM，可重放）
    Deterministic,
    /// 需 LLM 深读
    Llm,
    /// 混合（确定性子集 + LLM 解释层）
    Hybrid,
}

/// 16 门禁注册表（顺序与 gate14.py 的 G01..G16 一致）。
pub const GATES: &[GateSpec] = &[
    GateSpec { id: "G01_opening4",           name: "开篇四断言",   kind: GateKind::Deterministic, severity: Severity::P0 },
    GateSpec { id: "G02_hook_density",       name: "章末钩密度",   kind: GateKind::Deterministic, severity: Severity::P0 },
    GateSpec { id: "G03_shuang_interval",    name: "爽点间隔",     kind: GateKind::Hybrid,        severity: Severity::P1 },
    GateSpec { id: "G04_ability_cost",       name: "能力代价",     kind: GateKind::Hybrid,        severity: Severity::P1 },
    GateSpec { id: "G05_pov_consistent",     name: "视角一致",     kind: GateKind::Deterministic, severity: Severity::P0 },
    GateSpec { id: "G06_character_consistency", name: "人设一致",  kind: GateKind::Deterministic, severity: Severity::P1 },
    GateSpec { id: "G07_OOC",                name: "OOC 判据",     kind: GateKind::Llm,           severity: Severity::P0 },
    GateSpec { id: "G08_foreshadow_recover", name: "伏笔回收率",   kind: GateKind::Hybrid,        severity: Severity::P0 },
    GateSpec { id: "G09_info_density",       name: "信息密度",     kind: GateKind::Deterministic, severity: Severity::P0 },
    GateSpec { id: "G10_horror_chapter",     name: "恐怖章",       kind: GateKind::Hybrid,        severity: Severity::P1 },
    GateSpec { id: "G11_vol9_reversal",      name: "卷九反转",     kind: GateKind::Hybrid,        severity: Severity::P1 },
    GateSpec { id: "G12_naming_tier",        name: "命名阶位",     kind: GateKind::Deterministic, severity: Severity::P0 },
    GateSpec { id: "G13_scoring_67dim",      name: "67 维评分",    kind: GateKind::Hybrid,        severity: Severity::P1 },
    GateSpec { id: "G14_world_consistency",  name: "世界观一致性", kind: GateKind::Deterministic, severity: Severity::P0 },
    GateSpec { id: "G15_family_consistency", name: "家族一致性",   kind: GateKind::Deterministic, severity: Severity::P0 },
    GateSpec { id: "G16_leak_words",         name: "泄漏词",       kind: GateKind::Deterministic, severity: Severity::P0 },
];

pub fn gate(id: &str) -> Option<&'static GateSpec> {
    GATES.iter().find(|g| g.id == id)
}

/// 汇总判定（对齐 gate14 的 verdict 三态）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Pass,
    Fix,
    Reject,
}

impl Verdict {
    /// 有 P0 → reject；有 P1 → fix；否则 pass。
    pub fn from_counts(p0: usize, p1: usize) -> Self {
        if p0 > 0 {
            Verdict::Reject
        } else if p1 > 0 {
            Verdict::Fix
        } else {
            Verdict::Pass
        }
    }
}
