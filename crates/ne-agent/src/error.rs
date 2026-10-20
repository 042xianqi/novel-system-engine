//! error.rs —— 残响之河统一结构化错误体系（候选1 演练 · 抄 Op2 AIServiceError 分层）
//!
//! # 为什么要这一层
//! 重构前实测：全仓 `pub enum *Error` 只有 1 个（`llm_adapter::BackendError`，
//! 且**没有** `impl std::error::Error`），`thiserror` 装了依赖但 **0 处使用**，
//! 错误全靠 `anyhow::Error` 的字符串承载。后果：
//!   · 调用方无法 `match` 错误种类，只能靠 `err.to_string().contains("超时")` 猜；
//!   · 每种错误该不该重试、该不该降级、该给用户看什么话术，全散落在调用点；
//!   · 日志里只有一句人话，机器无法聚合统计。
//!
//! # 抄了 Op2 什么（`core/crates/provider/services/src/chat/llmprovider/AIService.rs:46-57`）
//! Op2 的做法是：**一个领域枚举 + thiserror derive + 每个变体自带话术**。
//! ```ignore
//! pub enum AiServiceError {
//!     #[error("provider is not implemented: {0}")]
//!     ProviderNotImplemented(String),
//!     #[error("request cancelled")]
//!     RequestCancelled,
//!     #[error("connection failed: {0}")]
//!     ConnectionFailed(String),
//!     #[error("request failed: {0}")]
//!     RequestFailed(String),
//!     #[error("token calculation failed: {0}")]
//!     TokenCalculationFailed(String),
//! }
//! ```
//! 它**刻意不用 `#[from]` 做自动转换**（避免把底层 IO 细节泄进领域层），
//! 也不追求覆盖全部错误 —— 只给「调用方真会分支处理」的边界建模。
//! 本文件照此标准，只建模 **3 个真实边界**，不做大而全的统一错误树。
//!
//! # 与 Op2 的差异（翻译说明，非照抄）
//! 1. Op2 枚举无 `source`，本文件给可重试类加 `#[source]` 保留根因链，
//!    便于日志打 `err.chain()`；Op2 用 `String` 吞掉了根因。
//! 2. 新增 `retryable()` / `user_message()` 两个判定方法 —— 这是我方
//!    `BackendError`（`llm_adapter.rs:57`）已有的资产（可判定是否可重试），
//!    Op2 的枚举没有，保留并升级。
//! 3. 业务类（门禁/卡牌）错误不在此文件：它们属于 C 类我方独有资产，
//!    留在各自 crate，只把「边界」错误收进来。
//!
//! # 铁则
//! - 本文件**不引入 tokio/reqwest**，纯 std + thiserror。
//! - 不做 `From<anyhow::Error>` 的兜底转换：那是把字符串错误重新糊成枚举，
//!   属于假结构化，宁可不做。

use std::fmt;

/// 错误分类（供日志聚合与统计）。**这不是错误本身**，是给机器看的标签。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ErrorClass {
    /// 配置/凭证问题：重试无用，必须人介入。
    Config,
    /// 网络/超时：可退避重试。
    Transport,
    /// 上游协议不符（返回体不是预期 JSON 等）：重试通常无用。
    Protocol,
    /// 本地 IO（文件/DB）：视情况可重试。
    Io,
    /// 业务规则拒绝（门禁不通过、预算超限）：不是故障。
    Business,
    /// 内部不变量被破坏：属于 bug，需要修代码。
    Internal,
    /// 任务被取消：不是故障，不该记 error 级日志。
    Cancelled,
}

impl ErrorClass {
    /// 该类别默认是否值得重试。
    ///
    /// 注意：这是**默认值**，调用方可用 `HarnessError::retryable()` 拿最终判定
    /// （它会照顾 `Protocol` 里少数可重试的子类）。
    pub fn default_retryable(self) -> bool {
        matches!(self, ErrorClass::Transport | ErrorClass::Io)
    }

    /// 该类别默认的日志级别。Cancelled 不该刷 error 日志污染告警。
    pub fn log_level(self) -> &'static str {
        match self {
            ErrorClass::Cancelled | ErrorClass::Business => "info",
            ErrorClass::Config | ErrorClass::Protocol => "warn",
            ErrorClass::Transport | ErrorClass::Io | ErrorClass::Internal => "error",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ErrorClass::Config => "config",
            ErrorClass::Transport => "transport",
            ErrorClass::Protocol => "protocol",
            ErrorClass::Io => "io",
            ErrorClass::Business => "business",
            ErrorClass::Internal => "internal",
            ErrorClass::Cancelled => "cancelled",
        }
    }
}

