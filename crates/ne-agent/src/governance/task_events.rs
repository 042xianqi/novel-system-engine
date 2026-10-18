
//! 任务事件广播（SSE 用）——零依赖实现，不引入 tokio/axum。
//!
//! # 为什么自己做
//! 本模块所在的 web_gateway 是 `std::net::TcpListener` 手写的阻塞 HTTP/1.1，
//! 没有 axum、没有 tokio（Cargo.toml 里明确写了“不引入 reqwest/tokio”）。
//! 所以 broadcast 只能用 `std::sync::mpsc` + `Mutex<Vec<Sender>>` 手工实现。
//!
//! # 关键设计
//! 1. **每个 SSE 连接一个 mpsc::Sender**：连接断开时 receiver 被 drop，
//!    下次 broadcast 发现 `send()` 失败就自动摘除该订阅者（自愈）。
//! 2. **环形缓冲保留最近事件**：新连接进来先补发最近 N 条，
//!    避免“刚打开面板前 3 秒的日志看不到”。
//! 3. **全进程单例**：用 `OnceLock` 存全局 hub，gateway 和 exec 回写都能拿到。

use std::sync::mpsc::{channel, Sender};
use std::sync::{Mutex, OnceLock};
use serde_json::{json, Value};

/// 单个 SSE 订阅者
struct Subscriber {
    tx: Sender<String>,
}

struct Hub {
    subs: Mutex<Vec<Subscriber>>,
    /// 最近事件环形缓冲（补发给新连接）
    recent: Mutex<Vec<String>>,
}

const RECENT_CAP: usize = 200;

fn hub() -> &'static Hub {
    static H: OnceLock<Hub> = OnceLock::new();
    H.get_or_init(|| Hub {
        subs: Mutex::new(Vec::new()),
        recent: Mutex::new(Vec::new()),
    })
}

/// 订阅：返回 receiver，SSE 线程不断 recv 并写 socket。
/// 新订阅者会立刻收到最近 RECENT_CAP 条历史事件（补发）。
pub fn subscribe() -> std::sync::mpsc::Receiver<String> {
    let (tx, rx) = channel::<String>();
    let tx_backup = tx.clone();   // 补发历史用（避免二次加锁）
    {
        let mut subs = hub().subs.lock().unwrap_or_else(|e| e.into_inner());
        subs.push(Subscriber { tx });
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
    rx
}

/// 广播一条事件。所有存活订阅者都会收到；已断开的自动摘除。
pub fn publish(kind: &str, payload: Value) {
    let ev = json!({
        "type": kind,
        "ts": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
        "data": payload,
    });
    // SSE 帧格式：data: <json>\n\n
    let line = format!("data: {}\n\n", serde_json::to_string(&ev).unwrap_or_else(|_| "{}".into()));
    // 存环形缓冲
    {
        let mut recent = hub().recent.lock().unwrap_or_else(|e| e.into_inner());
        recent.push(line.clone());
        if recent.len() > RECENT_CAP {
            let drop_n = recent.len() - RECENT_CAP;
            recent.drain(0..drop_n);
        }
    }
    // 分发给所有订阅者，掉线的摘掉
    let mut subs = hub().subs.lock().unwrap_or_else(|e| e.into_inner());
    subs.retain(|s| s.tx.send(line.clone()).is_ok());
}

/// 当前订阅者数量（调试用）
pub fn subscriber_count() -> usize {
    hub().subs.lock().unwrap_or_else(|e| e.into_inner()).len()
}
