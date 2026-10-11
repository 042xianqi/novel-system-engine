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
