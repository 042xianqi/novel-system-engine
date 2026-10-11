# OMP（Oh-My-Pi）逻辑参考笔记 → Rust 映射

> 目的：**学 oh-my-pi 的逻辑，用 Rust 落到现有 `ne-*` 代码树上。**
> 纪律：只记模式/数据流/签名，**不贴源码、不引 OMP 为依赖、不动 funnel / ne-check / 剧情库**。
> 参考源码：`/workspace/ref-omp/oh-my-pi/`（只读，296M，TypeScript+Rust）。
> 时间：2026-10-11。

---

## 0. 先纠正一个前提（重要）

| 你以为 | 实际 |
|---|---|
| OMP 是纯 Rust 单二进制 | 上游 `can1357/oh-my-pi` 是 **TS monorepo + Rust 性能原语**；纯 Rust 单二进制的是你自己的 `042xianqi/omp-rs` |
| `crates/omp-core / omp-tasks / omp-memory / omp-cli` | **三个仓里全部 0 命中**，不存在 |
| `src/vault/`、`config/builtins` | 不存在。`.omp/` 是本仓自用 commands/skills/tools；`crates/pi-builtins` 是 **shell 内建命令** |

**真实分层（上游）**：`packages/ai`（多provider LLM）→ `packages/agent`（agent 循环内核）→ `packages/coding-agent`（应用层：tasks/tools/prompts/session/memory）→ `packages/tui`。

---

## 1. Agent 主循环（`packages/agent/src/agent-loop.ts`）

**入口**：`agentLoop(prompts, context, config, signal, streamFn) -> EventStream<AgentEvent, AgentMessage[]>`（:620）
**核心**：`runLoop`（:1294）**双层 while + 事件流**。

```
while (true) {                                   // 外层：一次「用户请求」的完整推进
  let hasMoreToolCalls = true;
  while (hasMoreToolCalls || pendingMessages.length > 0) {   // 内层：工具轮 / 插话轮
     deadline 检查 → pause gate → 塞入 pendingMessages（steering）到 context
     prepareProviderCall(context)                 // 组装 provider 请求
     beforeModelCall(context) 门（可 stop）
     ⟶ 调模型（流式）→ assistant message
     if stopReason ∈ {error, aborted}: 为每个 toolCall 造 placeholder 结果，收尾 return   (:1520)
     toolCalls = content.filter(type==toolCall)   // 过滤已被上游执行的
     hasMoreToolCalls = (stopReason==toolUse||stop) && toolCalls.length>0              (:1587)
     if hasMoreToolCalls: executeToolCalls(...)  // 并发门见 §2
     回注 toolResults 到 context
     emitTurnEnd({ willContinue: hasMoreToolCalls && !deadline })                      (:1736)
  }
  // 外层收尾：drain asides / followUp / late-steering；都没有 → break  (:1793-1809)
}
```

**关键语义（Rust 要照搬）**：
1. **`stopReason` 决定是否继续**：`toolUse`/`stop` 且本轮有 tool_calls → 继续；`length`(截断) 不执行工具；`error`/`aborted` 造配对占位结果后结束。
2. **tool_use ⇄ tool_result 必须配对**：aborted/error 时给每个 toolCall 补一个「aborted 结果」，否则 provider 报错。（:1004 `AgentToolResult` 的 `isError`/`useless` 语义）
3. **steering**：运行中用户插话不打断工具，攒到「批次边界」再注入；abort 时不消费队列（留给续跑）。
4. **deadline 贯穿**：每个边界都查 `isDeadlineExceeded`，超时优雅收尾而非硬杀。
5. **pause gate**：进程级 `/pause` 在 turn 边界停驻。
6. **可恢复**：`agentLoopContinue`（:682）从已有 context 续跑（重试/打断后恢复）。

**Rust 映射**：`while` + `tokio::select!`（信号/流/超时）。事件用 `tokio::sync::mpsc`（对标 `EventStream`）。`AbortSignal` → `CancellationToken`+`tokio_util::sync`。**不要**用 async 递归。

---

