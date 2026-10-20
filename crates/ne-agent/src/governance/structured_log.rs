//! structured_log.rs —— 结构化日志子系统（候选1 演练 · 抄 Op2 AppLogger 两件套）
//!
//! **只写 stderr / 显式日志文件**，绝不污染 stdout 的 `--json`。
//!
//! # 模式
//! - `plain`（默认，向后兼容）：人类可读单行。
//! - `structured`：每行一条 JSON 对象，字段固定 `ts/level/module/msg/attrs`。
//!
//! # 本次改造抄了 Op2 什么
//! 参照：`core/crates/foundation/util/src/AppLogger.rs`
//!
//! 1. **fd 分流**（`AppLogger.rs:456-464`）
//!    Op2 原语义：`ERROR | ASSERT → stderr`，其余 → `stdout`。
//!    见 [`op2_level_fd`]（保留原语义，便于对照）与 [`level_fd`]（我方最终口径）。
//!
//! 2. **批写 + Drop flush**（`AppLogger.rs:79-100`）
//!    Op2 用 `ThreadLocal` 攒写、`Drop` 里倒出（「including during error
//!    unwinding」= panic 展开也刷）。见 [`LogFileWriteBatch`] / [`batch_file_writes`]。
//!
//! # 与 Op2 的差异（翻译说明，非照抄 —— 两条都是**故意不抄**）
//! - ❌ **info 不写 stdout**。Op2 的 `info → stdout` 会撞我方铁则：
//!   `--json` 模式下 stdout 是**机器消费的 JSON**，掺日志行会让 `jq` 直接解析失败
//!   （这是 `structured_log.rs` 文件头从第一版就写死的铁则）。故 [`level_fd`]
//!   默认把 info/debug 也压到 stderr；确有需要时用 [`set_stdout_for_info`] 显式开启。
//! - ❌ **不抄 `operit_host_api` 的 host sink 抽象**。那是 Op2 宿主桥（Flutter/Android
//!   侧收日志），我方是单二进制 CLI，没有宿主。直接写 fd 更简单可靠。
//! - ✅ **额外补了 Op2 没有的东西**：文件句柄复用（Op2 每条 `writeFile` 重开）。
//!   改造前我方每条日志 `OpenOptions::open()` + drop 一次；现在句柄缓存在
//!   [`SINK`] 里复用，路径变了才重开。
//!
//! # 铁则
//! - ❌ 不往外上传任何日志；只输出 stderr 或**显式指定**的本地文件（不自动写盘）。
//! - ✅ `--quiet` 同时抑制日志与进度。
//! - ✅ `PROGRESS` 进度行**不属于**本子系统（独立走 stderr），互不干扰。
//! - ✅ 日志自身任何失败都**静默吞掉**：写日志不能反过来把程序弄崩。

use serde_json::{json, Value};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// 日志模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogMode {
    Plain,
    Structured,
}

impl LogMode {
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        match s {
            "plain" => Ok(LogMode::Plain),
            "structured" => Ok(LogMode::Structured),
            other => anyhow::bail!("未知日志模式 '{other}'（仅支持 plain | structured）"),
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            LogMode::Plain => "plain",
            LogMode::Structured => "structured",
        }
    }
}

/// 日志落点（文件描述符级）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFd {
    Stdout,
    Stderr,
}

/// **Op2 原语义**：`ERROR | ASSERT → stderr`，其余 → `stdout`。
///
/// 保留这个纯函数是为了对照与单测——**我方默认不走它**（见 [`level_fd`]）。
pub fn op2_level_fd(level: &str) -> LogFd {
    match level {
        "error" | "assert" => LogFd::Stderr,
        _ => LogFd::Stdout,
    }
}

/// info/debug 是否放行到 stdout（默认 **false**，守 `--json` 铁则）。
static STDOUT_INFO: AtomicBool = AtomicBool::new(false);

/// 显式允许 info/debug 写 stdout。
///
/// 只有确认该进程 stdout **不承载机器读取内容**（例如纯人看的交互命令）时才开启；
/// 带 `--json` 的命令**绝不能**开。
pub fn set_stdout_for_info(enabled: bool) {
    STDOUT_INFO.store(enabled, Ordering::Relaxed);
}

/// **我方最终分流口径**。
///
/// = Op2 语义 + 一条覆盖：`--json` 铁则优先，info 默认也进 stderr。
pub fn level_fd(level: &str) -> LogFd {
    match op2_level_fd(level) {
        LogFd::Stdout if !STDOUT_INFO.load(Ordering::Relaxed) => LogFd::Stderr,
        fd => fd,
    }
}

