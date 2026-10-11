//! 子智能体 Lane —— 隔离子会话 + 写租约。
//!
//! 契约对照太墟 `SubagentArgsParser.kt` + `SubagentTaskSpec`（`McpModels.kt:113`）。
//!
//! # 与太墟的关键差异（如实说明，不粉饰）
//!
//! 太墟的 Lane 是**真并发**（多协程 + 物理隔离租约），因为它是 Android 宿主，
//! 面板要同时跑多个子任务。**ne-agent 不做并发**：
//!   * 运行时而 **std 阻塞 IO、单线程**（见架构决策，无 tokio）
//!   * 写小说是**长链顺序任务**，并发收益低、正确性风险高（同一章被两个 Lane 写）
//!
//! 所以这里实现的是**顺序 Lane**：每个子任务开一个**独立子会话**（隔离上下文），
//! 依次执行，收集结果。写租约仍做**串行化校验**（防同一路径被多个子任务声明，
//! 这在顺序模型里是"提前发现冲突"而非"运行时竞争"）。
//!
//! # 隔离靠"独立子会话"
//!
//! 每个子任务在自己的会话里跑（`parent_session_id` 指向父会话），
//! 上下文/记忆召回都按子会话走 —— 父会话不被污染，子任务之间的历史也不串。
//! 这正是太墟 `writePaths` 想表达的意思：**并行子任务不能互相看见对方的半成品**。

use anyhow::{bail, Result};
use serde::Deserialize;

/// 子任务规格（对齐太墟 `SubagentTaskSpec`）。
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(default)]
pub struct SubagentTask {
    /// 任务短名（默认"子任务"）
    #[serde(rename = "taskName", alias = "task_name")]
    pub task_name: String,
    /// profile id/name 覆盖（ne-agent 暂未实现 profile 目录 → 仅记录）
    pub role: String,
    /// 任务指令（**必填**）
    pub prompt: String,
    /// 部门（ne-agent 暂未实现目录匹配 → 仅记录）
    pub department: String,
    /// 简短英文关键词（同上，仅记录）
    #[serde(rename = "agentQuery", alias = "agent_query")]
    pub agent_query: String,
    /// 声明的写目标；空 = 只读并行；`["*"]` = 独占整工作区
    #[serde(rename = "writePaths", alias = "write_paths")]
    pub write_paths: Vec<String>,
    /// 模型覆盖；None/"inherit" = 继承父会话
    pub model: Option<String>,
    /// 续跑目标（对应上次汇报的 task_id）
    #[serde(rename = "task_id", alias = "taskId")]
    pub task_id: Option<String>,
}

/// 默认最大子任务数（太墟 `DEFAULT_MAX_TASKS = 6`）。
pub const DEFAULT_MAX_TASKS: usize = 6;

/// 解析子任务列表（太墟 `SubagentArgsParser.parse` 的等价物）。
///
/// 容错：`subagents` 可以是数组、单个对象，或缺失（此时把整个 args 当一个任务）。
/// 校验：prompt 非空；role 或 (department + agentQuery) 至少一组非空 → 否则丢弃该条。
pub fn parse_subagents(args: &serde_json::Value, max_tasks: usize) -> Vec<SubagentTask> {
    let candidates: Vec<&serde_json::Value> = match args.get("subagents") {
        Some(serde_json::Value::Array(a)) => a.iter().collect(),
        Some(o @ serde_json::Value::Object(_)) => vec![o],
        _ => vec![args],
    };
    let mut out = Vec::new();
    for c in candidates {
        let Ok(mut t) = serde_json::from_value::<SubagentTask>(c.clone()) else {
            continue;
        };
        if t.prompt.trim().is_empty() {
            continue;
        }
        // role 或 (department + agentQuery) 至少一组
        let has_role = !t.role.trim().is_empty();
        let has_catalog = !t.department.trim().is_empty() && !t.agent_query.trim().is_empty();
        if !has_role && !has_catalog {
            continue;
        }
        if t.task_name.trim().is_empty() {
            t.task_name = "子任务".into();
        }
        out.push(t);
        if out.len() >= max_tasks {
            break;
        }
    }
    out
}

