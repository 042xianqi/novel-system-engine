//! 任务事件广播（SSE 用）——零依赖实现，不引入 tokio/axum。
//!
//! # 为什么自己做
//! 本模块所在的 web_gateway 是 `std::net::TcpListener` 手写的阻塞 HTTP/1.1，
//! 没有 axum、没有 tokio（Cargo.toml 里明确写了"不引入 reqwest/tokio"）。
//! 所以 broadcast 只能用 `std::sync::mpsc` + `Mutex<Vec<Sender>>` 手工实现。
//!
//! # P1 换血：String → GovEvent 强类型枚举
//! 抄 Operit2 RuntimeEvent.rs 的设计（domain + topic + payload + 时间戳），
//! 贴本仓库语义重写为纯 std 版：
//!   · 事件在广播层是 `GovEvent` 枚举（编译期校验 kind 合法性），
//!     只在「补发历史 / 分发订阅者」两个出口序列化成 SSE 行；
//!   · 对外 SSE 协议 100% 不变（`data: {"type":...,"ts":...,"data":...}`），
//!     前端 TaskBoard.vue 零改动；
//!   · `publish_raw(kind, payload)` 保留旧入参形态（兼容包装），
//!     现有调用方（task_store.rs 两处）零改动。
//!
//! # 关键设计
//! 1. **每个 SSE 连接一个 mpsc::Sender**：连接断开时 receiver 被 drop，
//!    下次 broadcast 发现 `send()` 失败就自动摘除该订阅者（自愈）。
//! 2. **环形缓冲保留最近事件**：新连接进来先补发最近 N 条，
//!    避免"刚打开面板前 3 秒的日志看不到"。缓冲存 `GovEvent`（强类型），
//!    补发时才序列化。
//! 3. **全进程单例**：用 `OnceLock` 存全局 hub，gateway 和 exec 回写都能拿到。

use std::sync::mpsc::{channel, Sender};
use std::sync::{Mutex, OnceLock};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

// ── GovEvent：强类型事件（P1 换血核心）─────────────────────────────

/// 事件域（仿 Op2 RuntimeEventDomain；本系统目前 3 个域，够用不加戏）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GovEventDomain {
    /// 任务生命周期与进度（task_state / card_* / log）
    #[serde(rename = "task")]
    Task,
    /// LLM 原生 token 流（thinking / content 双通道）
    #[serde(rename = "stream")]
    Stream,
    /// 系统级杂项（hello / 未来扩展）
    #[serde(rename = "system")]
    System,
}

/// 事件主题（编译期枚举所有合法 kind）。
/// 新增事件类型时在这里加变体 + `kind()` 映射，编译器保证广播层不会拼错字符串。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GovEventTopic {
    /// 任务状态回写（update_result 广播）
    #[serde(rename = "task_state")]
    TaskState,
    /// 进程日志行（stdout/stderr）
    #[serde(rename = "log")]
    Log,
    /// 模型思考 token（reasoning）
    #[serde(rename = "thinking")]
    Thinking,
    /// 模型正文 token（content）
    #[serde(rename = "content")]
    Content,
    /// 单卡生成开始
    #[serde(rename = "card_start")]
    CardStart,
    /// 单卡生成完成
    #[serde(rename = "card_done")]
    CardDone,
    /// 单卡生成失败
    #[serde(rename = "card_fail")]
    CardFail,
    /// SSE 连接 hello 帧
    #[serde(rename = "hello")]
    Hello,
}

impl GovEventTopic {
    /// 主题 → 线上字符串（与既有 SSE 协议完全一致的 kind 值）
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TaskState => "task_state",
            Self::Log => "log",
            Self::Thinking => "thinking",
            Self::Content => "content",
            Self::CardStart => "card_start",
            Self::CardDone => "card_done",
            Self::CardFail => "card_fail",
            Self::Hello => "hello",
        }
    }

    /// 线上字符串 → 主题；未知 kind 归入 Log（旧行为：字符串直通）。
    pub fn parse(s: &str) -> Self {
        match s {
            "task_state" => Self::TaskState,
            "thinking" => Self::Thinking,
            "content" => Self::Content,
            "card_start" => Self::CardStart,
            "card_done" => Self::CardDone,
            "card_fail" => Self::CardFail,
            "hello" => Self::Hello,
            // 未知/历史 kind（含 "log"）一律归 Log，保证旧行为无损
            _ => Self::Log,
        }
    }
}