## 2. 工具总线（`Tool` / `AgentTool`）

**签名**（`packages/ai/src/types.ts:1590` + `packages/agent/src/types.ts:1118`）：
```ts
interface Tool<P> { name; description; parameters: P; strict?; examples?; }
interface AgentTool<P,D> extends Tool<P> {
  label: string;
  execute(toolCallId, params, signal?, onUpdate?, ctx?) => Promise<AgentToolResult<D>>;
  approval?: ToolApproval;        // read|write|exec，或 {tier,reason,policy}
  concurrency?: "shared"|"exclusive"|(args)=>...;   // 同轮多调用的调度
  loadMode?: "essential"|"discoverable";            // 是否常驻顶层 schema
  interruptible?: boolean;        // 能否被 steering 中断
  hidden?; summary?; docTopics?(); intent?; ...
}
interface AgentToolResult<D> { content: (Text|Image)[]; details?: D; isError?; useless?; }
```

**注册**：`normalizeTools()`（`agent-loop.ts:1008`）把工具列表转成 provider 的 `Context["tools"]`。**编译期静态**（代码里 import 后注册），不是运行时加载。

**执行链**：`executeToolCalls()`（:3190）
- 入参：context / assistantMessage / signal / config / 等
- 每个 call → 找工具 → `run` → `ToolResultMessage` 回注
- **未知工具/参数非法 → 可读错误结果，不炸循环**（`createAbortedToolResult` / `toolNotAvailable` 语义）
- 权限：`approval` tier 决定 auto-approve / prompt / deny

**并发门**（:3775-3796）：
```
concurrency=="shared"    → 塞进 sharedTasks，可并行
concurrency=="exclusive" → 等前面全部完成（Promise.all([lastExclusive, ...sharedTasks])），独占
最后 Promise.allSettled(tasks)
```

**Rust 映射**：
```rust
#[async_trait]
trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn schema(&self) -> serde_json::Value;           // parameters
    fn tier(&self) -> Tier;                          // read/write/exec
    fn concurrency(&self) -> Concurrency;            // Shared/Exclusive
    async fn execute(&self, call_id: &str, args: Value,
                     cx: &ToolCtx) -> ToolResult;    // ToolResult{content, details, is_error, useless}
}
struct ToolRegistry { defs: Vec<ToolDef>, tools: HashMap<String, Arc<dyn Tool>> }
```
> 注意：`AgentTool` 是**运行时 trait 对象**（非静态泛型），因为要放进 `Vec<AgentTool>`；Rust 用 `Arc<dyn Tool>` 同理。

---

## 3. 任务分发（**没有统一 `trait Task`**）

**真实结构**：`coding-agent` 靠 **mode + 子命令 + agent 配置** 分发，不是 `router.register_task::<T>()`。
- `modes/`：`interactive-mode.ts` / `print-mode.ts` / `rpc` / `acp` —— 运行模式（TUI 交互 / 无头打印 / RPC）
- `task/`：**子代理派发**而非任务类型。核心：
  - `workpool.ts`（667行）：`WorkPoolItem{ id, seq, text, agentId?, keyIndex?, estimatedWeight?, status }` —— 按 key lane 并发派活
  - `structured-subagent.ts`、`executor.ts`、`spawn-policy.ts`、`parallel.ts`、`agents.ts`
- 主会话 vs 子代理：`AgentRegistry` + `MAIN_AGENT_ID`；子代理是**同构 agent 的派生实例**
- CLI：`cli.ts` + `cli/commands/`（命令 → 调 mode/session）

**结论**：OMP 的任务抽象是「**agent 实例 + 配置 + 并发池**」，不是 trait 注册表。
**Rust 映射建议**：不要硬套 `trait Task`。用
```rust
enum Mode { Interactive, Print, Rpc }
struct TaskSpec { kind: TaskKind, agent: AgentConfig, isolation: Isolation }
// NarrativeTask 只是 coding-agent 里的一个「自定义工具/命令」，复用同一个 agent loop
```
如果要 trait，就定义 `trait Command { async fn run(&self, cx: &mut Ctx) -> Result<()> }` 映射 CLI 子命令即可。

