//! 标准化进度输出（world-gov）—— **只写 stderr**，绝不污染 `--json` 的 stdout。
//!
//! # 铁则
//! - 进度是**粗估**，不是高精度 ETA。
//! - `--quiet` 时完全静默。
//! - 行格式固定：`PROGRESS: <label> current=X total=Y pct=Z% eta≈Ns`。

/// 构造一行标准进度（**纯函数**，便于单测格式合法性）。
///
/// - `total == 0` 视为 100%（空集合已完成）。
/// - `eta_secs == None` → `eta≈?`（未知，不臆造数字）。
pub fn progress_line(label: &str, current: usize, total: usize, eta_secs: Option<u64>) -> String {
    let pct = if total == 0 { 100 } else { (current.saturating_mul(100)) / total };
    let eta = match eta_secs {
        Some(s) => format!("{s}s"),
        None => "?".to_string(),
    };
    format!("PROGRESS: {label} current={current} total={total} pct={pct}% eta≈{eta}")
}

/// 打印进度到 **stderr**（`quiet=true` 时静默）。
pub fn emit(quiet: bool, label: &str, current: usize, total: usize, eta_secs: Option<u64>) {
    if quiet {
        return;
    }
    eprintln!("{}", progress_line(label, current, total, eta_secs));
}

/// 粗估 ETA：按已处理条数线性外推（不追求精度）。
pub fn rough_eta(elapsed_secs: f64, done: usize, total: usize) -> Option<u64> {
    if done == 0 || total <= done {
        return Some(0);
    }
    let per = elapsed_secs / done as f64;
    let remain = (total - done) as f64 * per;
    if !remain.is_finite() || remain < 0.0 {
        None
    } else {
        Some(remain.round() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_format_is_stable_and_parseable() {
        let l = progress_line("scan-library", 3, 4, Some(12));
        assert_eq!(l, "PROGRESS: scan-library current=3 total=4 pct=75% eta≈12s");
        // 必须含固定前缀与四个字段，便于脚本消费
        assert!(l.starts_with("PROGRESS: "));
        for k in ["current=", "total=", "pct=", "eta≈"] {
            assert!(l.contains(k), "缺字段 {k}");
        }
    }

    #[test]
    fn zero_total_is_100pct_and_unknown_eta() {
        let l = progress_line("x", 0, 0, None);
        assert!(l.contains("pct=100%"), "空集合应 100%");
        assert!(l.contains("eta≈?"), "未知 ETA 用 ?，不臆造");
    }

    #[test]
    fn pct_never_overflows_or_panics() {
        let l = progress_line("x", usize::MAX, 1, None);
        assert!(l.contains("pct="), "极端值不 panic");
    }

    #[test]
    fn rough_eta_monotonic_sane() {
        assert_eq!(rough_eta(10.0, 5, 5), Some(0));
        assert_eq!(rough_eta(0.0, 0, 10), Some(0));
        assert_eq!(rough_eta(10.0, 5, 10), Some(10));
        assert_eq!(rough_eta(10.0, 1, 4), Some(30));
    }
}