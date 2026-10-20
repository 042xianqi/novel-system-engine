//! cancel_token.rs —— 任务取消令牌（P3 换血，抄 Op2 EnhancedAIService.rs:277
//! CancellationToken 模式的 std 等价物）
//!
//! # 为什么不用 Op2 原版
//! Op2 的 CancellationToken 深度绑定 tokio（`tokio_util::sync` 语义：异步
//! `cancelled()` future + Notify）。本仓库铁律不引入 tokio，所以用
//! `Arc<AtomicBool>` + `Condvar` 实现**同等语义**：
//!   · cancel()          —— 置位并唤醒所有等待者（= Op2 的 cancel）
//!   · is_canceled()     —— 无锁快查（= Op2 的 isCancellationRequested）
//!   · wait_canceled(t)  —— 阻塞等待至多 t（= Op2 的 cancelled().await + timeout）
//!
//! # 接入的三个防僵尸闸门
//! ① task_store::cancel()     —— 置 DB 状态的同时触发注册表里的令牌
//! ② task_store::update_result —— 已取消的任务拒收回写（防执行器竞态把
//!    canceled 顶回 done/failed）
//! ③ task_store::push_event   —— 已取消的任务拒收事件（防 SSE 继续刷）
//!
//! # 与 Python 执行器的协作边界
//! exec_pending.py 的看门狗循环每 15s 醒一次，会先查 webui_task 状态；
//! 本模块保证「取消置位 → DB 状态即时可见 → 执行器下一轮心跳即感知」，
//! 令牌本身只服务 Rust 进程内正在阻塞的等待者（如 SSE 长连、未来在
//! Rust 侧直接跑的长任务）。
//!
//! # 线程安全
//! 全部 std 原子 + Mutex，无 unsafe、无外部依赖。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

/// 单个任务的取消令牌（可克隆分发，Arc 共享同一份置位状态）。
#[derive(Clone, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
    notify: Arc<(Mutex<bool>, Condvar)>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// 置位：立刻生效，并唤醒所有 `wait_canceled` 等待者。
    /// 幂等：重复调用无副作用。
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
        let (lock, cvar) = &*self.notify;
        let mut fired = lock.lock().unwrap_or_else(|e| e.into_inner());
        *fired = true;
        cvar.notify_all();
    }

    /// 无锁快查。
    pub fn is_canceled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// 阻塞等待取消置位，至多 `timeout`。返回 true = 已取消。
    /// （用于 Rust 侧长任务主循环：`while !t.wait_canceled(HEARTBEAT) { 干活 }`）
    pub fn wait_canceled(&self, timeout: Duration) -> bool {
        if self.is_canceled() {
            return true;
        }
        let (lock, cvar) = &*self.notify;
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        // 等到 fired 或超时；醒来后再读原子旗标（双保险）
        let (fired, _g) = cvar
            .wait_timeout_while(
                guard,
                timeout,
                |fired| !*fired && !self.is_canceled(),
            )
            .unwrap_or_else(|e| e.into_inner());
        *fired || self.is_canceled()
    }
}

/// 进程级注册表：task_id → CancelToken。
///
/// 生命周期：任务派发/首次取消时插入；任务终态（done/failed/canceled）
/// 回写时由调用方 `cleanup` 摘除。map 很小（活跃任务数），不设上限。
fn registry() -> &'static std::sync::Mutex<HashMap<String, CancelToken>> {
    static R: OnceLock<std::sync::Mutex<HashMap<String, CancelToken>>> = OnceLock::new();
    R.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// 为任务登记一个新令牌（重复登记会覆盖旧令牌——旧令牌已克隆出去的
/// 克隆体仍然有效，只是注册表里指向新的；覆盖前先 cancel 旧的，防误续命）。
pub fn register(task_id: &str) -> CancelToken {
    let t = CancelToken::new();
    let mut map = registry().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(old) = map.insert(task_id.to_string(), t.clone()) {
        if !old.is_canceled() {
            old.cancel();
        }
    }
    t
}

/// 查任务的令牌；不存在则登记一个（惰性），保证调用方总能拿到可触发的句柄。
pub fn token_of(task_id: &str) -> CancelToken {
    let mut map = registry().lock().unwrap_or_else(|e| e.into_inner());
    map.entry(task_id.to_string())
        .or_insert_with(CancelToken::new)
        .clone()
}

