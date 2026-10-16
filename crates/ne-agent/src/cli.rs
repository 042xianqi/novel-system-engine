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
    /// 关闭进度/冗余日志（进度只走 stderr，不影响 --json 的 stdout）
    #[arg(long, global = true)]
    pub(crate) quiet: bool,
    /// 日志模式：plain（默认）| structured（stderr 每行 JSON）
    #[arg(long, global = true, default_value = "plain")]
    pub(crate) log_mode: String,
    /// 日志输出文件（不指定则写 stderr；不自动写盘）
    #[arg(long, global = true, value_name = "PATH")]
    pub(crate) log_file: Option<PathBuf>,

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
        /// 生成前跑注入就绪闸门（细纲/资料卡/角色卡/伏笔/事件/前文/记忆/钩子）。
        /// 发现 Fatal 项即中止生成，不调模型。默认关闭。
        #[arg(long)]
        preflight: bool,
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
    /// 系统健康自检：加载时模块自检 + 生成前注入就绪闸门（只读、零外呼）
    Health {
        #[command(subcommand)]
        cmd: HealthCmd,
    },
    /// 细纲工具：把「1 拍 5 章概要复制」重建为「5 个递进子拍」（调 LLM）
    Outline {
        #[command(subcommand)]
        cmd: OutlineCmd,
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
    /// 【规约适配】全局系统自检（= world-gov self-check run 的别名入口）
    ///
    /// 第 18 批：对齐《项目总规约》的 `sys-check full --json`。底层**复用**
    /// `world-gov self-check` 同一实现，不另起一套。
    SysCheck {
        /// 子命令：full（全套自检）。缺省即 full。
        #[arg(default_value = "full")]
        mode: String,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 【规约适配】管线自动扫描（= scan-library 的 AI 引导包装）
    ///
    /// 第 18 批：对齐《项目总规约》的 `pipeline scan-auto --ai-guided`。
    /// **默认只产探针清单（零外呼）**；`--ai-run` 才显式开闸门走真实语义扫描。
    Pipeline {
        #[command(subcommand)]
        cmd: PipelineCmd,
    },
    /// 【规约适配】顶层快照入口（= `world-gov diff snap` 别名，对齐规约 `snap create`）
    ///
    /// 第 18 批：底层完全复用 `world-gov diff snap`，不另起逻辑。
    Snap {
        #[command(subcommand)]
        cmd: DiffSnapCmd,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 【规约适配】顶层 Web 面板入口（= `world-gov web` 别名，对齐规约 `web serve`）
    Web {
        #[command(subcommand)]
        cmd: WebCmd,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 【规约适配】顶层沙盘入口（= `world-gov sandbox` 别名，对齐规约 `sandbox spawn`）
    Sandbox {
        #[command(subcommand)]
        cmd: SandboxCmd,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
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
    /// 【规约适配】探测全部 MCP 服务及其工具可用性（对齐 `mcp probe-all --json`）
    ///
    /// 逐一连接每个已启用服务，报告握手是否成功、工具数、以及（可选）逐个轻量调用探活。
    ProbeAll {
        /// 对每个工具做一次轻量调用探活（默认只握手 + 列工具）
        #[arg(long)]
        call_test: bool,
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

/// 系统健康自检子命令。
#[derive(Subcommand, Debug)]
pub enum HealthCmd {
    /// 加载时模块自检：库根结构 / novel.db 业务表 / 配置自洽 / 细纲 / 模板 / 角色卡。
    Modules {
        /// 卷号
        #[arg(long, default_value = "卷01")]
        volume: String,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 生成前注入就绪闸门：检查指定章所有待注入项（细纲/资料卡/角色卡/伏笔/事件/前文/记忆/钩子）。
    Preflight {
        /// 章号
        #[arg(long, default_value_t = 1)]
        chapter: u32,
        /// 卷号
        #[arg(long, default_value = "卷01")]
        volume: String,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 启动自检：设计层词污染全量扫描（注入文件/注入表/资料卡/已生成正文/细纲雷同）。
    Boot {
        /// 卷号
        #[arg(long, default_value = "卷01")]
        volume: String,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
}

/// 细纲工具子命令。
#[derive(Subcommand, Debug)]
pub enum OutlineCmd {
    /// 把「1 拍 5 章概要复制」重建为「5 个递进子拍」（调 LLM；原文件备份 .bak_rebuild）。
    Rebuild {
        /// 卷号
        #[arg(long, default_value = "卷01")]
        volume: String,
        /// 只重建第 N 拍（1 起）；不传 = 全卷
        #[arg(long)]
        beat: Option<usize>,
        /// 只生成不落盘（打印预览）
        #[arg(long)]
        dry_run: bool,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 设计层词清洗：把细纲章文本的工程词改写为叙事层表达（调 LLM；备份 .bak_sanitize）。
    Sanitize {
        /// 卷号
        #[arg(long, default_value = "卷01")]
        volume: String,
        /// 只清洗第 N 章（1 起）；不传 = 全卷
        #[arg(long)]
        chapter: Option<u32>,
        /// 只生成不落盘（打印预览）
        #[arg(long)]
        dry_run: bool,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 细纲出厂门禁：查本卷细纲章文本的设计层词与章概要雷同（只读，不调模型）。
    Check {
        /// 卷号
        #[arg(long, default_value = "卷01")]
        volume: String,
        /// JSON 输出
        #[arg(long)]
        json: bool,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
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
        /// 第 13 批：真实 LLM 三重锁之命令行锁（还需 feature + WOGOV_ALLOW_REAL_LLM=yes）
        #[arg(long)]
        real_semantic: bool,
        /// 提交任务进内存管线队列（沙盘：同调用内走通 enqueue→simulate→入库）
        #[arg(long)]
        enqueue: bool,
        /// 落库持久化（默认 off = 纯内存队列；开启后才写 novel.db.task_jobs）
        #[arg(long)]
        persist: bool,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 法理溯源链（key ← 受约束于 ← 上层 ← L0 源头；纯只读）
    Trace {
        #[arg(long)]
        key: String,
        /// 兼容沙盘写法：本命令纯只读，接受但无副作用
        #[arg(long)]
        dry_run: bool,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 【调试】门控状态检查：打印三条件真值（hidden，不出现在 help）
    #[command(hide = true)]
    GateCheck {
        /// 显式携带 --semantic 模拟"命令行已开"场景
        #[arg(long)]
        semantic: bool,
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
        /// 提交任务进内存管线队列（沙盘走通 enqueue→simulate→入库）
        #[arg(long)]
        enqueue: bool,
        /// 落库持久化（默认 off = 纯内存；开启后写 novel.db.task_jobs）
        #[arg(long)]
        persist: bool,
        /// 逃生门：关闭层过滤，扫描全部条目（含 AUX/L1 等工程辅助）
        #[arg(long)]
        all_layers: bool,
        /// 增量扫描：Unix 时间戳数字，或 `last`（读 .scan_snapshot.json 的上次锚点）
        #[arg(long)]
        since: Option<String>,
        /// 基于版本快照做增量（自该快照以来 增/改 的条目；第 11 批）
        #[arg(long = "since-snap")]
        since_snap: Option<String>,
        /// 扫描成功后写回 .scan_snapshot.json（时间锚点 + 统计 + 指纹；--dry-run 时不写）
        #[arg(long)]
        write_snapshot: bool,
        /// 关闭探针缓存（默认开启）
        #[arg(long)]
        no_cache: bool,
        /// 强制无视缓存，全量重生成
        #[arg(long)]
        invalidate_cache: bool,
        /// 只扫含该标签的条目（第 10 批；可重复；默认不过滤）
        #[arg(long = "with-tag", value_name = "TAG")]
        with_tag: Vec<String>,
        /// 排除含该标签的条目（第 10 批；可重复）
        #[arg(long = "without-tag", value_name = "TAG")]
        without_tag: Vec<String>,
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
        /// 提交任务进内存管线队列
        #[arg(long)]
        enqueue: bool,
        /// 落库持久化（默认 off = 纯内存；开启后写 novel.db.task_jobs）
        #[arg(long)]
        persist: bool,
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 语义管线队列（**进程内存**，进程退出即清空；不碰磁盘库）
    Queue {
        #[command(subcommand)]
        cmd: QueueCmd,
    },
    /// 【调试】只跑 semantic_runtime 沙盘替身，验证 序列化→findings→入库 全链路（hidden）
    #[command(hide = true)]
    SemanticVerify {
        /// 沙盘 seed（同 seed 同输出，可复现）
        #[arg(long, default_value_t = 42)]
        seed: u64,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 【调试】prompt_builder 预览：探针 JSON → 结构化 prompt 文本（**独立于推理管线，零外呼**）
    #[command(hide = true)]
    PromptPreview {
        /// 探针类型：inspect-card | scan-library | check-outline
        #[arg(long)]
        kind: String,
        /// inspect-card：卡片 key（走真实探针产出）；与 --probe-file 二选一
        #[arg(long)]
        key: Option<String>,
        /// 直接读探针 JSON 文件（scan-library/check-outline 报告或 inspect-card 探针包）
        #[arg(long)]
        probe_file: Option<PathBuf>,
        /// check-outline：卷名（走真实探针产出）；与 --probe-file 二选一
        #[arg(long)]
        volume: Option<String>,
        /// 把预览对应的探针 JSON 落盘到该目录（方便调试；不指定则不写）
        #[arg(long, value_name = "DIR")]
        export_probe: Option<PathBuf>,
        /// 打印完整 user（默认只打印统计 + 前 800 字）
        #[arg(long)]
        full: bool,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 【调试】审计报告导出（只读拼接 risk_index/task_jobs/基线；产物只落 reports/）
    #[command(hide = true)]
    AuditExport {
        #[command(subcommand)]
        cmd: AuditExportCmd,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 【调试】批量检视（三种输入；默认只预览/入队，绝不自动推理）
    #[command(hide = true)]
    InspectBatch {
        /// 模式 a：读 scan 落盘探针 JSON，取其 key 列表
        #[arg(long)]
        from_scan: Option<String>,
        /// 模式 b：读文本文件（每行一个 key，忽略空行/#注释）
        #[arg(long)]
        id_list: Option<String>,
        /// 模式 c：增量取 key（Unix 时间戳或 last）
        #[arg(long)]
        since: Option<String>,
        /// 分片：只取第 N/M 片（如 1/4）；结合 --since/全库再切分（第 17 批）
        #[arg(long, value_name = "N/M")]
        slice: Option<String>,
        /// 只算数量，不写任何任务/探针
        #[arg(long)]
        dry_run: bool,
        /// 【第 18.2 批】只输出 key 清单（每行一个，纯文本，便于脚本消费）；不推理、不落库
        #[arg(long)]
        list_keys: bool,
        /// 入队（默认只预览不入队）
        #[arg(long)]
        enqueue: bool,
        /// 入队是否落库持久化（仅 --enqueue 时有效）
        #[arg(long)]
        persist: bool,
        /// 关闭层过滤（默认只取 L2/L3/L4）
        #[arg(long)]
        all_layers: bool,
        /// 只取含该标签的条目（第 10 批；可重复；默认不过滤）
        #[arg(long = "with-tag", value_name = "TAG")]
        with_tag: Vec<String>,
        /// 排除含该标签的条目（第 10 批；可重复）
        #[arg(long = "without-tag", value_name = "TAG")]
        without_tag: Vec<String>,
        /// 第 13 批：真实 LLM 三重锁之命令行锁（批量语义）
        #[arg(long)]
        real_semantic: bool,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 【调试】探针缓存（status|gc|clear；只动 07_勘误工坊/probe_cache/）
    #[command(hide = true)]
    Cache {
        #[command(subcommand)]
        cmd: CacheCmd,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 【调试】探针打包导出（create|list|unpack；只含结构化探针，不含资料库源文件）
    #[command(hide = true)]
    ProbeExport {
        #[command(subcommand)]
        cmd: ProbeExportCmd,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 【调试】健康自检（环境/索引/目录/管线；--fix 仅建目录）
    #[command(hide = true)]
    SelfCheck {
        #[command(subcommand)]
        cmd: SelfCheckCmd,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 【调试】人工标签（add|rm|list|filter|export|import；只动 item_tag，与主索引隔离）
    #[command(hide = true)]
    Tag {
        #[command(subcommand)]
        cmd: TagCmd,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 【调试】人工备注（set|get|list|export|import；只动 item_note，与主索引隔离）
    #[command(hide = true)]
    Note {
        #[command(subcommand)]
        cmd: NoteCmd,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 【调试】差异快照（snap create/list/delete/export | show | filter-changed；逻辑快照不复制正文）
    #[command(hide = true)]
    Diff {
        #[command(subcommand)]
        cmd: DiffCmd,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 【调试】Web 面板（serve；仅 127.0.0.1，零依赖，无后台常驻）
    #[command(hide = true)]
    Web {
        #[command(subcommand)]
        cmd: WebCmd,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 【调试】冲突记录库（list|show|review|summary；存 AI 观点，人是终审）
    #[command(hide = true)]
    Conflict {
        #[command(subcommand)]
        cmd: ConflictCmd,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 【调试】关系网（build|list|summary|dangling|orphans|neighbors|path|cycles；只读）
    #[command(hide = true)]
    Graph {
        #[command(subcommand)]
        cmd: GraphCmd,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 【调试】沙盘推演（run|verify|list|show|delete；确定性多步推演，可重放）
    #[command(hide = true)]
    Sandbox {
        #[command(subcommand)]
        cmd: SandboxCmd,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 【调试】终态总览（只读聚合全批次产物：能力/隔离表/关系网/沙盘/三重锁/闭环链）
    #[command(hide = true)]
    Status {
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 【调试】LLM 深度接入（llm stats|backends|cache|config|preview；默认三重闸门锁死）
    #[command(hide = true)]
    Llm {
        #[command(subcommand)]
        cmd: LlmCmd,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
pub enum ConflictCmd {
    /// 列出冲突记录（可按状态/key 过滤）
    List {
        /// pending | valid | false_alarm | ignored
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        key: Option<String>,
        /// 显示原始 prompt/response 全文
        #[arg(long)]
        full: bool,
    },
    /// 查看单条（含 raw 预览）
    Show {
        id: i64,
        #[arg(long)]
        full: bool,
    },
    /// 人工复核：更新 review_status（可反向给条目打 ai-flagged）
    Review {
        id: i64,
        /// pending | valid | false_alarm | ignored
        #[arg(long)]
        status: String,
        /// 同时给该条目的 probe_key 打 ai-flagged 标签
        #[arg(long)]
        flag: bool,
    },
    /// 统计（总数 + 按状态分布）
    Summary,
}
#[derive(Subcommand, Debug)]
pub enum GraphCmd {
    /// 构建关系图（读卡正文抽引用边 → 事务重建隔离表 rel_edge；可 --dry-run）
    Build {
        /// 只统计不落库
        #[arg(long)]
        dry_run: bool,
        /// 限流：最多扫描多少张卡
        #[arg(long)]
        max_cards: Option<usize>,
        /// 关联快照 id（写入 meta，便于回溯）
        #[arg(long)]
        snap: Option<String>,
    },
    /// 列出引用边（可按 src 过滤）
    List {
        #[arg(long)]
        key: Option<String>,
        #[arg(long, default_value_t = 200)]
        limit: usize,
    },
    /// 统计摘要（节点/边/悬空/孤立/top度）
    Summary {
        #[arg(long, default_value_t = 10)]
        top: usize,
    },
    /// 悬空引用（指向不存在的卡）
    Dangling {
        #[arg(long, default_value_t = 200)]
        limit: usize,
    },
    /// 孤立卡（无入边）
    Orphans {
        #[arg(long, default_value_t = 200)]
        limit: usize,
    },
    /// 某卡的上/下游邻居
    Neighbors {
        #[arg(long)]
        key: String,
        /// out|in（默认两者都出）
        #[arg(long)]
        dir: Option<String>,
    },
    /// 最短引用链 from → to
    Path {
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
        #[arg(long, default_value_t = 6)]
        max_depth: usize,
    },
    /// 循环引用检测
    Cycles {
        #[arg(long, default_value_t = 6)]
        max_len: usize,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
}
#[derive(Subcommand, Debug, Clone)]
pub enum SandboxCmd {
    /// 跑一次多步确定性推演（--save 才落库；数值可重放）
    Run {
        /// 场景 JSON 文件路径（--scenario 或 --quick 二选一）
        #[arg(long)]
        scenario: Option<String>,
        /// 快捷预设（warn / critical / tip / calm）
        #[arg(long)]
        quick: Option<String>,
        /// 推演主体卡 key（取自关系网，给出受影响集）
        #[arg(long)]
        actor: Option<String>,
        /// 记录标签
        #[arg(long, default_value = "")]
        label: String,
        /// 落库（默认只算不写）
        #[arg(long)]
        save: bool,
    },
    /// 重放校验（同 scenario 重跑，比对指纹）
    Verify {
        run_id: i64,
    },
    /// 列出推演记录
    List {
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// 查看单条（--full 含 scenario 与逐步状态）
    Show {
        run_id: i64,
        #[arg(long)]
        full: bool,
    },
    /// 删除一条推演记录（含步骤）
    Delete {
        run_id: i64,
    },
    /// 从逻辑快照派生沙箱初始态（第 17 批；只读快照，不推演）
    Spawn {
        /// 逻辑快照 id（world-gov diff snap create 产出）
        #[arg(long)]
        from_snap: String,
        /// 把派生初始态落库成一条 0 步基线记录（默认只算不写）
        #[arg(long)]
        save: bool,
        /// 【规约适配】AI 沙箱模式：从快照派生后，用已放行的 LLM 做一次 AI 引导推演
        /// （对齐 `sandbox spawn --ai-sandbox`）。默认关；开启仍受三重锁约束。
        #[arg(long)]
        ai_sandbox: bool,
        /// ai-sandbox 模式：把 AI 引导后的初始态落库（默认只算不写）
        #[arg(long)]
        dry_run: bool,
    },
    /// 逐步轨迹（第 17 批；只读某次推演的 step 序列）
    Trace {
        #[command(subcommand)]
        cmd: SandboxTraceCmd,
    },
}

/// sandbox trace 子命令。
#[derive(Subcommand, Debug, Clone)]
pub enum SandboxTraceCmd {
    /// 列出某次推演的逐步轨迹（--run 指定 run_id）
    List {
        #[arg(long)]
        run: i64,
    },
}

/// 【规约适配】pipeline 子命令（对齐《项目总规约》的 pipeline 命令面）。
///
/// 全部为**适配层**：底层复用 world-gov 既有成熟实现，不另起一套。
#[derive(Subcommand, Debug)]
pub enum PipelineCmd {
    /// AI 引导的全库扫描（= scan-library 的包装；默认只产探针清单，--ai-run 才真实语义扫描）
    ScanAuto {
        /// 分片 N/M（如 1/4）
        #[arg(long)]
        slice: Option<String>,
        /// 显式请求 AI 引导（对齐规约 `--ai-guided`）；仅标注意图，仍需 --ai-run 才外呼
        #[arg(long)]
        ai_guided: bool,
        /// 真实语义扫描（三重锁之命令行锁；仍需 feature + WOGOV_ALLOW_REAL_LLM=yes）
        #[arg(long)]
        ai_run: bool,
        /// 只算不落盘
        #[arg(long)]
        dry_run: bool,
        /// 关闭层过滤（默认只扫 L2/L3/L4）
        #[arg(long)]
        all_layers: bool,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
    /// 世界观分析（= scan-library --semantic 的别名；只产结构化报告）
    WorldAnalyze {
        /// 分片 N/M
        #[arg(long)]
        slice: Option<String>,
        /// 真实语义分析走统一收口（三重锁之命令行锁）
        #[arg(long)]
        ai_run: bool,
        /// 只算不落盘
        #[arg(long)]
        dry_run: bool,
        /// 资料库根
        #[arg(long, env = "NE_AGENT_LIB", value_name = "DIR")]
        lib: Option<PathBuf>,
    },
}
#[derive(Subcommand, Debug)]
pub enum LlmCmd {
    /// 调用统计（token 累计 / 缓存命中 / 熔断状态）
    Stats {
        /// 重置熔断
        #[arg(long)]
        reset_breaker: bool,
    },
    /// 可用后端清单（含 feature 状态）
    Backends,
    /// 推理缓存管理：status | list | gc | clear
    #[command(subcommand)]
    Cache(LlmCacheCmd),
    /// 配置：show（安全快照）| init（写注释模板）| path
    #[command(subcommand)]
    Config(LlmConfigCmd),
    /// 预览 prompt（只渲染不发送；需 --template）
    Preview {
        /// 模板名（check_world | entity_extract | sandbox_step）
        #[arg(long)]
        template: String,
        /// 探针 JSON 文件（其指纹进 cache_key）
        #[arg(long)]
        probe: Option<String>,
        /// 导出完整 prompt 到文件（不发起调用）
        #[arg(long)]
        out: Option<String>,
    },
    /// 跑一次管线调用（默认 mock；--real-semantic + 三重闸门才真出网）
    Run {
        /// 模板名
        #[arg(long)]
        template: String,
        /// 探针 JSON 文件
        #[arg(long)]
        probe: Option<String>,
        /// 真实调用（还需 feature + WOGOV_ALLOW_REAL_LLM=yes）
        #[arg(long)]
        real_semantic: bool,
        /// 只用 mock 后端（不联网）
        #[arg(long)]
        mock: bool,
        /// 跳过缓存
        #[arg(long)]
        bypass_cache: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum LlmCacheCmd {
    /// 缓存状态（条数/大小/过期）
    Status,
    /// 列出缓存条目
    List {
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// GC：删过期缓存
    Gc {
        #[arg(long, default_value_t = 30)]
        ttl_days: u64,
    },
    /// 清空全部缓存
    Clear,
}

#[derive(Subcommand, Debug)]
pub enum LlmConfigCmd {
    /// 显示安全快照（不回显 key/URL）
    Show,
    /// 写注释模板到 {work-root}/config/llm.toml（已存在不覆盖，除非 --force）
    Init {
        #[arg(long)]
        force: bool,
    },
    /// 打印配置文件路径
    Path,
    /// 写提示词模板目录（prompts/*.toml）
    InitPrompts {
        #[arg(long)]
        force: bool,
    },
}
#[derive(Subcommand, Debug, Clone)]
pub enum WebCmd {
    /// 启动面板（前台阻塞；Ctrl-C 退出）
    Serve {
        /// 端口（默认 8270；仅绑定 127.0.0.1）
        #[arg(long, default_value_t = 8270)]
        port: u16,
        /// 不自动打开浏览器（本环境默认即不打开）
        #[arg(long)]
        no_open: bool,
        /// 【规约适配】只做一次健康探测后立即退出（对齐 `web serve --health-check-only`）
        /// 真实起服务 + 发一个请求验证可用，然后关闭；不常驻。
        #[arg(long)]
        health_check_only: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum DiffCmd {
    /// 快照管理：create | list | delete | export
    Snap {
        #[command(subcommand)]
        cmd: DiffSnapCmd,
    },
    /// 对比两快照：added / removed / modified
    Show {
        /// 基线快照 id
        snap_a: String,
        /// 目标快照 id
        snap_b: String,
    },
    /// 输出自某快照以来改动（增/改）的 key 列表（可管道进 scan/inspect-batch）
    FilterChanged {
        /// 锚点快照 id
        #[arg(long = "since-snap")]
        since_snap: String,
        /// 自动给改动条目打标签（仅打标签，不做任何自动校验）
        #[arg(long)]
        auto_tag_changed: bool,
        /// 自动打的标签名（默认「最近变更」）
        #[arg(long, default_value = "最近变更")]
        tag: String,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum DiffSnapCmd {
    /// 拍快照（只存索引+元信息哈希，不复制正文）
    Create {
        /// 可命名：v1-初始设定 / v2-修改北境势力
        #[arg(long)]
        label: Option<String>,
    },
    /// 列出全部快照
    List,
    /// 删除快照（只删本表行，不动任何业务数据）
    Delete {
        snap_id: String,
    },
    /// 导出该快照对应探针集合（复用 probe-export；--to 目录）
    Export {
        snap_id: String,
        #[arg(long = "to", value_name = "DIR")]
        to: PathBuf,
    },
}

#[derive(Subcommand, Debug)]
pub enum TagCmd {
    /// 给条目加标签（可多个）
    Add {
        /// 条目 key（如 CHAR-10-001）
        key: String,
        /// 一个或多个标签
        #[arg(required = true, value_name = "TAG")]
        tags: Vec<String>,
    },
    /// 删某条目的某个标签
    Rm {
        key: String,
        tag: String,
    },
    /// 列标签（可按键/按标签过滤）
    List {
        #[arg(long)]
        key: Option<String>,
        #[arg(long)]
        tag: Option<String>,
    },
    /// 只看某标签匹配的 key 列表（可管道给其他命令）
    Filter {
        #[arg(long = "tag")]
        tag: String,
    },
    /// 导出全部标签库备份（JSON）
    Export {
        #[arg(long = "to", value_name = "PATH")]
        to: PathBuf,
    },
    /// 从备份导入标签库
    Import {
        #[arg(value_name = "PATH")]
        path: PathBuf,
        #[arg(long)]
        dry_run: bool,
        /// 覆盖（先清空被导入 key 的现有标签）
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum NoteCmd {
    /// 设置/更新条目备注（新建或部分更新）
    Set {
        /// 条目 key
        key: String,
        /// 复核状态：unreviewed|ok|suspect|false_positive|critical
        #[arg(long)]
        status: Option<String>,
        /// 自由备注文本
        #[arg(long)]
        content: Option<String>,
    },
    /// 取某条目备注
    Get {
        #[arg(long)]
        key: String,
    },
    /// 列备注（可按状态过滤）
    List {
        #[arg(long)]
        status: Option<String>,
    },
    /// 导出全部备注库备份（JSON）
    Export {
        #[arg(long = "to", value_name = "PATH")]
        to: PathBuf,
    },
    /// 从备份导入备注库
    Import {
        #[arg(value_name = "PATH")]
        path: PathBuf,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum CacheCmd {
    /// 缓存状态：数量、总大小、旧于 N 天条目数
    Status {
        /// 视为"旧"的天数阈值
        #[arg(long, default_value_t = 30)]
        older_than: u64,
    },
    /// 清理超期缓存
    Gc {
        /// 清理旧于 N 天的缓存项
        #[arg(long, default_value_t = 30)]
        older_than: u64,
    },
    /// 全部清空
    Clear,
}

#[derive(Subcommand, Debug)]
pub enum ProbeExportCmd {
    /// 打包一批探针为一个 .wg-probe.tar.gz
    Create {
        /// 输出路径（默认 07_勘误工坊/probes_<ts>.wg-probe.tar.gz）
        #[arg(long, value_name = "PATH")]
        out: Option<PathBuf>,
        /// 只含指定层（逗号分隔，如 L2,L3）
        #[arg(long)]
        only_layers: Option<String>,
        /// 只含 src_mtime ≥ ts 的条目
        #[arg(long)]
        since: Option<i64>,
        /// 以某个 scan 探针文件为源
        #[arg(long)]
        from_scan: Option<String>,
        /// 只含某任务产生的探针
        #[arg(long)]
        job_id: Option<String>,
        /// 只打包含该标签的条目（第 10 批；可重复；默认不过滤）
        #[arg(long = "with-tag", value_name = "TAG")]
        with_tag: Vec<String>,
        /// 排除含该标签的条目（第 10 批；可重复）
        #[arg(long = "without-tag", value_name = "TAG")]
        without_tag: Vec<String>,
        /// 只算数量不生成包
        #[arg(long)]
        dry_run: bool,
    },
    /// 列包内清单（不解压）
    List {
        /// 包路径
        archive: PathBuf,
    },
    /// 解包到指定目录（默认 07_勘误工坊/probe_unpack/）
    Unpack {
        /// 包路径
        archive: PathBuf,
        /// 目标目录
        #[arg(long, value_name = "DIR")]
        to: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
pub enum SelfCheckCmd {
    /// 运行全套自检
    Run {
        /// 安全自动修复：仅创建缺失目录（不 ALTER 表/不删任务）
        #[arg(long)]
        fix: bool,
        /// 缓存"旧"的定义天数
        #[arg(long, default_value_t = 30)]
        older_than: u64,
    },
}

#[derive(Subcommand, Debug)]
pub enum AuditExportCmd {
    /// 生成一份新报告并落盘（--dry-run 只算不落地）
    New {
        /// 报告格式
        #[arg(long, default_value = "minimal-md")]
        format: String,
        /// 只算不落地
        #[arg(long)]
        dry_run: bool,
        /// 只导出语义脏点（is_semantic=1）
        #[arg(long)]
        only_semantic: bool,
        /// 只导出结构脏点（is_semantic=0）
        #[arg(long)]
        only_structural: bool,
        /// 只导出某法理依据哈希（law#<hash>）的条目
        #[arg(long)]
        law_hash: Option<String>,
        /// 只导出某任务产生的风险（source 含该 job id）
        #[arg(long)]
        job_id: Option<String>,
        /// 零风险时不报错（跳过空报告）
        #[arg(long)]
        ignore_empty: bool,
        /// 只导出含该标签条目的风险（第 10 批；可重复；默认不过滤）
        #[arg(long = "with-tag", value_name = "TAG")]
        with_tag: Vec<String>,
        /// 排除含该标签条目的风险（第 10 批；可重复）
        #[arg(long = "without-tag", value_name = "TAG")]
        without_tag: Vec<String>,
        /// 绑定版本快照锚点（第 11 批）：报告记录「基于世界哪一版」
        #[arg(long = "snap", value_name = "SNAP_ID")]
        snap: Option<String>,
    },
    /// 列出已有报告（只列文件名 + 时间，不读内容）
    List,
    /// 打印已有报告内容
    Show {
        /// 报告 run_id（见 list）
        run_id: String,
        /// 报告格式（决定查找的扩展名）
        #[arg(long, default_value = "minimal-md")]
        format: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum QueueCmd {
    /// 列出当前内存任务 ID/状态/类型
    List,
    /// 清空内存队列（仅内存，不碰磁盘库）
    Clear,
    /// 查看单个任务详情（含探针路径）
    Peek {
        /// 任务 id
        id: String,
    },
    /// 向前推进一步（需 --persist 的持久任务；**不自动轮询**，一次一推）
    Step {
        /// 最多推进 N 个 ReadyForInferring 任务（默认 1）
        #[arg(long, default_value_t = 1)]
        limit: usize,
        /// 单轮最大推进上限（防手滑一口气推几百个；默认 20）
        #[arg(long, default_value_t = 20)]
        max_per_round: usize,
        /// 使用持久层（novel.db.task_jobs）；缺省则对内存队列操作
        #[arg(long)]
        persist: bool,
        /// 清理超过 7 天的旧持久任务（安全清理，只动 task_jobs）
        #[arg(long)]
        prune: bool,
        /// 导出长期 Pending（>24h）任务清单为 JSON（只读排查死任务）
        #[arg(long)]
        dump_stuck: bool,
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

#[cfg(test)]
mod pipeline_alias_tests {
    use super::*;
    use clap::Parser;

    /// 第 18 批：规约适配别名 `sys-check full` 可解析。
    #[test]
    fn sys_check_full_parses() {
        let cli = Cli::try_parse_from(["ne-agent", "sys-check", "full"]).expect("sys-check full 应可解析");
        assert!(matches!(cli.cmd, Cmd::SysCheck { ref mode, .. } if mode == "full"));
    }

    /// `sys-check` 缺省 mode 也是 full（规约只要求 full）。
    #[test]
    fn sys_check_defaults_full() {
        let cli = Cli::try_parse_from(["ne-agent", "sys-check"]).unwrap();
        assert!(matches!(cli.cmd, Cmd::SysCheck { ref mode, .. } if mode == "full"));
    }

    /// `pipeline scan-auto --ai-guided`（规约原写法）可解析。
    #[test]
    fn pipeline_scan_auto_parses() {
        let cli = Cli::try_parse_from(["ne-agent", "pipeline", "scan-auto", "--ai-guided", "--dry-run"])
            .expect("pipeline scan-auto 应可解析");
        match cli.cmd {
            Cmd::Pipeline { cmd: PipelineCmd::ScanAuto { ai_guided, dry_run, ai_run, .. } } => {
                assert!(ai_guided && dry_run && !ai_run);
            }
            _ => panic!("应解析为 Pipeline::ScanAuto"),
        }
    }

    /// `pipeline world-analyze`（规约命令面）可解析。
    #[test]
    fn pipeline_world_analyze_parses() {
        let cli = Cli::try_parse_from(["ne-agent", "pipeline", "world-analyze", "--slice", "1/8"])
            .expect("pipeline world-analyze 应可解析");
        assert!(matches!(cli.cmd, Cmd::Pipeline { cmd: PipelineCmd::WorldAnalyze { .. } }));
    }

    /// `mcp probe-all --json` 的 probe-all 子命令可解析。
    #[test]
    fn mcp_probe_all_parses() {
        let cli = Cli::try_parse_from(["ne-agent", "--json", "mcp", "probe-all"]).unwrap();
        assert!(matches!(cli.cmd, Cmd::Mcp { cmd: McpCmd::ProbeAll { .. } }));
        let cli2 = Cli::try_parse_from(["ne-agent", "mcp", "probe-all", "--call-test"]).unwrap();
        assert!(matches!(cli2.cmd, Cmd::Mcp { cmd: McpCmd::ProbeAll { call_test: true } }));
    }

    /// `world-gov sandbox spawn --from-snap X --ai-sandbox --dry-run` 可解析（规约 daemon）。
    #[test]
    fn sandbox_spawn_ai_flag_parses() {
        let cli = Cli::try_parse_from([
            "ne-agent", "world-gov", "sandbox", "spawn",
            "--from-snap", "abc123", "--ai-sandbox", "--dry-run",
        ])
        .expect("sandbox spawn --ai-sandbox 应可解析");
        match cli.cmd {
            Cmd::WorldGov { cmd: WorldGovCmd::Sandbox { cmd: SandboxCmd::Spawn { ai_sandbox, dry_run, .. }, .. } } => {
                assert!(ai_sandbox && dry_run);
            }
            _ => panic!("应解析为 Sandbox::Spawn"),
        }
    }

    /// `world-gov web serve --health-check-only` 可解析。
    #[test]
    fn web_serve_health_check_parses() {
        let cli = Cli::try_parse_from([
            "ne-agent", "world-gov", "web", "serve", "--port", "8270", "--health-check-only",
        ])
        .expect("web serve --health-check-only 应可解析");
        match cli.cmd {
            Cmd::WorldGov { cmd: WorldGovCmd::Web { cmd: WebCmd::Serve { health_check_only, .. }, .. } } => {
                assert!(health_check_only);
            }
            _ => panic!("应解析为 Web::Serve"),
        }
    }

    /// 第 18 批补齐：**顶层** `snap create --label X --json` 可解析（对齐规约）。
    #[test]
    fn top_snap_create_parses() {
        let cli = Cli::try_parse_from(["ne-agent", "snap", "create", "--label", "verify-base", "--json"])
            .expect("顶层 snap create 应可解析");
        match cli.cmd {
            Cmd::Snap { cmd: DiffSnapCmd::Create { ref label }, .. } => {
                assert_eq!(label.as_deref(), Some("verify-base"));
            }
            _ => panic!("应解析为 Cmd::Snap(DiffSnapCmd::Create)"),
        }
    }

    /// 顶层 `snap list` / `snap delete <id>` 也可用（同 world-gov diff snap 子命令）。
    #[test]
    fn top_snap_list_delete_parse() {
        let cli = Cli::try_parse_from(["ne-agent", "snap", "list"]).unwrap();
        assert!(matches!(cli.cmd, Cmd::Snap { cmd: DiffSnapCmd::List, .. }));
        let cli2 = Cli::try_parse_from(["ne-agent", "snap", "delete", "abc"]).unwrap();
        assert!(matches!(cli2.cmd, Cmd::Snap { cmd: DiffSnapCmd::Delete { .. }, .. }));
    }

    /// 顶层 `web serve --port 8270 --health-check-only` 可解析（对齐规约）。
    #[test]
    fn top_web_serve_health_check_parses() {
        let cli = Cli::try_parse_from(["ne-agent", "web", "serve", "--port", "8270", "--health-check-only"])
            .expect("顶层 web serve 应可解析");
        match cli.cmd {
            Cmd::Web { cmd: WebCmd::Serve { health_check_only, port, .. }, .. } => {
                assert!(health_check_only);
                assert_eq!(port, 8270);
            }
            _ => panic!("应解析为 Cmd::Web(WebCmd::Serve)"),
        }
    }

    /// 顶层 `sandbox spawn --from-snap X --ai-sandbox --dry-run` 可解析（对齐规约）。
    #[test]
    fn top_sandbox_spawn_ai_parses() {
        let cli = Cli::try_parse_from([
            "ne-agent", "sandbox", "spawn", "--from-snap", "abc123", "--ai-sandbox", "--dry-run",
        ])
        .expect("顶层 sandbox spawn 应可解析");
        match cli.cmd {
            Cmd::Sandbox { cmd: SandboxCmd::Spawn { ai_sandbox, dry_run, .. }, .. } => {
                assert!(ai_sandbox && dry_run);
            }
            _ => panic!("应解析为 Cmd::Sandbox(SandboxCmd::Spawn)"),
        }
    }

    /// 顶层 `sandbox run --quick warn` 等其他子命令也复用同一枚举。
    #[test]
    fn top_sandbox_run_parses() {
        let cli = Cli::try_parse_from(["ne-agent", "sandbox", "run", "--quick", "warn"]).unwrap();
        assert!(matches!(cli.cmd, Cmd::Sandbox { cmd: SandboxCmd::Run { .. }, .. }));
    }
}
