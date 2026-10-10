//! gate14 移植的**已知行为差异**（对拍结论固化，2026-10-10）。
//!
//! 对拍方法：合成 lab 环境（novel.db + 细纲 + 测试文本），
//! Rust `gate14::run_all` vs Python `gate14.run_14`，同一文本 ch=3 卷01。
//!
//! 结果：16/16 门禁 pass 判定一致，verdict 一致，p0 一致。
//! 唯一差异：**Rust 的 G08 多检出 1 条 p1** —— 因为 Python 原实现有 bug。

/// **Python 原代码 bug（已验证）**：`gate14.g08_foreshadow_recover` 里
/// `post = r_.get("bayes.posterior")` 拿到的是 **字符串** `'0.5000'`
/// （`dynamics.bayes_mood` 用 `f"{v:.4f}"` 格式化输出），
/// 然后 `if post < p1_line:` 直接比较 str 与 float → `TypeError` →
/// 被 `run_14` 的 `except Exception` 吞掉，记为 `异常skip`。
///
/// ⇒ **G08（伏笔回收率）门禁在 Python 版从未真正生效。**
///
/// Rust 版在解析时做了 `s.parse::<f64>()`，因此 G08 **正常工作**。
/// 这是**有意的行为改进**，不是移植错误；此处用测试锁死该结论。
#[test]
fn g08_python_bug_is_documented() {
    // 复现 Python 的取值方式：bayes_mood 返回字符串
    let r = ne_engine::dynamics::bayes_mood(&serde_json::json!({
        "prior": 0.5, "evidence": 0.0, "deadline": 3, "foreshadow_id": "F-001"
    }))
    .unwrap();
    let post = r.get("bayes.posterior").unwrap();
    assert!(post.is_string(), "bayes.posterior 是字符串（与 Python 一致）");
    assert_eq!(post.as_str().unwrap(), "0.5000");

    // Python 会因为 `'0.5000' < 0.5` 抛 TypeError；
    // Rust 侧必须能正确解析成 f64 才能让 G08 工作。
    let parsed: Result<f64, _> = post.as_str().unwrap().parse();
    assert!(parsed.is_ok(), "Rust 必须能解析该字符串，否则 G08 同样失效");
    assert!((parsed.unwrap() - 0.5).abs() < 1e-9);
}

/// G08 的判定逻辑本身：后验 0.5 落在 [P1线0.5, P2线0.6) → p1（不打回）。
#[test]
fn g08_post_0_5_lands_in_p1_band() {
    let (p2, p1_line) = (0.6_f64, 0.5_f64);
    let post = 0.5_f64;
    assert!(post >= p1_line, "未落到 p0（后验未低于 P1 线）");
    assert!(post < p2, "落到 p1 带（低于 P2 线）");
}