/// 强类型治理事件。
/// 序列化形态与旧 String 版逐字段一致：{"type","ts","data"}。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GovEvent {
    /// 事件域
    pub domain: GovEventDomain,
    /// 事件主题（= 线上 type 字段）
    pub topic: GovEventTopic,
    /// 事件负载（自由 JSON，与旧 data 字段一致）
    pub payload: Value,
    /// 发生时间（毫秒，与旧 ts 字段一致）
    pub ts: u64,
}

impl GovEvent {
    /// 构造：按主题自动判定域。
    pub fn new(topic: GovEventTopic, payload: Value) -> Self {
        let domain = match topic {
            GovEventTopic::TaskState
            | GovEventTopic::CardStart
            | GovEventTopic::CardDone
            | GovEventTopic::CardFail => GovEventDomain::Task,
            GovEventTopic::Thinking | GovEventTopic::Content => GovEventDomain::Stream,
            GovEventTopic::Log | GovEventTopic::Hello => GovEventDomain::System,
        };
        Self {
            domain,
            topic,
            payload,
            ts: now_ms(),
        }
    }

    /// 线上 type 字段值（= topic.as_str()）。
    pub fn kind(&self) -> &'static str {
        self.topic.as_str()
    }

    /// 序列化成 SSE 帧行（`data: {...}\n\n`）。
    /// 线上 JSON 与旧 String 版逐字段一致（type/ts/data），前端零感知。
    pub fn to_sse_frame(&self) -> String {
        let wire = json!({
            "type": self.topic.as_str(),
            "ts": self.ts,
            "data": self.payload,
        });
        format!(
            "data: {}\n\n",
            serde_json::to_string(&wire).unwrap_or_else(|_| "{}".into())
        )
    }
}

/// 当前毫秒时间戳。
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ── 广播 Hub（结构不变，缓冲与信道改存 GovEvent）────────────────────

/// 单个 SSE 订阅者
struct Subscriber {
    /// 唯一订阅号，供 unsubscribe 精确摘除
    id: u64,
    tx: Sender<GovEvent>,
}
/// 订阅号自增源（进程内唯一即可，不要求跨重启连续）
static NEXT_SUB_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

struct Hub {
    subs: Mutex<Vec<Subscriber>>,
    /// 最近事件环形缓冲（补发给新连接；存强类型 GovEvent）
    recent: Mutex<Vec<GovEvent>>,
}

const RECENT_CAP: usize = 200;

fn hub() -> &'static Hub {
    static H: OnceLock<Hub> = OnceLock::new();
    H.get_or_init(|| Hub {
        subs: Mutex::new(Vec::new()),
        recent: Mutex::new(Vec::new()),
    })
}

/// 订阅：返回 (receiver, 订阅号)。
/// 新订阅者会立刻收到最近 RECENT_CAP 条历史事件（补发）。
///
/// 【R12 修复】原签名只返回 Receiver，调用方无法注销自己：
///   `publish()` 里虽有 `subs.retain(|s| s.tx.send(...).is_ok())`，
///   但那只在**有新事件发布时**才执行一次。本系统的 SSE 常年无事件
///   （任务看板一天可能只发几条），于是一旦 curl / 浏览器关掉连接，
///   对应订阅者会一直躺在 Vec 里直到下一个事件 —— 实测表现为
///   subscribers 只增不减（2→3→4→5），且每个僵尸订阅都持有一个
///   unbounded channel 缓冲，长期运行会线性吃内存。
///   现在返回唯一 id，SSE 线程退出时调 `unsubscribe(id)` 立刻摘除。
pub fn subscribe() -> (std::sync::mpsc::Receiver<GovEvent>, u64) {
    let (tx, rx) = channel::<GovEvent>();
    let tx_backup = tx.clone(); // 补发历史用（避免二次加锁）
    let id = NEXT_SUB_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    {
        let mut subs = hub().subs.lock().unwrap_or_else(|e| e.into_inner());
        subs.push(Subscriber { id, tx });
    }
    // 补发历史（在阻塞 recv 之前，保证顺序）
    // 【注意】这里不能再去 lock subs —— 会与上面的锁顺序冲突造成死锁。
    // 正确做法：把 tx 也留一份在局部变量里，直接用它补发。
    {
        let recent = hub().recent.lock().unwrap_or_else(|e| e.into_inner());
        for ev in recent.iter() {
            let _ = tx_backup.send(ev.clone());
        }
    }
    (rx, id)
}