/// 写租约冲突检查（太墟 `writePaths` 语义）。
///
/// 返回 `Err` 表示有冲突。规则：
///   * `["*"]`（整工作区独占）**不能与任何其它子任务共存**
///   * 两个子任务不能声明同一（规范化后）路径
///
/// ⚠ 这是**提前发现的配置错误**，不是运行时竞争防护——因为 ne-agent 是顺序执行。
/// 但语义与太墟一致：它想表达的是"这些子任务如果并发会打架"。
pub fn check_write_leases(tasks: &[SubagentTask]) -> Result<()> {
    let wildcard: Vec<&str> = tasks
        .iter()
        .filter(|t| t.write_paths.iter().any(|p| p.trim() == "*"))
        .map(|t| t.task_name.as_str())
        .collect();
    if !wildcard.is_empty() {
        if tasks.len() > 1 {
            bail!(
                "写租约冲突：子任务 {:?} 声明了整工作区独占（\"*\"），不能与其它子任务同时存在",
                wildcard
            );
        }
        return Ok(());
    }
    let mut seen: std::collections::HashMap<String, &str> = std::collections::HashMap::new();
    for t in tasks {
        for p in &t.write_paths {
            let key = normalize_path(p);
            if key.is_empty() {
                continue;
            }
            if let Some(prev) = seen.get(&key) {
                bail!("写租约冲突：路径 `{p}` 被「{prev}」与「{}」同时声明", t.task_name);
            }
            seen.insert(key, t.task_name.as_str());
        }
    }
    Ok(())
}

/// 路径规范化（去尾部 `/`、折叠 `./`；不要求存在）。
fn normalize_path(p: &str) -> String {
    let p = p.trim().trim_start_matches("./");
    p.trim_end_matches('/').to_string()
}

/// 一个子任务的执行结果。
#[derive(Debug, Clone)]
pub struct LaneOutcome {
    pub task_name: String,
    /// 子会话 id（隔离单元）
    pub session_id: String,
    pub ok: bool,
    pub output: String,
}

/// 生成子任务的独立会话 id（父会话 + 序号 + 短名）。
///
/// 太墟用 Lane 名做 task_id 续跑；ne-agent 用「父会话/子名」做可读的稳定 id。
pub fn lane_session_id(parent: &str, idx: usize, task_name: &str) -> String {
    let slug: String = task_name
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .take(24)
        .collect();
    let slug = if slug.is_empty() { "sub".to_string() } else { slug };
    format!("{parent}~lane{idx}-{slug}")
}