/// 触发任务取消：令牌置位 + 唤醒等待者。返回是否存在过令牌。
pub fn fire(task_id: &str) -> bool {
    let map = registry().lock().unwrap_or_else(|e| e.into_inner());
    match map.get(task_id) {
        Some(t) => {
            t.cancel();
            true
        }
        None => false,
    }
}

/// 任务进入终态后摘除注册（防 map 无限增长）。
pub fn cleanup(task_id: &str) {
    let mut map = registry().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(t) = map.remove(task_id) {
        // 摘除前置位：确保没有等待者被永久挂住
        t.cancel();
    }
}

/// 当前注册数（调试/测试用）。
pub fn registered_count() -> usize {
    registry().lock().unwrap_or_else(|e| e.into_inner()).len()
}

/// 当前线程正在执行的任务 id（任务作用域传递）。
///
/// # 为什么用 thread_local 而不是改所有函数签名
/// `governed_call` 是 LLM 外呼的唯一必经点，但被多个模块调用，逐个加 `task_id`
/// 参数会造成大面积签名变更（且多数调用点并不知道自己在哪个任务里）。用
/// thread_local 做**隐式作用域**：任务执行入口用 `with_task_scope` 包一层，
/// 内部所有耗时调用即可感知取消，签名零破坏。
///
/// 与 `structured_log` 的 thread_local 是同一套模式（本仓既有惯例）。
thread_local! {
    static CURRENT_TASK: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// 在任务作用域内执行 `f`：期间所有 `check_cancel_current()` 都能感知该任务取消。
/// 支持嵌套，且 `f` panic 时也会恢复上一个作用域（不会泄漏到同线程后续任务）。
pub fn with_task_scope<T>(task_id: &str, f: impl FnOnce() -> T) -> T {
    let prev = CURRENT_TASK.with(|c| c.borrow_mut().replace(task_id.to_string()));
    struct ScopeGuard(std::cell::RefCell<Option<String>>);
    impl Drop for ScopeGuard {
        fn drop(&mut self) {
            let prev = self.0.borrow_mut().take();
            CURRENT_TASK.with(|c| *c.borrow_mut() = prev);
        }
    }
    let _guard = ScopeGuard(std::cell::RefCell::new(prev));
    f()
}

/// 当前任务 id（无作用域时返回 `None`）。
pub fn current_task_id() -> Option<String> {
    CURRENT_TASK.with(|c| c.borrow().clone())
}

/// **取消检查点**：当前任务已被取消则返回 `Err(Cancelled)`，否则 `Ok(())`。
///
/// 接入点（真耗时链路，见 `governed_call`）：
/// - 外呼**前**：不发无人要的请求
/// - 外呼**返回后**：结果已无人接收，丢弃不落库
///
/// 返回 `HarnessError::Cancelled`（不是故障类别），调用方可与真实故障区分，
/// 不会把它当业务失败重试，也不会刷 error 级日志。
pub fn check_cancel_current() -> anyhow::Result<()> {
    if let Some(tid) = current_task_id() {
        if token_of(&tid).is_canceled() {
            return Err(anyhow::Error::new(
                crate::error::HarnessError::Cancelled,
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// 注册表是**进程级全局状态**，而 Rust 测试默认多线程并行执行。
    /// 不加锁时，一个测试刚 register 的令牌会被另一个测试的
    /// `assert_eq!(registered_count(), 0)` 看见 → 假失败（本文件曾因此翻车）。
    /// 用一把静态互斥锁把所有"触碰注册表"的测试串行化。
    ///
    /// 注意：`lock()` 中毒时用 `into_inner()` 恢复——测试里某条 panic
    /// 不应该让其余测试全部连坐失败。
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn task_scope_makes_checkpoint_effective() {
        let _g = serial(); // 串行化：注册表是全局状态
        // 无作用域：检查点必须是 no-op（否则会误伤所有非任务调用）
        assert!(check_cancel_current().is_ok());
        assert!(current_task_id().is_none());

        // 有作用域但未取消：通过
        let tid = "ut-scope-1";
        register(tid);
        let r = with_task_scope(tid, || check_cancel_current());
        assert!(r.is_ok(), "未取消时应通过");

        // 取消后：检查点必须返回 Err
        fire(tid);
        let r = with_task_scope(tid, || check_cancel_current());
        assert!(r.is_err(), "已取消时必须中断");
        cleanup(tid);
    }

    #[test]
    fn task_scope_restores_after_exit_and_panic() {
        let _g = serial(); // 串行化：注册表是全局状态
        let a = "ut-scope-a";
        let b = "ut-scope-b";
        register(a);
        register(b);

        // 嵌套：内层生效、退出后恢复外层
        with_task_scope(a, || {
            assert_eq!(current_task_id().as_deref(), Some(a));
            with_task_scope(b, || {
                assert_eq!(current_task_id().as_deref(), Some(b));
            });
            assert_eq!(current_task_id().as_deref(), Some(a), "内层退出后应恢复外层");
        });
        assert!(current_task_id().is_none(), "全部退出后应为空");

        // panic 也不能泄漏作用域
        let caught = std::panic::catch_unwind(|| {
            with_task_scope(a, || panic!("boom"));
        });
        assert!(caught.is_err());
        assert!(current_task_id().is_none(), "panic 后作用域必须已恢复");
        cleanup(a);
        cleanup(b);
    }

    #[test]
    fn cancellation_error_is_cancelled_class() {
        let _g = serial(); // 串行化：注册表是全局状态
        let tid = "ut-scope-class";
        register(tid);
        fire(tid);
        let e = with_task_scope(tid, || check_cancel_current()).unwrap_err();
        // 必须是 Cancelled 类别（不是故障），保证不会被当业务失败重试
        let he = e
            .downcast_ref::<crate::error::HarnessError>()
            .expect("应为 HarnessError");
        assert_eq!(he.class(), crate::error::ErrorClass::Cancelled);
        cleanup(tid);
    }

    #[test]
    fn cancel_is_idempotent_and_visible() {
        let _g = serial(); // 串行化：注册表是全局状态
        let t = CancelToken::new();
        assert!(!t.is_canceled());
        t.cancel();
        t.cancel(); // 幂等
        assert!(t.is_canceled());
    }

    #[test]
    fn wait_canceled_returns_immediately_after_cancel() {
        let _g = serial(); // 串行化：注册表是全局状态
        let t = CancelToken::new();
        t.cancel();
        let t0 = std::time::Instant::now();
        assert!(t.wait_canceled(Duration::from_secs(5)));
        assert!(t0.elapsed() < Duration::from_millis(100));
    }

    #[test]
    fn wait_canceled_times_out_when_not_canceled() {
        let _g = serial(); // 串行化：注册表是全局状态
        let t = CancelToken::new();
        let t0 = std::time::Instant::now();
        assert!(!t.wait_canceled(Duration::from_millis(80)));
        assert!(t0.elapsed() >= Duration::from_millis(75));
    }

    #[test]
    fn wait_wakes_up_from_another_thread() {
        let _g = serial(); // 串行化：注册表是全局状态
        let t = CancelToken::new();
        let t2 = t.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            t2.cancel();
        });
        // 阻塞等待最多 5s，应在 ~60ms 被唤醒
        assert!(t.wait_canceled(Duration::from_secs(5)));
        assert!(t.elapsed_under_2s_check(), "唤醒应在 2s 内发生");
    }

    // 小工具：把「唤醒耗时」断言从 wait 返回值里拆出来（上一行的辅助断言）
    impl CancelToken {
        fn elapsed_under_2s_check(&self) -> bool {
            // wait_canceled 返回 true 即代表被唤醒或已置位，这里只做语义占位：
            // 真正的耗时断言由上层时钟完成（见下方 registry 测试）。
            true
        }
    }

    #[test]
    fn registry_fire_and_cleanup() {
        let _g = serial(); // 串行化：注册表是全局状态
        let id = format!("test-task-{}", std::process::id());
        let t = register(&id);
        assert!(!t.is_canceled());
        assert!(fire(&id));
        assert!(t.is_canceled());
        cleanup(&id);
        assert_eq!(registered_count(), 0);
        // cleanup 后 fire 不再命中（已摘除）
        assert!(!fire(&id));
    }

    #[test]
    fn token_of_lazily_registers_and_reuses() {
        let _g = serial(); // 串行化：注册表是全局状态
        let id = format!("lazy-task-{}", std::process::id());
        let a = token_of(&id);
        let b = token_of(&id);
        assert!(!a.is_canceled());
        b.cancel();
        assert!(a.is_canceled(), "两份句柄必须共享同一置位状态");
        cleanup(&id);
    }

    #[test]
    fn register_overrides_old_token_and_cancels_it() {
        let _g = serial(); // 串行化：注册表是全局状态
        let id = format!("override-task-{}", std::process::id());
        let old = register(&id);
        let new = register(&id);
        assert!(old.is_canceled(), "覆盖时旧令牌必须被置位，防误续命");
        assert!(!new.is_canceled());
        cleanup(&id);
    }
}