---

## 4. 记忆系统

**抽象**（`memory-backend/types.ts`）—— 后端**互斥**，`resolveMemoryBackend(settings)` 返回且仅返回一个：
```ts
type MemoryBackendId = "off" | "local" | "hindsight" | "mnemopi" | "sharpshooter";
interface MemoryBackend {
  start(opts): void|Promise<void>;       // 每 session 一次，MUST 非抛错（失败要吞）
  status(): Promise<MemoryBackendStatus>;
  search(query, opts?): Promise<SearchResult>;   // 召回
  save(input | string): Promise<SaveResult>;     // 固化
  // prepare(): MemoryPromptPreparation  —— recall 结果暂存，随本轮 user message 一起投递
}
```
- **媒体**：`local-backend.ts`（文件/SQLite）、`mnemopi`（向量 `embeddings.ts` + 情景图 `episodic-graph.ts` + 实体 `entities.ts` + `banks.ts`）、`hindsight`（`bank.ts`/`mental-models.ts`/`transcript.ts`）
- **绑定**：`start({ session, settings, modelRegistry, agentDir, taskDepth, parent*SessionState })` → 记忆挂在 **session** 上；`agentDir`（≈ vault 根）决定存储位置
- **注入**：`MemoryPromptPreparation.context` + `notice` 作为隐藏消息随 user turn 投递，`commit()` 在投递校验后落库

**Rust 映射建议**：
```rust
trait MemoryBackend: Send + Sync {
    fn id(&self) -> MemoryBackendId;
    async fn start(&self, opts: &StartOptions) -> anyhow::Result<()>;   // 非抛错语义
    async fn recall(&self, query: &str, limit: usize) -> RecallResult;
    async fn consolidate(&self, turn: &Turn) -> SaveResult;
    async fn status(&self) -> Status;
}
```
> 你的 **L0-L3 分层是自己发明的**，OMP 无对应；`recall/consolidate` 的**动词与数据流**可参考，分层名随你。

---

## 5. 双层 prompt（**真实机制找到了**）

- **内置**：`import systemPromptTemplate from "./prompts/system/system-prompt.md" with { type:"text" }`（`system-prompt.ts:30`）—— 编译期打包。Rust 对应 **`include_str!`**。
- **运行时 override**：`discoverSystemPromptOverride(cwd)`（:256）→ `loadCapability(systemPromptCapability.id, {cwd})`，按 **`project` 级 → `user` 级** 顺序找，支持两种 `kind`：
  - `text`：整段替换；
  - `template`：替换模板骨架（保留变量插值）。
  - 空模板 → 警告并**回退内置**（`logger.warn("...using the bundled prompt")`）。
- **模板语法**：`{{#if X}}...{{/if}}`、`{{#each}}`、`{{var}}`（Handlebars 风格），变量如 `{{toolInventory}}`、`{{intentField}}`、`{{MAX_CONCURRENCY}}`。
- **组装分段**（`system-prompt.md` 内置模板）：`# Engineering` → `# Personality` → `# Skills & Rules` → `# Internal URLs` → `# Tool Inventory` → `# xd:// Tool Devices` → 项目 `AGENTS.md`（`project-prompt.md`）。

**Rust 映射**：
```rust
fn load_system_prompt(cwd: &Path) -> String {
    for level in [Level::Project, Level::User] {         // 项目优先
        if let Some(p) = find_override(cwd, level) {
            if let Ok(s) = fs::read_to_string(&p) { if !s.trim().is_empty() { return s; } }
        }
    }
    include_str!("../prompts/system/system-prompt.md").to_string()   // 回退内置
}
```
> 「每次执行重读」（CLI 短进程）；长驻进程需重启 —— 和 OMP 一致，不做 watch。

---

## 6. 多模型 / Provider（`packages/ai`）

