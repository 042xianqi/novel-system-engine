//! 语义管线（world-gov）—— 「探针 → 入队 → (将来喂 LLM) → 结果 → 入库」完整异步管线**占位**。
//!
//! # 边界（铁则）
//! - **纯内存队列**：`SemanticQueue` 是**进程生命周期内存队列**，进程退出即清空；
//!   不写 db、不建持久任务表、不起 daemon、不 fork 子进程。
//! - **不发真实请求**：真实 infer 分支带 `TODO` + `bail!`；本模块**零 reqwest / 零 llm::call**。
//! - 沙盘 `simulate_findings` 仅用于把管线走通，产出**极少样例**（≤2 条），不制造脏风险。
//! - 所有真实外呼点必须经 `governance::get_semantic_gate()` 收口（本模块不例外）。
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Mutex;

// ────────────────────────────────── 状态机

/// 任务状态流转：Pending → Probing → ReadyForInferring → Done
/// （任意阶段可被 Rejected 拒绝）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum JobState {
    /// 刚入队
    Pending,
    /// 正在生成探针包
    Probing,
    /// 探针就绪，等待真实推理（门控放行后才允许推进）
    ReadyForInferring,
    /// 已完成（findings 已产出/入库）
    Done,
    /// 被拒绝（参数非法 / 门控拦截 / 失败）
    Rejected,
}

impl JobState {
    pub fn as_str(&self) -> &'static str {
        match self {
            JobState::Pending => "Pending",
            JobState::Probing => "Probing",
            JobState::ReadyForInferring => "ReadyForInferring",
            JobState::Done => "Done",
            JobState::Rejected => "Rejected",
        }
    }
    /// 合法状态流转（防跳态）。
    pub fn can_transition_to(&self, next: JobState) -> bool {
        use JobState::*;
        matches!(
            (self, next),
            (Pending, Probing) | (Pending, Rejected)
                | (Probing, ReadyForInferring) | (Probing, Rejected)
                | (ReadyForInferring, Done) | (ReadyForInferring, Rejected)
        )
    }
}

/// 任务类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum JobKind {
    ScanLibrary,
    CheckOutline,
    InspectCard,
}

impl JobKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            JobKind::ScanLibrary => "ScanLibrary",
            JobKind::CheckOutline => "CheckOutline",
            JobKind::InspectCard => "InspectCard",
        }
    }
}

/// 时间戳（秒级，避免引入新依赖；与 governance::chrono_like_now 同口径）。
fn now_ts() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// 任务 id（进程内单调 + 时间前缀；不引入 uuid 依赖）。
fn gen_id(seq: u64) -> String {
    format!("job-{:x}-{:04x}", now_ts(), seq & 0xffff)
}

/// **参数派生**任务 id：同 `kind + slice + volume`（+ 可选 key）→ 同 id。
/// 用于跨进程幂等（持久层 UPSERT 命中同一行，不灌水）。
/// 不含时间戳/随机源，因此**可复现**。
pub fn derived_id(kind: JobKind, slice: Option<(u32, u32)>, volume: Option<&str>, key: Option<&str>) -> String {
    let mut s = String::new();
    s.push_str(kind.as_str());
    if let Some((n, m)) = slice {
        s.push_str(&format!("|s{n}/{m}"));
    }
    if let Some(v) = volume {
        s.push_str(&format!("|v{v}"));
    }
    if let Some(k) = key {
        s.push_str(&format!("|k{k}"));
    }
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("job-{:016x}", h)
}

/// 一个语义任务。
#[derive(Debug, Clone, Serialize)]
pub struct SemanticJob {
    pub id: String,
    pub kind: JobKind,
    /// 分片 (n, m)，仅 ScanLibrary 有
    pub slice: Option<(u32, u32)>,
    /// 卷号，仅 CheckOutline 有
    pub volume: Option<String>,
    /// 探针包落盘路径（Probing 完成后填）
    pub probe_path: String,
    /// 创建时间（秒）
    pub created: u64,
    pub state: JobState,
}

// ────────────────────────────────── 极简内存队列

/// 进程生命周期内存队列（非持久化）。
#[derive(Default)]
pub struct SemanticQueue {
    jobs: Vec<SemanticJob>,
    seq: u64,
}