// ══════════════════════ 配置 ══════════════════════

#[derive(Clone)]
struct LogConfig {
    mode: LogMode,
    quiet: bool,
    file: Option<PathBuf>,
}

static CFG: Mutex<Option<LogConfig>> = Mutex::new(None);

/// 初始化日志配置（进程级；调用一次）。
pub fn init(mode: LogMode, quiet: bool, file: Option<PathBuf>) {
    let mut g = CFG.lock().unwrap_or_else(|e| e.into_inner());
    *g = Some(LogConfig { mode, quiet, file });
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// 纯函数：把一条日志格式化为一行（便于单测，不涉 IO）。
pub fn format_line(mode: LogMode, ts: u64, level: &str, module: &str, msg: &str, attrs: &Value) -> String {
    match mode {
        LogMode::Structured => {
            let obj = json!({
                "ts": ts, "level": level, "module": module, "msg": msg, "attrs": attrs,
            });
            obj.to_string()
        }
        LogMode::Plain => {
            let a = if attrs.as_object().map(|o| o.is_empty()).unwrap_or(true) {
                String::new()
            } else {
                format!(" {attrs}")
            };
            format!("[{level}] {module}: {msg}{a}")
        }
    }
}

// ══════════════════════ 文件落盘（句柄复用 + 立即 flush）══════════════════════

struct FileSink {
    path: PathBuf,
    file: std::fs::File,
}

/// 进程级文件句柄缓存。
///
/// 改造前：每条日志 `OpenOptions::open()` → `write` → drop，反复 open/close。
/// 改造后：句柄只开一次，路径变化才重开；每条写完 `flush`，
/// 保证**崩溃/断电前最后一条也已离开用户态**（`File` 无 BufWriter，`flush`
/// 对 File 是 no-op，但显式写出意图，日后若换 BufWriter 语义不丢）。
static SINK: Mutex<Option<FileSink>> = Mutex::new(None);

/// 追加一行到文件。任何 IO 失败都静默放弃（日志不能弄崩程序）。
fn append_line(path: &Path, line: &str) {
    let mut g = SINK.lock().unwrap_or_else(|e| e.into_inner());
    let need_open = match g.as_ref() {
        Some(s) => s.path != path,
        None => true,
    };
    if need_open {
        match OpenOptions::new().create(true).append(true).open(path) {
            Ok(f) => {
                *g = Some(FileSink { path: path.to_path_buf(), file: f });
            }
            Err(_) => return,
        }
    }
    if let Some(s) = g.as_mut() {
        let _ = writeln!(s.file, "{line}");
        let _ = s.file.flush();
    }
}

// ══════════════════════ 批写 + Drop flush（抄 Op2 LogFileWriteBatch）══════════════════════

#[derive(Default)]
struct BatchState {
    depth: usize,
    pending: Vec<(PathBuf, String)>,
}

thread_local! {
    static BATCH: std::cell::RefCell<BatchState> = std::cell::RefCell::new(BatchState::default());
}

/// 日志批写作用域守卫。
///
/// 抄 Op2 `LogFileWriteBatch`（`AppLogger.rs:79-100`）：作用域内只攒不写，
/// **`Drop` 时统一倒出**——包括 `panic` 展开路径（`Drop` 一定跑）。
///
/// # 用法
/// ```ignore
/// {
///     let _b = batch_file_writes();   // depth += 1
///     for i in 0..1000 { log("info", "m", &format!("line {i}"), json!({})); }
/// }   // ← _b drop，1000 行一次写盘
/// ```
///
/// 注意：**只有文件模式受益**；stderr 模式不缓冲（终端要即时看到）。
pub struct LogFileWriteBatch {
    /// 让它 !Send：ThreadLocal 深度必须同线程配对反映（抄 Op2 的 `PhantomData<Rc<()>>`）。
    _not_send: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl Drop for LogFileWriteBatch {
    fn drop(&mut self) {
        let lines = BATCH.with(|b| {
            let mut b = b.borrow_mut();
            b.depth = b.depth.saturating_sub(1);
            if b.depth == 0 {
                std::mem::take(&mut b.pending)
            } else {
                Vec::new()
            }
        });
        // 出 borrow 后再落盘，避免 thread_local 借用与 SINK 锁嵌套
        for (p, l) in lines {
            append_line(&p, &l);
        }
    }
}

/// 开始一个文件批写作用域（`Drop` 时 flush）。
pub fn batch_file_writes() -> LogFileWriteBatch {
    BATCH.with(|b| b.borrow_mut().depth += 1);
    LogFileWriteBatch { _not_send: std::marker::PhantomData }
}

/// 当前线程是否在批写作用域内（调试/测试用）。
pub fn in_batch() -> bool {
    BATCH.with(|b| b.borrow().depth > 0)
}

// ══════════════════════ 主写入 ══════════════════════

/// 记录一条日志（debug/info/warn/error）。`attrs` 建议传对象。
pub fn log(level: &str, module: &str, msg: &str, attrs: Value) {
    let (mode, quiet, file) = {
        let g = CFG.lock().unwrap_or_else(|e| e.into_inner());
        match &*g {
            Some(c) => (c.mode, c.quiet, c.file.clone()),
            None => (LogMode::Plain, false, None), // 未初始化 → 默认 plain 且不静默
        }
    };
    if quiet {
        return;
    }
    let line = format_line(mode, now_ms(), level, module, msg, &attrs);

    match file {
        Some(p) => {
            // 批写作用域内只入队；否则立即落盘
            let queued = BATCH.with(|b| {
                let mut b = b.borrow_mut();
                if b.depth > 0 {
                    b.pending.push((p.clone(), line.clone()));
                    true
                } else {
                    false
                }
            });
            if !queued {
                append_line(&p, &line);
            }
        }
        None => {
            // fd 分流（Op2 语义 + 我方 --json 护栏）
            let out = format!("{line}\n");
            match level_fd(level) {
                LogFd::Stderr => {
                    let _ = std::io::stderr().write_all(out.as_bytes());
                }
                LogFd::Stdout => {
                    let _ = std::io::stdout().write_all(out.as_bytes());
                }
            }
        }
    }
}

/// 便捷宏式封装。
pub fn info(module: &str, msg: &str, attrs: Value) { log("info", module, msg, attrs); }
pub fn warn(module: &str, msg: &str, attrs: Value) { log("warn", module, msg, attrs); }
pub fn error(module: &str, msg: &str, attrs: Value) { log("error", module, msg, attrs); }

/// 当前是否为结构化模式（供调用方决定是否附加额外信息）。
pub fn is_structured() -> bool {
    let g = CFG.lock().unwrap_or_else(|e| e.into_inner());
    matches!(g.as_ref().map(|c| c.mode), Some(LogMode::Structured))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试间共享静态（CFG/SINK/BATCH），文件类测试用唯一路径 + 本锁串行。
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn tmp_log(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "wg_log_{}_{}_{}.log",
            tag,
            std::process::id(),
            now_ms()
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn plain_format_is_human_readable() {
        let l = format_line(LogMode::Plain, 1, "info", "scan", "started", &json!({}));
        assert_eq!(l, "[info] scan: started");
        let l2 = format_line(LogMode::Plain, 1, "warn", "scan", "x", &json!({"a":1}));
        assert!(l2.contains("[warn] scan: x"));
        assert!(l2.contains("\"a\":1"));
    }

    #[test]
    fn structured_format_is_valid_json_with_fixed_fields() {
        let l = format_line(
            LogMode::Structured, 1700000000123, "info", "scan-library", "scan.started",
            &json!({"slice":"1/4"}),
        );
        let v: Value = serde_json::from_str(&l).expect("必须是合法 JSON");
        for k in ["ts", "level", "module", "msg", "attrs"] {
            assert!(v.get(k).is_some(), "缺字段 {k}");
        }
        assert_eq!(v["ts"], 1700000000123u64);
        assert_eq!(v["level"], "info");
        assert_eq!(v["attrs"]["slice"], "1/4");
        // 单行：不含换行
        assert!(!l.contains('\n'));
    }

    #[test]
    fn log_mode_parse() {
        assert_eq!(LogMode::parse("plain").unwrap(), LogMode::Plain);
        assert_eq!(LogMode::parse("structured").unwrap(), LogMode::Structured);
        assert!(LogMode::parse("xml").is_err());
    }

    #[test]
    fn quiet_suppresses_and_file_mode_does_not_touch_stdout() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let p = tmp_log("quiet");
        init(LogMode::Structured, false, Some(p.clone()));
        log("info", "m", "hello", json!({"k":1}));
        let content = std::fs::read_to_string(&p).unwrap();
        assert!(content.contains("\"msg\":\"hello\""), "应写入文件");
        // quiet：不再写
        init(LogMode::Structured, true, Some(p.clone()));
        let before = std::fs::read_to_string(&p).unwrap();
        log("info", "m", "should_not_appear", json!({}));
        let after = std::fs::read_to_string(&p).unwrap();
        assert_eq!(before, after, "--quiet 必须抑制日志");
        let _ = std::fs::remove_file(&p);
        init(LogMode::Plain, true, None);
    }

    // ── 新增：抄 Op2 的两件套 ─────────────────────────────

    #[test]
    fn op2_semantics_error_to_stderr_others_to_stdout() {
        // 对照 Op2 AppLogger.rs:456-464 原语义
        assert_eq!(op2_level_fd("error"), LogFd::Stderr);
        assert_eq!(op2_level_fd("assert"), LogFd::Stderr);
        assert_eq!(op2_level_fd("info"), LogFd::Stdout);
        assert_eq!(op2_level_fd("warn"), LogFd::Stdout);
        assert_eq!(op2_level_fd("debug"), LogFd::Stdout);
    }

    #[test]
    fn our_level_fd_keeps_info_off_stdout_by_default() {
        // 我方铁则：默认 info 也进 stderr，绝不污染 --json 的 stdout
        set_stdout_for_info(false);
        assert_eq!(level_fd("error"), LogFd::Stderr);
        assert_eq!(level_fd("info"), LogFd::Stderr, "info 绝不能默认进 stdout");
        assert_eq!(level_fd("debug"), LogFd::Stderr);
        // 显式开启后才放行（供纯人看的命令）
        set_stdout_for_info(true);
        assert_eq!(level_fd("info"), LogFd::Stdout);
        assert_eq!(level_fd("error"), LogFd::Stderr, "error 永远 stderr");
        set_stdout_for_info(false);
    }

    #[test]
    fn file_sink_reuses_handle_across_writes() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let p = tmp_log("reuse");
        init(LogMode::Plain, false, Some(p.clone()));
        for i in 0..5 {
            log("info", "m", &format!("line{i}"), json!({}));
        }
        let c = std::fs::read_to_string(&p).unwrap();
        let n = c.lines().filter(|l| l.contains("[info] m: line")).count();
        assert_eq!(n, 5, "5 条都必须落盘（句柄复用不能丢行）");
        let _ = std::fs::remove_file(&p);
        init(LogMode::Plain, true, None);
    }

    #[test]
    fn batch_defers_writes_until_drop() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let p = tmp_log("batch");
        init(LogMode::Plain, false, Some(p.clone()));
        {
            let _b = batch_file_writes();
            assert!(in_batch());
            log("info", "m", "batched1", json!({}));
            log("info", "m", "batched2", json!({}));
            // 作用域内尚未落盘
            let mid = std::fs::read_to_string(&p).unwrap_or_default();
            assert!(!mid.contains("batched1"), "批写作用域内不应立即落盘");
        } // ← Drop 倒出
        assert!(!in_batch());
        let c = std::fs::read_to_string(&p).unwrap();
        assert!(c.contains("batched1"), "Drop 必须 flush");
        assert!(c.contains("batched2"), "Drop 必须 flush 全部");
        let _ = std::fs::remove_file(&p);
        init(LogMode::Plain, true, None);
    }

    #[test]
    fn batch_flushes_even_when_unwinding() {
        // 抄 Op2 的 "including during error unwinding"：panic 展开也要落盘
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let p = tmp_log("unwind");
        init(LogMode::Plain, false, Some(p.clone()));
        let path = p.clone();
        let r = std::panic::catch_unwind(move || {
            let _b = batch_file_writes();
            log("info", "m", "before_panic", json!({}));
            let _ = path;
            panic!("boom");
        });
        assert!(r.is_err(), "应当捕获到 panic");
        let c = std::fs::read_to_string(&p).unwrap();
        assert!(c.contains("before_panic"), "panic 展开路径也必须 flush");
        let _ = std::fs::remove_file(&p);
        init(LogMode::Plain, true, None);
    }

    #[test]
    fn log_never_panics_on_unwritable_path() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // 指向一个不存在的深目录：写日志失败必须静默，不能反过来崩程序
        let p = PathBuf::from("/nonexistent_dir_xyz/deep/a.log");
        init(LogMode::Plain, false, Some(p));
        log("error", "m", "should_not_panic", json!({}));
        init(LogMode::Plain, true, None);
    }
}