- `api-registry.ts`：`registerCustomApi` / `getCustomApi` —— **可注册自定义 API**（运行时注册表）
- `registry/registry.ts`：`PROVIDER_REGISTRY`（内置 provider 定义数组）
- **模型角色**：`modelRoles` 映射（`config/model-presets.ts`）+ `resolveModelRoleValue`/`resolveModelScope`；预设（preset）可整体切换角色快照
- 多角色（主/辅/摘要）通过 **role → model 映射**，不是硬编码

**Rust 映射**：你已有 `ne-engine::providers`（`models.yaml` + `config.yml` 的 roles 解析）—— 这正是同一模式的 Rust 版，**复用即可，不要重写**。

---

## 7. 配置与项目隔离（对标 vault）

- `~/.omp/agent/config.yml`：`modelRoles`、`task.keyPool`（settings 系统读这里）
- `~/.omp/agent/models.yml`：provider 段
- 项目上下文：`AGENTS.md`（项目提示词）+ `loadContextFiles`（向上走目录收集）+ `workspace-tree`
- **`agentDir`** 是记忆/会话/统计的落盘根 —— 这就是「vault」概念的真实载体（每条 session 绑一个 agentDir）

**Rust 映射**：`Ctx { vault_root: PathBuf, agent_dir: PathBuf, ... }`，session/memory/db 全挂 vault 下。

---

## 8. 关键文件索引（都给了 file:line）

| 关注点 | 文件:行 |
|---|---|
| Agent 入口/事件流 | `packages/agent/src/agent-loop.ts:620` |
| 主循环双层 while | `agent-loop.ts:1294` |
| 工具执行+回注 | `agent-loop.ts:3190` |
| 并发门（shared/exclusive） | `agent-loop.ts:3775-3796` |
| 停止/续跑语义 | `agent-loop.ts:1587`、`:682` |
| 工具类型 | `packages/agent/src/types.ts:1118`、`packages/ai/src/types.ts:1590` |
| Agent 状态 | `packages/agent/src/types.ts:991` |
| system prompt 组装 | `coding-agent/src/system-prompt.ts:637` |
| prompt override | `system-prompt.ts:256` |
| 内置 prompt 模板 | `coding-agent/src/prompts/system/system-prompt.md` |
| 记忆抽象 | `coding-agent/src/memory-backend/types.ts:1-120` |
| 子代理派发 | `coding-agent/src/task/workpool.ts` |
| provider 注册 | `packages/ai/src/registry/registry.ts:33` |
| 模型角色 | `coding-agent/src/config/model-presets.ts` |

---

## 9. 明确不做

- 不复制 OMP 源码 / http 实现 / 内部协议；不把 OMP 作为 cargo 依赖。
- 不推倒 funnel / ne-check / 剧情库。
- 不做动态插件（OMP 也不走）。
- 不强造 `trait Task` 注册表（OMP 本来就没有）；任务=复用同一 agent loop + 不同配置/命令。
- 不抄 `pi-builtins`（那是 shell 内建，与 prompt 无关）。
- 不做 prompt 文件 watch 热重载。

---

## 10. 落到现有 ne-* 树的映射总表

| OMP 概念 | Rust 落地位置 | 现状 |
|---|---|---|
| `packages/ai` provider | `ne-engine::providers` + `llm` + `keypool` | ✅ 已有，复用 |
| agent-loop 双层 while | `ne-agent::core::loop`（新建） | 🆕 |
| `Tool` + `AgentToolResult` | `ne-agent::core::tool`（新建） | 🆕 |
| Tool 并发门 | `ne-agent::core::tool::dispatch` | 🆕 |
| 记忆 `MemoryBackend` | `ne-agent::core::memory`（新建，L0-L3 自定） | 🆕 |
| session/agentDir | `ne-agent::core::session` + `vault` | 🆕 |
| prompt 内置+override | `ne-agent::core::prompt` + `prompts/*.md` + `include_str!` | 🆕 |
| 任务分发 | `ne-agent::tasks`（命令枚举，非 trait 注册表） | 🆕 |
| 小说流水线 | `ne-agent::tasks::narrative` 包 `ne-engine`+`ne-qc` | ♻️ 包装现有 |
| MCP | `ne-mcp` 降为可选层 | ♻️ |
