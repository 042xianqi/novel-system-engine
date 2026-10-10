//! ne-funnel —— 《残响之河》L0–L5 六层漏斗框架（Rust）。
//!
//! 事实源：`05_管线框架/funnel.yaml`（单一事实源）+ `rules.md`（门禁铁律）。
//!
//! 职责边界（刻意划分，避免重复实现导致漂移）：
//!   ✅ 本 crate 负责：层模型 / 依赖图 / 执行序 / 产物校验 / 状态推进 / 门禁注册表
//!   ❌ 不负责：门禁判定逻辑（在 Python gate14.py，661 行 + 67 维评分换算）
//!              LLM 调用（在 l5.py 的 7-key 池）
//!              数据库灌库（在 db/*.py）
//!
//! 这样 Rust 侧是「骨架与调度」，Python 侧是「判定与生成」，边界清晰、各自可测。

pub mod engine;
pub mod funnel;
pub mod gates;
pub mod model;

pub use engine::{plan, Plan};
pub use funnel::{find_root, load, Loaded};
pub use gates::{gate, Verdict, GATES};
pub use model::{Funnel, Layer, LayerId, LayerState};