impl SemanticQueue {
    pub fn new() -> Self {
        SemanticQueue { jobs: Vec::new(), seq: 0 }
    }

    /// 入队：`slice` 与 `volume` 互斥校验由调用方负责；此处只做基本登记。
    /// 返回 (job_id, 是否新入队)；**重复入队**（同 kind+slice+volume 且未 Done/Rejected）
    /// 返回既存 id 且 flag=false，避免同一任务刷爆队列。
    pub fn enqueue(
        &mut self,
        kind: JobKind,
        slice: Option<(u32, u32)>,
        volume: Option<String>,
    ) -> (String, bool) {
        if let Some(existing) = self.jobs.iter().find(|j| {
            j.kind == kind
                && j.slice == slice
                && j.volume == volume
                && !matches!(j.state, JobState::Done | JobState::Rejected)
        }) {
            return (existing.id.clone(), false);
        }
        self.seq = self.seq.wrapping_add(1);
        let id = gen_id(self.seq);
        self.jobs.push(SemanticJob {
            id: id.clone(),
            kind,
            slice,
            volume,
            probe_path: String::new(),
            created: now_ts(),
            state: JobState::Pending,
        });
        (id, true)
    }

    pub fn count(&self) -> usize {
        self.jobs.len()
    }

    pub fn peek(&self, id: &str) -> Option<&SemanticJob> {
        self.jobs.iter().find(|j| j.id == id)
    }

    /// 可变取任务（内部流转用）。
    pub fn get_mut(&mut self, id: &str) -> Option<&mut SemanticJob> {
        self.jobs.iter_mut().find(|j| j.id == id)
    }

    /// 尝试取出第一个可推进的任务；`drain_all=true` 时取出全部。
    /// 这里"取出"= 返回其 id 列表（不删除记录，交由状态机推进）。
    pub fn try_drain(&self, drain_all: bool) -> Vec<String> {
        let mut out = Vec::new();
        for j in &self.jobs {
            if matches!(j.state, JobState::Pending | JobState::ReadyForInferring) {
                out.push(j.id.clone());
                if !drain_all {
                    break;
                }
            }
        }
        out
    }

    /// 清空队列（**仅内存**，不碰磁盘库）。
    pub fn clear(&mut self) -> usize {
        let n = self.jobs.len();
        self.jobs.clear();
        n
    }

    pub fn list(&self) -> Vec<&SemanticJob> {
        self.jobs.iter().collect()
    }
}

/// 全局内存队列（进程级单例，`Mutex` 守护）。
static QUEUE: Mutex<Option<SemanticQueue>> = Mutex::new(None);

/// 在闭包里访问全局队列。
pub fn with_queue<R>(f: impl FnOnce(&mut SemanticQueue) -> R) -> R {
    let mut guard = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    let q = guard.get_or_insert_with(SemanticQueue::new);
    f(q)
}

// ────────────────────────────────── 状态推进 + 沙盘模拟

/// 推进状态（带合法性校验）。非法跳态返回 Err。
pub fn advance_state(id: &str, next: JobState) -> Result<JobState, String> {
    with_queue(|q| {
        let job = q.get_mut(id).ok_or_else(|| format!("未知 job id: {id}"))?;
        if !job.state.can_transition_to(next) {
            return Err(format!(
                "非法状态流转：{} → {}",
                job.state.as_str(),
                next.as_str()
            ));
        }
        job.state = next;
        Ok(next)
    })
}

/// 设置探针落盘路径（Probing 阶段产物登记）。
pub fn set_probe_path(id: &str, path: &str) -> Result<(), String> {
    with_queue(|q| {
        let job = q.get_mut(id).ok_or_else(|| format!("未知 job id: {id}"))?;
        job.probe_path = path.to_string();
        Ok(())
    })
}

