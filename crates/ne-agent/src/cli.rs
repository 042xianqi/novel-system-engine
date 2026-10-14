//! CLI 参数结构（从 `main.rs` 拆出，架构门禁：单文件 ≤ 700 行）。
//!
//! 只有"形状"，无逻辑；逻辑在 `cmds.rs` 与 `ne_agent::*` 各模块里。

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "ne-agent",
    about = "《残响之河》Agent Runtime —— 单二进制智能体",
    version,
    long_about = None
)]
pub(crate) struct Cli {
    /// vault 目录（默认：当前目录下的 .neagent，或 $NE_AGENT_VAULT）
    #[arg(long, global = true, env = "NE_AGENT_VAULT", value_name = "DIR")]
    pub(crate) vault: Option<PathBuf>,
    /// 工作区沙箱根（chat 默认 vault/workspace；指向资料库根可直读真实库）
    #[arg(long, global = true, value_name = "DIR")]
    pub(crate) work_root: Option<PathBuf>,

    /// 输出 JSON（便于脚本消费）
    #[arg(long, global = true)]
    pub(crate) json: bool,

    #[command(subcommand)]
    pub(crate) cmd: Cmd,
}


#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// 管理项目隔离库（vault）
    Vault {
        #[command(subcommand)]
        cmd: VaultCmd,
    },
    /// 查看 / 校验模型配置
    Models {
        #[command(subcommand)]
        cmd: ModelsCmd,
    },
    /// 交互式对话
    Chat {
        /// 单次提问（非交互，直接打印回复后退出）
        #[arg(short, long)]
        message: Option<String>,
        /// 恢复最近一次会话
        #[arg(long)]
        resume: bool,
        /// 指定会话 id
        #[arg(long, value_name = "ID")]
        session: Option<String>,
        /// 新建会话
        #[arg(long)]
        new: bool,
        /// 只读规划模式：禁止一切写/执行/外部调用（覆盖 runtime.yaml）
        #[arg(long)]
        plan: bool,
        /// 审批模式：request / assisted / full_access（覆盖 runtime.yaml）
        #[arg(long, value_name = "MODE")]
        approval: Option<String>,
    },
    /// 管理会话
    Session {
        #[command(subcommand)]
        cmd: SessionCmd,
    },
    /// 查看/设置上下文预算（runtime.yaml 的 context 段）
    ///
    /// 不带参数 = 查看；带 --window/--reserve/--keep-recent 等 = 写回 runtime.yaml
    Context {
        /// 上下文窗口上限（tokens）
        #[arg(long)]
        window: Option<u32>,
        /// 为模型输出预留的 tokens（从窗口中扣除）
        #[arg(long)]
        reserve: Option<u32>,
        /// 自动压缩开关（--auto-compact 视为 true；也可 --auto-compact false）
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        auto_compact: Option<bool>,
        /// 自动压缩触发阈值（占折叠线的百分比，1~100）
        #[arg(long)]
        auto_threshold: Option<u32>,
        /// 保留最近 N 条工具结果不被截断
        #[arg(long)]
        keep_recent: Option<usize>,
        /// 仅打印最终配置，不写文件（配合 --json 供 UI 读）
        #[arg(long)]
        dry_run: bool,
    },
    /// 压缩会话上下文（把早期历史折叠成摘要；原文可回读）
    Compress {
        /// 会话 id（缺省用最新）
        #[arg(long)]
        id: Option<String>,
        /// 模式：after（默认，压当前轮之前）/ before（压锚点轮之前）
        #[arg(long, default_value = "after")]
        mode: String,
        /// 锚点：before 模式下必须原样摘自某条用户消息
        #[arg(long)]
        anchor: Option<String>,
        /// 用机械摘要（不调模型；离线/调试用）
        #[arg(long)]
        mechanical: bool,
        /// 查看当前压缩状态（不发压缩）
        #[arg(long)]
        show: bool,
        /// 清除本会话全部压缩记录（还原原始上下文；不删消息）
        #[arg(long)]
        clear: bool,
    },
    /// 按 id 读历史原文（含被压缩折叠的内容）
    History {
        #[command(subcommand)]
        cmd: HistoryCmd,
    },
    /// 列出内置工具
    Tools,
    /// 显示版本
    Version,
    /// 估算文本/文件的 token（调试上下文预算）
    Estimate {
        /// 文件路径（省略则从 stdin 读）
        #[arg(value_name = "FILE")]
        file: Option<PathBuf>,
    },
    /// 管理长期记忆（pinned 常驻 / recall 按需）
    Memory {
        #[command(subcommand)]
        cmd: MemoryCmd,
    },
    /// 按 l5_质检.md 对已生成正文做 LLM 深读质检（按需跑，不进门禁）
    /// 按 l5_质检.md 对已生成正文做 LLM 深读质检（按需跑，不进门禁，不烧常规额度）
    Score {
        /// 章号
        #[arg(long, default_value_t = 1)]
        chap_id: u32,
        /// 卷号
        #[arg(long, default_value = "卷01")]
        volume: String,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
        /// 指定正文文件（默认 04_正文/{volume}/第N章.md）
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// 按 l5_修复.md 定向修复（读质检 findings + 原文）
    Fix {
        #[arg(long, default_value_t = 1)]
        chap_id: u32,
        #[arg(long, default_value = "卷01")]
        volume: String,
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
        /// findings JSON 文件路径
        #[arg(long)]
        findings: Option<PathBuf>,
        /// 只打印修复后正文不落盘
        #[arg(long)]
        dry_run: bool,
    },
    /// 生成一章正文（细纲 → 注入包 → 模型 → 门禁）
    Novel {
        /// 章号（第几章）
        #[arg(long, default_value_t = 1)]
        chap_id: u32,
        /// 卷号
        #[arg(long, default_value = "卷01")]
        volume: String,
        /// 资料库根（默认：vault 的 workspace 或 $NE_AGENT_LIB）
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
        /// 只生成不落盘
        #[arg(long)]
        dry_run: bool,
        /// 只组装 prompt，不调模型（调试用；打印 prompt 长度与前 500 字）
        #[arg(long)]
        prompt_only: bool,
        /// 启用记忆子系统（preprocess → 三层系统提示词 → post_update）。
        ///
        /// 默认 **true**（60 章压测已通过）。三种写法均兼容：
        /// `--use-memory`、`--use-memory=true`、`--use-memory=false`。
        /// 省略则用默认值 true。
        #[arg(long, num_args = 0..=1, default_value_t = true, default_missing_value = "true")]
        use_memory: bool,
        /// 落盘后可选合规复核（默认关闭；开启只记录 refs/待评，不改生成内容）
        #[arg(long)]
        check_compliance: bool,
    },
    /// L0~锚点层级健康巡检（独立外挂；--semantic 默认关闭）
    AuditLayerHealth {
        /// 开启后才调轻量模型做语义检查
        #[arg(long)]
        semantic: bool,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 资料库整套治理工具集
    LibraryCtl {
        #[command(subcommand)]
        cmd: LibraryCtlCmd,
    },
    /// 章节合规复核（独立外挂；--semantic 开启才调模型）
    AuditChapterCompliance {
        #[arg(long, default_value_t = 0)]
        from: u32,
        #[arg(long)]
        to: Option<u32>,
        #[arg(long, default_value = "卷01")]
        volume: String,
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
        /// 开启才调模型
        #[arg(long)]
        semantic: bool,
    },
    /// 全卷引用图谱（显式引用 vs 上下文带入）
    AuditRefs {
        #[arg(long, default_value = "卷01")]
        volume: String,
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 风险清单（默认只看未解决）
    AuditRiskList {
        /// 列出已解决的风险
        #[arg(long)]
        all: bool,
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 里程碑快照（仅提供能力，不自动执行）
    LibCheckpoint {
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
        /// 输出目录（默认 <库根>/09_冷备份）
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// 只读审计：验证生成链路实际取材覆盖面（不调模型、不写库）
    AuditMaterialCoverage {
        /// 起始章号
        #[arg(long, default_value_t = 1)]
        from: u32,
        /// 结束章号（省略 = 只审计 from 单章）
        #[arg(long)]
        to: Option<u32>,
        /// 卷号
        #[arg(long, default_value = "卷01")]
        volume: String,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 世界观认知治理层（独立外挂；语义默认关，只产证据与建议）
    WorldGov {
        #[command(subcommand)]
        cmd: WorldGovCmd,
    },
    /// 管理外部 MCP 服务
    Mcp {
        #[command(subcommand)]
        cmd: McpCmd,
    },
    /// 管理持久化任务（状态机 + CAS）
    Task {
        #[command(subcommand)]
        cmd: TaskCmd,
    },
    /// 运行子智能体 Lane（顺序子会话，写租约校验）
    Subagent {
        /// 子任务 JSON 文件（含 subagents 数组）或 "-" 从 stdin 读
        #[arg(value_name = "FILE")]
        file: PathBuf,
        /// 会话 id（缺省用最新会话）
        #[arg(long)]
        session: Option<String>,
        /// 最大子任务数
        #[arg(long, default_value_t = 6)]
        max_tasks: usize,
    },
}

#[derive(Subcommand, Debug)]
pub enum TaskCmd {
    /// 列出所有任务
    List,
    /// 新建一个 queued 任务
    New {
        /// 会话 id
        #[arg(long)]
        session: String,
        /// 任务描述（必填）
        description: String,
        /// 标题（缺省取描述首行）
        #[arg(long)]
        title: Option<String>,
        /// 是否允许进程重启后自动恢复
        #[arg(long, default_value_t = true)]
        auto_resume: bool,
        /// 最大尝试次数
        #[arg(long, default_value_t = 2)]
        max_attempts: i64,
    },
    /// 查看一个任务
    Show { id: String },
    /// 手动转移任务状态（调试/运维）
    Set {
        id: String,
        /// 目标状态：running/waiting_approval/recovering/suspended/failed/completed/cancelled
        status: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum McpCmd {
    /// 列出已配置的服务
    List,
    /// 连接服务并枚举其工具（会真的启动服务）
    Tools {
        /// 只查某个服务
        name: Option<String>,
    },
    /// 直接调用一个工具（调试用）：mcp call <服务> <工具> '{"k":"v"}'
    Call {
        server: String,
        tool: String,
        /// JSON 参数对象（默认 {}）
        #[arg(default_value = "{}")]
        args_json: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum MemoryCmd {
    /// 列出记忆
    List {
        /// 只看 pinned
        #[arg(long)]
        pinned: bool,
    },
    /// 保存一条记忆（同 scope+owner+key 覆盖）
    Save {
        /// 键
        key: String,
        /// 值
        value: String,
        /// 作用域：global | project | session
        #[arg(long, default_value = "project")]
        scope: String,
        /// 类型：preference | rule | fact | project_info
        #[arg(long, default_value = "fact")]
        kind: String,
        /// 钉选到 system prompt 稳定前缀（始终注入）
        #[arg(long)]
        pinned: bool,
    },
    /// 删除一条记忆
    Forget {
        key: String,
        #[arg(long, default_value = "project")]
        scope: String,
    },
    /// 试跑召回：给一段查询，看会召回哪些（不发模型请求）
    Recall {
        /// 查询文本
        query: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum SessionCmd {
    /// 列出会话
    List,
    /// 显示某会话的历史消息
    Show {
        /// 会话 id（缺省用最新）
        id: Option<String>,
    },
    /// 回退会话分支（不删消息，只移动 head）
    Rewind {
        /// 会话 id（缺省用最新）
        #[arg(long)]
        id: Option<String>,
        /// 回退到指定消息 id
        #[arg(long, conflicts_with_all = ["back", "last_turn"])]
        to: Option<i64>,
        /// 往回退 N 条消息
        #[arg(long, conflicts_with_all = ["to", "last_turn"])]
        back: Option<usize>,
        /// 撤掉最后一轮 user（重来这一轮）
        #[arg(long = "last-turn", conflicts_with_all = ["to", "back"])]
        last_turn: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum HistoryCmd {
    /// 按 id 读取一条历史原文（含被压缩折叠的内容）
    Read {
        /// 消息 id
        id: i64,
        /// 会话 id（缺省用最新）
        #[arg(long, alias = "id")]
        session: Option<String>,
    },
    /// 在会话全部消息中模糊搜索（含被压缩折叠的原文）
    Search {
        /// 搜索词
        query: String,
        /// 会话 id（缺省用最新）
        #[arg(long, alias = "id")]
        session: Option<String>,
        /// 最多返回条数
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
}

#[derive(Subcommand, Debug)]
pub enum VaultCmd {
    /// 初始化 vault（建目录 + 写缺省配置）
    Init,
    /// 显示 vault 状态（路径、文件、是否就绪）
    Show,
    /// 从全局 OMP 配置导入 models.yml / config.yml
    ImportLlm,
}

#[derive(Subcommand, Debug)]
pub enum ModelsCmd {
    /// 列出 provider / key 数量 / 模型 / 角色（不打印 key）
    List,
    /// 校验配置能否组装出可用的 LlmParams（不发请求）
    Check,
}


#[derive(Subcommand, Debug)]
pub enum WorldGovCmd {
    /// 建立世界观法理基线（只存摘要指纹，不存原文；不修改任何文件）
    Bootstrap {
        /// 沙盘：只输出报告，不写 world_baseline.json
        #[arg(long)]
        dry_run: bool,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 单卡深度检视（默认产探针包；--semantic 才调模型）
    InspectCard {
        /// 卡片 key（如 CHAR-90-038）
        #[arg(long)]
        key: String,
        /// 开启才调模型做语义裁决
        #[arg(long)]
        semantic: bool,
        /// 沙盘：--semantic 时只打印将发送的 prompt，不调模型
        #[arg(long)]
        dry_run: bool,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 法理溯源链（key ← 受约束于 ← 上层 ← L0 源头）
    Trace {
        #[arg(long)]
        key: String,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 【预留·第 2 批】全库语义脏点扫描（--semantic + --slice 分批）
    ScanLibrary {
        /// 显式开关；开启才调模型
        #[arg(long)]
        semantic: bool,
        /// 分批：N/M（如 1/4）
        #[arg(long)]
        slice: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 【预留·第 2 批】卷细纲法理评估（只出报告不改细纲）
    CheckOutline {
        #[arg(long, default_value = "卷01")]
        volume: String,
        #[arg(long)]
        semantic: bool,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
pub enum LibraryCtlCmd {
    /// 资料库标准化初始化（只建索引、只标记脏数据，不动原文件）
    Init,
    /// 分类建议（只读；输出当前层级|建议层级|建议tag|字数，可导 CSV/MD）
    Classify {
        /// 导出目录（默认 <库根>/07_勘误工坊/export）
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// 资料库深度巡检（--semantic 默认关闭）
    Audit {
        #[arg(long)]
        semantic: bool,
    },
    /// 导出生效 L0~L4 条目为 JSON+Markdown（可 --layer 分层）
    Export {
        #[arg(long)]
        out: Option<PathBuf>,
        /// 只导出指定层级（如 L4 / L0 / L0-L1x），省略=全导
        #[arg(long)]
        layer: Option<String>,
    },
    /// 基于磁盘源重建 LTM 索引清单（不覆盖库；支持 --batch 断点续扫）
    Reindex {
        /// 每批处理文件数（万级库分批用；0=不分批）
        #[arg(long, default_value_t = 0)]
        batch: usize,
    },
    /// 资料库大盘统计（总条目/总字符/分层/碎片/未归类/孤立/重复）
    Stats,
}