/// 汇总服务端渲染（供主模型读）。
pub fn render_outcomes(outcomes: &[LaneOutcome]) -> String {
    if outcomes.is_empty() {
        return "（无子任务）".into();
    }
    let mut s = String::new();
    for (i, o) in outcomes.iter().enumerate() {
        let status = if o.ok { "✓" } else { "✗" };
        s.push_str(&format!(
            "{status} [{}/{}] {}（会话 {}）\n{}\n\n",
            i + 1,
            outcomes.len(),
            o.task_name,
            o.session_id,
            if o.output.trim().is_empty() {
                "（无输出）"
            } else {
                o.output.trim()
            }
        ));
    }
    format!("子任务执行汇总：\n\n{}", s.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_array_of_subagents() {
        let args = json!({
            "subagents": [
                {"taskName": "甲", "role": "dev", "prompt": "做 A"},
                {"taskName": "乙", "department": "eng", "agentQuery": "rust", "prompt": "做 B"}
            ]
        });
        let t = parse_subagents(&args, DEFAULT_MAX_TASKS);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].task_name, "甲");
        assert_eq!(t[1].department, "eng");
    }

    #[test]
    fn parse_single_object() {
        let args = json!({"role": "dev", "prompt": "只做一件"});
        let t = parse_subagents(&args, DEFAULT_MAX_TASKS);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].task_name, "子任务", "缺省名");
    }

    #[test]
    fn drops_entries_without_prompt() {
        let args = json!({"subagents": [
            {"role": "dev", "prompt": ""},
            {"role": "dev", "prompt": "好的"}
        ]});
        let t = parse_subagents(&args, DEFAULT_MAX_TASKS);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].prompt, "好的");
    }

    #[test]
    fn drops_entries_without_role_or_catalog() {
        // 只有 prompt，没有 role 也没有 department+agentQuery → 丢弃
        let args = json!({"subagents": [{"prompt": "没角色"}]});
        assert!(parse_subagents(&args, DEFAULT_MAX_TASKS).is_empty());
        // department 有但 agentQuery 缺 → 仍不行
        let args = json!({"subagents": [{"department": "eng", "prompt": "x"}]});
        assert!(parse_subagents(&args, DEFAULT_MAX_TASKS).is_empty());
    }

    #[test]
    fn respects_max_tasks() {
        let arr: Vec<_> = (0..10)
            .map(|i| json!({"role": "dev", "prompt": format!("p{i}")}))
            .collect();
        let args = json!({"subagents": arr});
        assert_eq!(parse_subagents(&args, 6).len(), 6);
    }

    #[test]
    fn write_lease_wildcard_conflicts() {
        let tasks = vec![
            SubagentTask { task_name: "整库".into(), role: "r".into(), prompt: "p".into(),
                write_paths: vec!["*".into()], ..Default::default() },
            SubagentTask { task_name: "小改".into(), role: "r".into(), prompt: "p".into(),
                write_paths: vec!["a.txt".into()], ..Default::default() },
        ];
        assert!(check_write_leases(&tasks).is_err(), "独占+其它 应冲突");
    }

    #[test]
    fn write_lease_wildcard_alone_ok() {
        let tasks = vec![SubagentTask {
            task_name: "整库".into(), role: "r".into(), prompt: "p".into(),
            write_paths: vec!["*".into()], ..Default::default()
        }];
        assert!(check_write_leases(&tasks).is_ok());
    }

    #[test]
    fn write_lease_same_path_conflicts() {
        let tasks = vec![
            SubagentTask { task_name: "甲".into(), role: "r".into(), prompt: "p".into(),
                write_paths: vec!["src/a.rs".into()], ..Default::default() },
            SubagentTask { task_name: "乙".into(), role: "r".into(), prompt: "p".into(),
                write_paths: vec!["./src/a.rs/".into()], ..Default::default() },
        ];
        assert!(check_write_leases(&tasks).is_err(), "同路径（不同写法）应冲突");
    }

    #[test]
    fn write_lease_disjoint_ok() {
        let tasks = vec![
            SubagentTask { task_name: "甲".into(), role: "r".into(), prompt: "p".into(),
                write_paths: vec!["a.rs".into()], ..Default::default() },
            SubagentTask { task_name: "乙".into(), role: "r".into(), prompt: "p".into(),
                write_paths: vec!["b.rs".into()], ..Default::default() },
        ];
        assert!(check_write_leases(&tasks).is_ok());
    }

    #[test]
    fn read_only_tasks_have_no_lease_conflict() {
        let tasks = vec![
            SubagentTask { task_name: "甲".into(), role: "r".into(), prompt: "p".into(), ..Default::default() },
            SubagentTask { task_name: "乙".into(), role: "r".into(), prompt: "p".into(), ..Default::default() },
        ];
        assert!(check_write_leases(&tasks).is_ok(), "都只读 → 无冲突");
    }

    #[test]
    fn lane_session_ids_are_distinct_and_readable() {
        let a = lane_session_id("S1", 0, "写第三章");
        let b = lane_session_id("S1", 1, "查资料");
        assert_ne!(a, b);
        assert!(a.starts_with("S1~lane0"));
        // 中文名会被过滤 → 仍有 fallback
        assert!(a.len() > "S1~lane0-".len());
    }

    #[test]
    fn render_outcomes_format() {
        let out = vec![
            LaneOutcome { task_name: "甲".into(), session_id: "s1".into(), ok: true, output: "结果 A".into() },
            LaneOutcome { task_name: "乙".into(), session_id: "s2".into(), ok: false, output: "失败".into() },
        ];
        let r = render_outcomes(&out);
        assert!(r.contains("✓"));
        assert!(r.contains("✗"));
        assert!(r.contains("结果 A"));
        assert!(r.contains("1/2"));
    }
}