/// 沙盘模拟 findings —— **仅用于管线走通，无任何真实语义**。
/// 严格只产 1~2 条 OK/UNCERTAIN 样例，绝不制造脏风险。
pub fn simulate_findings(job: &SemanticJob) -> Vec<Value> {
    let key = match job.kind {
        JobKind::ScanLibrary => {
            if let Some((n, m)) = job.slice {
                format!("SLICE-{n}of{m}")
            } else {
                "SLICE-ALL".to_string()
            }
        }
        JobKind::CheckOutline => format!("OUTLINE-{}", job.volume.clone().unwrap_or_default()),
        JobKind::InspectCard => "CARD-SIM".to_string(),
    };
    vec![
        json!({
            "key": key,
            "verdict": "OK",
            "kind": "沙盘样例",
            "law_ref": "",
            "explain": "沙盘模拟：管线走通验证，无真实语义结论。",
        }),
        json!({
            "key": format!("{key}-U"),
            "verdict": "UNCERTAIN",
            "kind": "沙盘样例",
            "law_ref": "",
            "explain": "沙盘模拟：UNCERTAIN 样例（按铁则此条不入库）。",
        }),
    ]
}

/// 处理单个任务（沙盘）：Pending→Probing→ReadyForInferring→(模拟)→Done。
/// 真实推理分支带 TODO + bail!，**不实现**。
pub fn process_simulated(id: &str) -> Result<Vec<Value>, String> {
    // Pending → Probing
    advance_state(id, JobState::Probing)?;
    // Probing → ReadyForInferring
    advance_state(id, JobState::ReadyForInferring)?;
    // 【TODO·放行令后填充】此处应调用 get_semantic_gate() 检查三条件；
    // 未放行→保持 ReadyForInferring（不入库）；放行后→真实推理（未实现）。
    let findings = {
        let q_job = with_queue(|q| q.peek(id).cloned());
        match q_job {
            Some(j) => simulate_findings(&j),
            None => return Err(format!("未知 job id: {id}")),
        }
    };
    // ReadyForInferring → Done
    advance_state(id, JobState::Done)?;
    Ok(findings)
}

/// 沙盘跑通整条入队链路：入队 → 处理 → 产出模拟 findings（供入库路由）。
/// 返回 (job_id, 是否新入队, findings)。
pub fn enqueue_and_simulate(
    kind: JobKind,
    slice: Option<(u32, u32)>,
    volume: Option<String>,
) -> Result<(String, bool, Vec<Value>), String> {
    let (id, fresh) = with_queue(|q| q.enqueue(kind, slice, volume));
    if !fresh {
        // 重复入队：不重跑，返回空 findings（幂等）
        return Ok((id, false, Vec::new()));
    }
    let findings = process_simulated(&id)?;
    Ok((id, true, findings))
}

