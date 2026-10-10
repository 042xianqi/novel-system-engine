//! ne-engine —— 《残响之河》L0–L5 引擎（Rust 全量实现）。
//!
//! 迁移目标（对齐 `05_管线框架/` 的 Python 实现，共约 4800 行核心）：
//!   ✅ gates  —— 16 门禁判定（原 engine/gate14.py 661 行）
//!   ✅ llm    —— LLM 调用 + key 池轮转 + 429 退避（原 l5.py call_llm）
//!   ✅ http   —— 阻塞式 HTTPS（**替代 omp-rs-7k 的 tokio+reqwest**）
//!   ✅ config —— 门禁阈值配置（原 data/dynamics_config.json 读取）
//!   ✅ ctx    —— 门禁输入（novel.db 只读 + 细纲/正文文件）
//!
//! 依赖纪律：**零 tokio、零 reqwest**。HTTPS 走 rustls + std::net 阻塞实现。
//! 数据纪律：DB 一律只读打开，绝不因质检改写用户资料库。

pub mod budget;
pub mod calib;
pub mod chapter;
pub mod db;
pub mod dynamics;
pub mod emotion;
pub mod ooc_gate;
pub mod config;
pub mod ctx;
pub mod gates;
pub mod gate14;
pub mod http;
pub mod keypool;
pub mod llm;
pub mod loaders;
pub mod ops;
pub mod providers;
pub mod role_samples;
pub mod state_load;
pub mod world_physics;
pub mod pyrandom;

pub use config::Config;
pub use ctx::RepoCtx;
pub use gates::{run_all, GateReport};
pub use llm::{call, LlmParams, Message};
pub use keypool::{lane_key, RateLimiter, MAX_PARALLEL_KEYS};
pub use providers::{build_params, Providers};