impl fmt::Display for ErrorClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// ne-agent 边界错误。
///
/// 只建模「调用方真会分支处理」的三类边界；业务错误不进这里。
#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    /// 配置错误：模型未配、vault 缺项、路径非法。
    #[error("配置错误：{msg}")]
    Config {
        msg: String,
        /// 出问题的配置键（如 `model.endpoint`），便于日志聚合定位。
        key: Option<&'static str>,
    },

    /// 上游（LLM Provider）调用失败。
    #[error("上游调用失败（{provider}）：{msg}")]
    Upstream {
        provider: String,
        msg: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    /// 上游返回体不符合协议（非预期 JSON、缺字段）。
    #[error("上游协议错误：{msg}")]
    Protocol { msg: String },

    /// 本地 IO / 数据库。
    #[error("IO 错误（{ctx}）：{source}")]
    Io {
        ctx: &'static str,
        #[source]
        source: std::io::Error,
    },

    /// 任务取消（不是故障）。
    #[error("任务已取消")]
    Cancelled,

    /// 内部不变量被破坏 —— 属于 bug，不应出现在正常路径。
    #[error("内部错误：{msg}")]
    Internal { msg: &'static str },
}

impl HarnessError {
    // ── 便捷构造（避免调用点写一堆花括号）────────────────────────

    pub fn config(msg: impl Into<String>) -> Self {
        HarnessError::Config { msg: msg.into(), key: None }
    }

    pub fn config_key(key: &'static str, msg: impl Into<String>) -> Self {
        HarnessError::Config { msg: msg.into(), key: Some(key) }
    }

    pub fn upstream(provider: impl Into<String>, msg: impl Into<String>) -> Self {
        HarnessError::Upstream { provider: provider.into(), msg: msg.into(), source: None }
    }

    pub fn upstream_with(
        provider: impl Into<String>,
        msg: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        HarnessError::Upstream {
            provider: provider.into(),
            msg: msg.into(),
            source: Some(Box::new(source)),
        }
    }

    pub fn protocol(msg: impl Into<String>) -> Self {
        HarnessError::Protocol { msg: msg.into() }
    }

    pub fn io(ctx: &'static str, source: std::io::Error) -> Self {
        HarnessError::Io { ctx, source }
    }

    pub fn internal(msg: &'static str) -> Self {
        HarnessError::Internal { msg }
    }

    // ── 判定 ──────────────────────────────────────────────

    /// 错误分类（给日志聚合用）。
    pub fn class(&self) -> ErrorClass {
        match self {
            HarnessError::Config { .. } => ErrorClass::Config,
            HarnessError::Upstream { .. } => ErrorClass::Transport,
            HarnessError::Protocol { .. } => ErrorClass::Protocol,
            HarnessError::Io { .. } => ErrorClass::Io,
            HarnessError::Cancelled => ErrorClass::Cancelled,
            HarnessError::Internal { .. } => ErrorClass::Internal,
        }
    }

    /// 该错误是否值得退避重试。
    ///
    /// 保留我方 `BackendError` 的既有能力（`llm_adapter.rs:57` 注释：
    /// 「可判定是否可重试」）—— Op2 的 `AiServiceError` 没有这个维度。
    pub fn retryable(&self) -> bool {
        self.class().default_retryable()
    }

    /// 可直接展示给用户的一句话（**不含**底层路径/密钥等技术细节）。
    ///
    /// 与 `Display` 的区别：`Display` 给日志（可含技术细节），本方法给 UI。
    pub fn user_message(&self) -> &str {
        match self {
            HarnessError::Config { .. } => "模型配置有问题，请到设置里检查",
            HarnessError::Upstream { .. } => "连不上模型服务，请稍后重试",
            HarnessError::Protocol { .. } => "模型返回了看不懂的内容，请重试",
            HarnessError::Io { .. } => "读写本地文件失败",
            HarnessError::Cancelled => "已取消",
            HarnessError::Internal { .. } => "内部错误，请反馈日志",
        }
    }

    /// 是否应记 error 级日志（Cancelled/Business 不算故障）。
    pub fn is_fault(&self) -> bool {
        !matches!(self.class(), ErrorClass::Cancelled | ErrorClass::Business)
    }

    pub fn chain(&self) -> String {
        let mut out = self.to_string();
        let mut src: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(self);
        while let Some(s) = src {
            out.push_str(" ← ");
            out.push_str(&s.to_string());
            src = s.source();
        }
        out
    }
}

/// 把既有 `llm_adapter::BackendError` 桥进结构化体系（**接调用点**，候选1 第二步）。
///
/// # 为什么用 From 而不是直接替换 BackendError
/// `BackendError` 是 llm_adapter 的**对外契约类型**，被 `semantic_pipeline::Outcome`
/// 与 `main.rs` 的 JSON 输出消费（`status`/`kind`/`message` 三个字段是稳定契约）。
/// 直接换掉会改动公开 API 与既有测试。所以这里做**单向桥接**：
///   - 新代码（日志/新边界）统一用 `HarnessError`；
///   - 旧契约保持原样，需要时 `HarnessError::from(backend_err)` 转换即可。
///
/// # 映射规则（对齐 `BackendError` 的既有语义）
/// - `Unavailable` → `Internal`？**不**。它表示"默认构建没有出网能力"，是
///   预期状态而非故障，所以映射成 `Config`（配置/构建问题，重试无用）。
/// - `NotConfigured` → `Config`
/// - `Protocol` → `Protocol`
/// - `Transport` → `Upstream`（可退避重试）
impl From<crate::governance::llm_adapter::BackendError> for HarnessError {
    fn from(e: crate::governance::llm_adapter::BackendError) -> Self {
        use crate::governance::llm_adapter::BackendError as B;
        match e {
            B::Unavailable(m) => HarnessError::config(m),
            B::NotConfigured(m) => HarnessError::config(m),
            B::Protocol(m) => HarnessError::protocol(m),
            B::Transport(m) => HarnessError::upstream("llm", m),
        }
    }
}

/// 根因链展平（给日志用）：`a ← b ← c`。
///
/// thiserror 的 `#[source]` 会保留链，但 `Display` 只显示最外层；
/// 本方法把整条链拼成一行，满足"用户提示 + 底层根因同时可见"的要求。
impl From<std::io::Error> for HarnessError {
    /// 无 ctx 的兜底转换。**优先用 `HarnessError::io(ctx, e)`** 带上上下文，
    /// 这里只是为了让 `?` 能在无 ctx 场景编译（ctx 记为 "io"）。
    fn from(source: std::io::Error) -> Self {
        HarnessError::Io { ctx: "io", source }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_mapping_is_total_and_stable() {
        // 每个变体都要落到唯一 class，避免以后加变体漏映射
        assert_eq!(HarnessError::config("x").class(), ErrorClass::Config);
        assert_eq!(HarnessError::upstream("p", "m").class(), ErrorClass::Transport);
        assert_eq!(HarnessError::protocol("m").class(), ErrorClass::Protocol);
        assert_eq!(
            HarnessError::io("ctx", std::io::Error::new(std::io::ErrorKind::Other, "e")).class(),
            ErrorClass::Io
        );
        assert_eq!(HarnessError::Cancelled.class(), ErrorClass::Cancelled);
        assert_eq!(HarnessError::internal("i").class(), ErrorClass::Internal);
    }

    #[test]
    fn retryable_follows_class_default() {
        assert!(HarnessError::upstream("p", "超时").retryable());
        assert!(
            HarnessError::io("f", std::io::Error::new(std::io::ErrorKind::Other, "e")).retryable()
        );
        assert!(!HarnessError::config("缺 key").retryable());
        assert!(!HarnessError::protocol("坏 JSON").retryable());
        assert!(!HarnessError::Cancelled.retryable());
    }

    #[test]
    fn display_carries_context_but_user_message_does_not() {
        let e = HarnessError::config_key("model.endpoint", "未配置 /root/.vault/x");
        // Display：给日志，含技术细节
        assert!(e.to_string().contains("/root/.vault/x"));
        // user_message：给 UI，绝不泄露路径
        assert!(!e.user_message().contains("/root"));
        assert!(!e.user_message().contains(".vault"));
    }

    #[test]
    fn cancelled_and_business_are_not_faults() {
        assert!(!HarnessError::Cancelled.is_fault());
        assert!(!HarnessError::Cancelled.retryable());
        assert_eq!(HarnessError::Cancelled.class().log_level(), "info");
        // 真故障
        assert!(HarnessError::internal("x").is_fault());
        assert_eq!(HarnessError::internal("x").class().log_level(), "error");
    }

    #[test]
    fn source_chain_is_preserved() {
        // Op2 的 AiServiceError 用 String 吞掉根因；我们用 #[source] 保留
        let root = std::io::Error::new(std::io::ErrorKind::TimedOut, "底层超时");
        let e = HarnessError::upstream_with("deepseek", "请求失败", root);
        let src = std::error::Error::source(&e).expect("必须保留 source");
        assert!(src.to_string().contains("底层超时"));
    }

    #[test]
    fn io_from_is_supported_but_loses_ctx() {
        let e: HarnessError = std::io::Error::new(std::io::ErrorKind::NotFound, "nofile").into();
        assert_eq!(e.class(), ErrorClass::Io);
        assert!(e.to_string().contains("nofile"));
    }
}
