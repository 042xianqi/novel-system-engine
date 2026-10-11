//! ne-agent —— 《残响之河》Agent Runtime。
//!
//! 目标形态：**一个单二进制智能体**，拥有自己的
//!   * 项目隔离库（vault）
//!   * 模型配置（models.yml + config.yml + runtime.yaml，复用 ne-engine）
//!   * 对话（session 持久化 / 恢复）
//!   * 记忆（pinned 常驻 + recall 按需）
//!   * 工具（enum 总线，静态注册）
//!   * 任务（chat / novel / memory / vault / tool）
//!
//! 现有 ne-* crate 一律不改，只被本 crate 调用。
//!
//! 阶段进度见 `docs/ROADMAP-ne-agent.md`。

pub mod config;
pub mod vault;

pub use config::RuntimeConfig;
pub use vault::Vault;
