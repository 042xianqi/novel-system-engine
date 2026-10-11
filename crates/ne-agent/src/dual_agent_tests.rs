//! `dual_agent` 的单元测试（单独文件，避免主文件超架构门禁 700 行）。

use super::*;

fn step(id: &str, deps: &[&str]) -> PlanStep {
    PlanStep {
        id: id.into(),
        title: format!("T{id}"),
        instruction: format!("do {id}"),
        expected_outcome: String::new(),
        dependencies: deps.iter().map(|s| s.to_string()).collect(),
        status: StepStatus::Pending,
        result_summary: None,
    }
}

#[test]
fn ready_requires_all_deps_completed() {
    let s = step("a", &["x"]);
    let mut done = std::collections::HashSet::new();
    assert!(!s.is_ready(&done));
    done.insert("x".into());
    assert!(s.is_ready(&done));
    let mut running = s.clone();
    running.status = StepStatus::Running;
    assert!(!running.is_ready(&done));
}

#[test]
fn parse_init_plan() {
    let text = r#"```json
{"thought":"拆解","action":"INIT_PLAN","plan":[{"id":"s1","title":"A","instruction":"做A","expectedOutcome":"A好","dependencies":[]},{"id":"s2","title":"B","instruction":"做B","dependencies":["s1"]}]}
```"#;
    match parse_planner_decision(text, &[]) {
        PlannerDecision::InitializePlan { plan, .. } => {
            assert_eq!(plan.len(), 2);
            assert_eq!(plan[1].dependencies, vec!["s1".to_string()]);
        }
        other => panic!("应是 INIT_PLAN：{other:?}"),
    }
}

#[test]
fn parse_finish() {
    let text = r#"{"action":"FINISH","finalReport":"全部搞定","plan":[]}"#;
    match parse_planner_decision(text, &[]) {
        PlannerDecision::Finish { final_report, .. } => assert_eq!(final_report, "全部搞定"),
        other => panic!("应是 FINISH：{other:?}"),
    }
}

#[test]
fn parse_fallback_is_execute_step_not_reset() {
    // 真实模型回自然语言（无 JSON、无完成信号）时，必须**累积**新步骤而非重置计划，
    // 否则每轮重置 step_1 → 永不终止（真机 bug 复现）。
    let existing = vec![step("step_1", &[])];
    let mut existing = existing;
    existing[0].status = StepStatus::Completed;
    let d = parse_planner_decision("完全没有 JSON 的自由文本", &existing);
    match d {
        PlannerDecision::ExecuteStep { step, updated_plan } => {
            assert_eq!(updated_plan.len(), 2, "应保留既有步骤并追加");
            assert_eq!(updated_plan[0].status, StepStatus::Completed, "既有完成态不丢");
            assert_ne!(step.id, "step_1", "兜底 id 必须避开已存在 id");
            assert!(step.instruction.contains("自由文本"));
        }
        other => panic!("应兜底为 ExecuteStep：{other:?}"),
    }
}

#[test]
fn natural_language_completion_detected() {
    // 模型不守 JSON 协议、直接用中文宣告完成 → 必须识别为 FINISH（否则死循环）。
    match parse_planner_decision("所有计划工序已全部完成！", &[]) {
        PlannerDecision::Finish { .. } => {}
        other => panic!("中文完成信号应判 FINISH：{other:?}"),
    }
    match parse_planner_decision("All tasks are completed. Done.", &[]) {
        PlannerDecision::Finish { .. } => {}
        other => panic!("英文完成信号应判 FINISH：{other:?}"),
    }
}

#[test]
fn word_boundary_avoids_false_completion() {
    // "uncompleted"/"unfinished" 不得误命中 → 应走兜底 ExecStep。
    match parse_planner_decision("the task is unfinished, keep going", &[]) {
        PlannerDecision::ExecuteStep { .. } => {}
        other => panic!("未完成措辞不应判 FINISH：{other:?}"),
    }
}

#[test]
fn cascade_marks_dependents_failed() {
    let mut steps = vec![step("a", &[]), step("b", &["a"]), step("c", &["b"])];
    steps[0].status = StepStatus::Failed;
    cascade_failures(&mut steps);
    assert_eq!(steps[1].status, StepStatus::Failed);
    assert_eq!(steps[2].status, StepStatus::Failed);
}

#[test]
fn replan_keeps_completed() {
    let mut steps = vec![step("a", &[]), step("b", &["a"])];
    steps[0].status = StepStatus::Completed;
    steps[0].result_summary = Some("A 完成".into());
    merge_replan(&mut steps, vec![step("a", &[]), step("b", &["a"]), step("c", &["b"])]);
    assert_eq!(steps[0].status, StepStatus::Completed);
    assert_eq!(steps[0].result_summary.as_deref(), Some("A 完成"));
    assert_eq!(steps[1].status, StepStatus::Pending);
    assert_eq!(steps.len(), 3);
}

#[test]
fn execute_step_failed_can_retry() {
    let mut steps = vec![step("a", &[])];
    steps[0].status = StepStatus::Failed;
    apply_execute_step(&mut steps, step("a", &[]), vec![]);
    assert_eq!(steps[0].status, StepStatus::Pending, "FAILED 重下发应重置为 PENDING");
}

#[test]
fn execute_step_completed_not_rerun() {
    let mut steps = vec![step("a", &[])];
    steps[0].status = StepStatus::Completed;
    steps[0].result_summary = Some("已完成".into());
    apply_execute_step(&mut steps, step("a", &[]), vec![]);
    assert_eq!(steps[0].status, StepStatus::Completed, "COMPLETED 不可重跑");
    assert_eq!(steps[0].result_summary.as_deref(), Some("已完成"));
}
