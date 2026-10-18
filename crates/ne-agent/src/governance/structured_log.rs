//! 结构化日志子系统（world-gov）—— **只写 stderr / 显式日志文件**，绝不污染 stdout 的 `--json`。
//!
//! # 模式
//! - `plain`（默认，向后兼容）：人类可读单行。
//! - `structured`：每行一条 JSON 对象，字段固定 `ts/level/module/msg/attrs`。
//!
//! # 铁则
//! - ❌ 不往外上传任何日志；只输出 stderr 或**显式指定**的本地文件（不自动写盘）。
//! - ✅ `--quiet` 同时抑制日志与进度。
//! - ✅ `PROGRESS` 进度行**不属于**本子系统（独立走 stderr），互不干扰。

use serde_json::{json, Value};
use std::path::PathBuf;
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
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
                let _ = writeln!(f, "{line}");
            }
        }
        None => eprintln!("{line}"),
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
        // 用临时文件验证：file 模式只写文件
        let p = std::env::temp_dir().join(format!("wg_log_{}.log", std::process::id()));
        let _ = std::fs::remove_file(&p);
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
        // 复位，避免影响其它测试
        init(LogMode::Plain, true, None);
    }
}