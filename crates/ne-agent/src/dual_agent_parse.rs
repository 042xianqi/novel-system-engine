//! Planner 决策协议解析（对齐太墟 `PlannerProtocolParser`）。
//!
//! 单独成文件：容错逻辑较长，避免 `dual_agent.rs` 超架构门禁（700 行）。

use super::*;

// ─────────────────────────────── 解析（对齐太墟 PlannerProtocolParser）

/// 从 Planner 输出里解析 JSON 决策。
///
/// **容错策略对齐太墟 `PlannerProtocolParser`**：真实模型经常不遵守提示词里的 JSON 协议，
/// 直接回自然语言（真机已复现：完整 system prompt 下模型仍回散文）。因此：
/// 1. 优先解析 JSON（```json 代码块 或裸 JSON）；
/// 2. JSON 解析失败 → **自然语言完成信号识别**（"已完成/全部完成/completed/…"）→ `Finish`；
/// 3. 否则降级为 `ExecuteStep`（**累积**新步骤，不重置计划），id 避开已有步骤防空转。
///
/// `current_steps` 用于：完成信号的 `completed_steps`、兜底步骤 id 防撞车。
pub fn parse_planner_decision(text: &str, current_steps: &[PlanStep]) -> PlannerDecision {
    let json = extract_json_block(text);
    if let Some(v) = json.as_ref().and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok()) {
        let thought = v.get("thought").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let action = v.get("action").and_then(|x| x.as_str()).unwrap_or("").to_uppercase();
        let plan = v.get("plan").and_then(|x| x.as_array()).map(|a| parse_steps(a)).unwrap_or_default();
        let step = v.get("step").and_then(parse_step);
        let final_report =
            v.get("finalReport").and_then(|x| x.as_str()).unwrap_or("").to_string();
        match action.as_str() {
            "FINISH" => {
                return PlannerDecision::Finish { final_report, completed_steps: plan };
            }
            "REPLAN" => {
                return PlannerDecision::Replan { reason: thought, new_steps: plan };
            }
            "EXECUTE_STEP" => {
                if let Some(s) = step {
                    return PlannerDecision::ExecuteStep { step: s, updated_plan: plan };
                }
            }
            // INIT_PLAN 与未知 action 都走末尾的 plan 兜底
            _ => {}
        }
        if !plan.is_empty() {
            return PlannerDecision::InitializePlan { thought, plan };
        }
        // JSON 解析成功但没有可用决策：按太墟 `else` 分支降级为单步执行（累积），
        // 指令用 thought，退化用原文。**不再重置整个计划**（防每轮重置导致空转）。
        let instruction = if thought.trim().is_empty() { text.trim() } else { &thought };
        return fallback_execute_step(current_steps, instruction);
    }

    // JSON 解析失败 → 自然语言启发式（对齐太墟）
    if looks_completed(text) {
        return PlannerDecision::Finish {
            final_report: text.trim().to_string(),
            completed_steps: current_steps.to_vec(),
        };
    }
    fallback_execute_step(current_steps, text.trim())
}

/// 自然语言完成信号识别（对齐太墟 `PlannerProtocolParser`，并补协议关键字）。
///
/// 真实模型经常无视 JSON 协议，直接用散文宣告完成（真机复现：模型回「最终报告（FINISH）」）。
/// 英文用**词边界**匹配，避免 `uncompleted`/`unfinished` 误命中 `completed`/`finished`。
///
/// 误判有安全网：协调器对「步骤未全部完成时的 FINISH」会打回并提示继续，不会静默收工。
fn looks_completed(text: &str) -> bool {
    const ZH: [&str; 5] = ["已完成", "全部完成", "实现完毕", "任务完成", "已闭环"];
    if ZH.iter().any(|k| text.contains(k)) {
        return true;
    }
    // `finish` 是协议自身的关键字（模型常说「最终报告（FINISH）」）——纳入信号。
    const EN: [&str; 5] = ["completed", "finished", "finish", "all done", "all tasks are completed"];
    let lower = text.to_lowercase();
    EN.iter().any(|k| has_word_boundary(&lower, k))
}