/// 注销订阅者（SSE 线程退出时必须调用，否则订阅者泄漏）。
pub fn unsubscribe(id: u64) {
    let mut subs = hub().subs.lock().unwrap_or_else(|e| e.into_inner());
    subs.retain(|s| s.id != id);
}

/// 广播一条强类型事件。所有存活订阅者都会收到；已断开的自动摘除。
pub fn publish_event(ev: GovEvent) {
    // 存环形缓冲（强类型）
    {
        let mut recent = hub().recent.lock().unwrap_or_else(|e| e.into_inner());
        recent.push(ev.clone());
        if recent.len() > RECENT_CAP {
            let drop_n = recent.len() - RECENT_CAP;
            recent.drain(0..drop_n);
        }
    }
    // 分发给所有订阅者，掉线的摘掉
    let mut subs = hub().subs.lock().unwrap_or_else(|e| e.into_inner());
    subs.retain(|s| s.tx.send(ev.clone()).is_ok());
}

/// 【兼容包装】旧入参形态：kind 字符串 + 载荷。
/// 现有调用方（task_store.rs 两处）零改动；新代码请用 publish_event。
/// 未知 kind 归入 Log 主题（旧字符串直通行为的等价归一）。
pub fn publish(kind: &str, payload: Value) {
    publish_event(GovEvent::new(GovEventTopic::parse(kind), payload));
}

/// 当前订阅者数量（调试用）
pub fn subscriber_count() -> usize {
    hub().subs.lock().unwrap_or_else(|e| e.into_inner()).len()
}

// ── 单元测试（P1：GovEvent 序列化兼容性）────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// 线上 JSON 必须与旧 String 版逐字段一致（type/ts/data），前端零感知。
    #[test]
    fn gov_event_wire_format_unchanged() {
        let ev = GovEvent::new(GovEventTopic::TaskState, json!({"id": "t1", "state": "done"}));
        let frame = ev.to_sse_frame();
        assert!(frame.starts_with("data: "));
        assert!(frame.ends_with("\n\n"));
        let v: Value = serde_json::from_str(frame[6..frame.len() - 2].trim()).unwrap();
        assert_eq!(v["type"], "task_state");
        assert!(v["ts"].is_u64());
        assert_eq!(v["data"]["id"], "t1");
        assert_eq!(v["data"]["state"], "done");
        // 强类型字段不外泄到线上协议
        assert!(v.get("domain").is_none());
        assert!(v.get("topic").is_none());
    }

    /// 未知 kind 归 Log：旧行为（字符串直通）的等价归一。
    #[test]
    fn unknown_kind_falls_back_to_log() {
        let ev = GovEvent::new(GovEventTopic::parse("some_future_kind"), json!({}));
        assert_eq!(ev.kind(), "log");
        let frame = ev.to_sse_frame();
        let v: Value = serde_json::from_str(frame[6..frame.len() - 2].trim()).unwrap();
        assert_eq!(v["type"], "log");
    }

    /// 域判定：task / stream / system 三域映射正确。
    #[test]
    fn domain_mapping() {
        assert_eq!(
            GovEvent::new(GovEventTopic::CardStart, json!({})).domain,
            GovEventDomain::Task
        );
        assert_eq!(
            GovEvent::new(GovEventTopic::Thinking, json!({})).domain,
            GovEventDomain::Stream
        );
        assert_eq!(
            GovEvent::new(GovEventTopic::Hello, json!({})).domain,
            GovEventDomain::System
        );
    }

    /// 端到端：publish → subscribe 补发 → 收到强类型事件且线上格式不变。
    #[test]
    fn publish_subscribe_roundtrip() {
        // 清场：本测试自建的订阅者只收自己之后的事件；先发一条建立缓冲
        publish("card_done", json!({"cardId": "CHAR-99-001"}));
        let (rx, id) = subscribe();
        // 补发：最近缓冲里至少含刚才那条 card_done
        let got = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert_eq!(got.kind(), "card_done");
        assert_eq!(got.payload["cardId"], "CHAR-99-001");
        unsubscribe(id);
        // 注销后再发布不应再收到（channel 断开）
        publish("hello", json!({}));
        assert!(rx.recv_timeout(std::time::Duration::from_millis(100)).is_err());
    }
}