// ────────────────────────────────── 单测

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_state_transitions_legal_and_illegal() {
        use JobState::*;
        assert!(Pending.can_transition_to(Probing));
        assert!(Probing.can_transition_to(ReadyForInferring));
        assert!(ReadyForInferring.can_transition_to(Done));
        // 跳态非法
        assert!(!Pending.can_transition_to(Done), "不得跳态");
        assert!(!Done.can_transition_to(Pending), "终态不可回退");
        assert!(!Probing.can_transition_to(Pending));
        // 任意阶段可拒绝
        assert!(Pending.can_transition_to(Rejected));
        assert!(Probing.can_transition_to(Rejected));
        assert!(ReadyForInferring.can_transition_to(Rejected));
    }

    #[test]
    fn queue_enqueue_peek_count_clear() {
        let mut q = SemanticQueue::new();
        assert_eq!(q.count(), 0);
        let (id1, fresh1) = q.enqueue(JobKind::ScanLibrary, Some((1, 4)), None);
        assert!(fresh1);
        assert_eq!(q.count(), 1);
        assert!(q.peek(&id1).is_some());
        assert_eq!(q.peek(&id1).unwrap().state, JobState::Pending);
        let (_id2, fresh2) = q.enqueue(JobKind::CheckOutline, None, Some("卷01".into()));
        assert!(fresh2);
        assert_eq!(q.count(), 2);
        assert_eq!(q.clear(), 2);
        assert_eq!(q.count(), 0);
    }

    #[test]
    fn queue_rejects_duplicate_active_job() {
        let mut q = SemanticQueue::new();
        let (id1, f1) = q.enqueue(JobKind::ScanLibrary, Some((1, 4)), None);
        let (id2, f2) = q.enqueue(JobKind::ScanLibrary, Some((1, 4)), None);
        assert!(f1);
        assert!(!f2, "同 kind+slice+volume 未完成时重复入队必须幂等");
        assert_eq!(id1, id2, "重复入队返回既存 id");
        assert_eq!(q.count(), 1, "队列不重复膨胀");
    }

    #[test]
    fn duplicate_allowed_after_done() {
        let mut q = SemanticQueue::new();
        let (id1, _) = q.enqueue(JobKind::InspectCard, None, None);
        q.get_mut(&id1).unwrap().state = JobState::Done;
        let (_id2, fresh) = q.enqueue(JobKind::InspectCard, None, None);
        assert!(fresh, "完成后允许重新入队");
        assert_eq!(q.count(), 2);
    }

    #[test]
    fn try_drain_returns_actionable_only() {
        let mut q = SemanticQueue::new();
        let (id1, _) = q.enqueue(JobKind::ScanLibrary, Some((1, 4)), None);
        let (id2, _) = q.enqueue(JobKind::ScanLibrary, Some((2, 4)), None);
        q.get_mut(&id2).unwrap().state = JobState::Done;
        let drained = q.try_drain(true);
        assert_eq!(drained, vec![id1], "只取 Pending/ReadyForInferring");
        let (id3, _) = q.enqueue(JobKind::ScanLibrary, Some((3, 4)), None);
        assert_eq!(q.try_drain(false).len(), 1, "单次只取一个");
        assert!(q.peek(&id3).is_some());
    }

    #[test]
    fn derived_id_is_deterministic_and_param_sensitive() {
        // 同参数 → 同 id（跨进程幂等基础）
        let a = derived_id(JobKind::ScanLibrary, Some((1, 4)), None, None);
        let b = derived_id(JobKind::ScanLibrary, Some((1, 4)), None, None);
        assert_eq!(a, b, "同参数必须派生出同 id");
        assert!(a.starts_with("job-"));
        assert_eq!(a.len(), 20, "job- + 16 hex");
        // 任一参数变化 → id 变化
        assert_ne!(a, derived_id(JobKind::ScanLibrary, Some((2, 4)), None, None), "slice 变 → id 变");
        assert_ne!(a, derived_id(JobKind::CheckOutline, None, Some("卷01"), None));
        assert_ne!(a, derived_id(JobKind::InspectCard, None, None, Some("K1")));
        assert_ne!(
            derived_id(JobKind::InspectCard, None, None, Some("K1")),
            derived_id(JobKind::InspectCard, None, None, Some("K2")),
            "key 变 → id 变"
        );
    }

    #[test]
    fn simulate_findings_are_minimal_ok_uncertain() {
        let job = SemanticJob {
            id: "j1".into(),
            kind: JobKind::ScanLibrary,
            slice: Some((2, 5)),
            volume: None,
            probe_path: String::new(),
            created: 0,
            state: JobState::Probing,
        };
        let f = simulate_findings(&job);
        assert_eq!(f.len(), 2, "只产 2 条样例");
        assert_eq!(f[0]["verdict"], "OK");
        assert_eq!(f[1]["verdict"], "UNCERTAIN");
        assert!(f.iter().all(|x| x["law_ref"] == ""), "沙盘无真实法理依据");
        assert!(f[0]["key"].as_str().unwrap().contains("2of5"));
    }

    #[test]
    fn simulated_findings_route_only_via_emit() {
        // 沙盘 findings 走 emit 后：OK/UNCERTAIN 均不入库 → 零脏风险
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../governance_schema.sql")).unwrap();
        crate::governance::ensure_risk_index_cols(&conn).unwrap();
        let job = SemanticJob {
            id: "j2".into(), kind: JobKind::CheckOutline, slice: None,
            volume: Some("卷01".into()), probe_path: String::new(),
            created: 0, state: JobState::ReadyForInferring,
        };
        let findings = simulate_findings(&job);
        let n = crate::world_gov::emit_semantic_findings(&conn, "world-gov queue(sim)", &findings).unwrap();
        assert_eq!(n, 0, "OK/UNCERTAIN 样例不得入库（零脏风险）");
        let r = crate::governance::risk_list(&conn, false).unwrap();
        assert_eq!(r["semantic_count"], 0);
    }
}