/// 判断 `hay`（应已小写）中是否包含以非字母边界包裹的 `needle`。
fn has_word_boundary(hay: &str, needle: &str) -> bool {
    let mut from = 0;
    while let Some(rel) = hay[from..].find(needle) {
        let start = from + rel;
        let end = start + needle.len();
        // 【决策二·E级修复】原式靠 `||` 短路保证切片非空（逻辑正确），但 unwrap
        // 仍是一个 panic 面：一旦条件顺序被调换或改成 && 就会崩。改用 map_or：
        // 取不到字符时返回 true（等价"不是字母"），与原式在不可达情形下语义一致。
        let before_ok = start == 0
            || hay[..start]
                .chars()
                .next_back()
                .map_or(true, |c| !c.is_ascii_alphabetic());
        let after_ok = end == hay.len()
            || hay[end..]
                .chars()
                .next()
                .map_or(true, |c| !c.is_ascii_alphabetic());
        if before_ok && after_ok {
            return true;
        }
        from = end;
    }
    false
}

/// 兜底：把这段文本作为一个**新**步骤追加执行（不重置计划）。
fn fallback_execute_step(current_steps: &[PlanStep], instruction: &str) -> PlannerDecision {
    let id = next_fallback_step_id(current_steps);
    let step = PlanStep {
        id: id.clone(),
        title: "执行下一步".into(),
        instruction: instruction.chars().take(500).collect(),
        expected_outcome: String::new(),
        dependencies: Vec::new(),
        status: StepStatus::Pending,
        result_summary: None,
    };
    let mut plan = current_steps.to_vec();
    plan.push(step.clone());
    PlannerDecision::ExecuteStep { step, updated_plan: plan }
}

/// 生成不与既有步骤冲突的兜底 id（`step_N` 递增直到不撞车）。
fn next_fallback_step_id(current_steps: &[PlanStep]) -> String {
    let existing: std::collections::HashSet<&str> =
        current_steps.iter().map(|s| s.id.as_str()).collect();
    let mut n = current_steps.len() + 1;
    loop {
        let id = format!("step_{n}");
        if !existing.contains(id.as_str()) {
            return id;
        }
        n += 1;
    }
}

fn parse_steps(arr: &[serde_json::Value]) -> Vec<PlanStep> {
    arr.iter().enumerate().filter_map(|(i, v)| parse_step_impl(v, i)).collect()
}

fn parse_step(v: &serde_json::Value) -> Option<PlanStep> {
    parse_step_impl(v, 0)
}

fn parse_step_impl(v: &serde_json::Value, idx: usize) -> Option<PlanStep> {
    let obj = v.as_object()?;
    let id = obj
        .get("id")
        .and_then(|x| x.as_str())
        .map(String::from)
        .unwrap_or_else(|| format!("step_{}", idx + 1));
    let title = obj.get("title").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let instruction = obj
        .get("instruction")
        .or_else(|| obj.get("prompt"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if instruction.trim().is_empty() && title.trim().is_empty() {
        return None;
    }
    let expected_outcome = obj
        .get("expectedOutcome")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let dependencies = obj
        .get("dependencies")
        .and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|d| d.as_str().map(String::from)).collect())
        .unwrap_or_default();
    Some(PlanStep {
        id,
        title,
        instruction,
        expected_outcome,
        dependencies,
        status: StepStatus::Pending,
        result_summary: None,
    })
}

/// 抽取 ```json ... ``` 块；无围栏则尝试 `{...}` 首尾配对。
fn extract_json_block(text: &str) -> Option<String> {
    if let Some(s) = text.find("```json") {
        let rest = &text[s + 7..];
        if let Some(e) = rest.find("```") {
            return Some(rest[..e].trim().to_string());
        }
    }
    if let Some(s) = text.find("```") {
        let rest = &text[s + 3..];
        if let Some(e) = rest.find("```") {
            let inner = rest[..e].trim();
            if inner.starts_with('{') {
                return Some(inner.to_string());
            }
        }
    }
    // 裸 JSON：取首个 { 到最后一个 }
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end > start {
        Some(text[start..=end].to_string())
    } else {
        None
    }
}

