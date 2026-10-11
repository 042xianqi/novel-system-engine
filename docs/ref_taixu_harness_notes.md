# 太墟 Harness vs oh-my-pi —— Agent Runtime 对照笔记

> 对象：`wkbin/taixu`（本地 `/workspace/taixu-ref`，v0.21.0+，Kotlin） vs `can1357/oh-my-pi`（TS+Rust）。
> 目的：用 Rust 在现有 `ne-*` 树上落地 Agent Runtime，取两者之长。
> 纪律：只记模式与签名，不贴源码、不引依赖、不动 funnel / ne-check / 剧情库。
> 时间：2026-10-11。

---

## 0. 一句话结论

**太墟 harness 就是你目标的现成参照物**——一个能跑的 Agent Runtime，语言是 Kotlin（比 oh-my-pi 的 TS 更贴近 Rust 的静态类型思维），而且它的**设计决策文档化程度远高于 oh-my-pi**（`docs/` 23 篇 + `pi_docs/` 8 篇）。

但它和你要的「单二进制 CLI」有一个**根本差异**：太墟是 **Android 应用内嵌 harness**（Koin 注入 + Room + DataStore + Compose），不是独立可执行文件。所以：

- **学它的逻辑与分层契约**（可直接映射到 Rust）；
- **不学它的宿主形态**（Android/Koin/Room 依赖）。

---

## 1. 太墟 harness 的真实结构（`harness/src/`，47535 行 Kotlin）

```
harness/
├── HarnessLoop.kt            1724  主循环（多会话并发、steering/queue、审批超时、恢复）
├── HarnessProviderRunner.kt   668  模型回合：能力选择、流式、重试、助手回复结算
├── HarnessToolRoundRunner.kt  393  工具回合：参数校验、单轮限额、执行、审批暂停
├── ToolRoundDispatcher.kt          单回合多工具并发（mutation 互斥 / read-only 4 并发）
├── ToolExecutor.kt           1509  内置工具分派（20 个 HarnessTool）
├── ApprovalPolicyEngine.kt    502  审批三档 normal/high/critical
├── SubagentOrchestrator.kt    841  子智能体 Lane 编排
├── ContextWindowPolicy.kt    1074  上下文窗口 / token 估算 / 输出预算
├── ProviderClient.kt         1744  OpenAI 兼容 + Anthropic + Responses API
├── prompt/
│   ├── SystemPromptBuilder.kt 682  分层组装、prefix cache 契约
│   ├── PromptRouter.kt             L2 规则路由（启发式选块 + load_rule 兜底）
│   ├── MemoryRecallSelector.kt     BM25+CJK bigram 召回、pinned/recall 正交
│   ├── PromptAssetLoader.kt        assets 模板 + {{VAR}} 严格校验
│   ├── PromptVariableResolver.kt
│   ├── DistroCatalog.kt / PrivilegeSectionRenderer.kt
├── task/AgentStateMachine.kt      持久化任务状态机（CAS 转换）
├── session/                       会话树、投影、Lane 管理、回合协调
├── compaction/                    压缩（摘要下沉）
├── checkpoint/                    检查点 / rewind
├── dual/                          双智能体（Planner/Executor）
├── subagent/                      Lane 契约与执行
├── mcp/                           MCP 客户端（http/stdio/oauth）+ in-process server
├── skill/                         技能加载（load_skill）
├── events/ metrics/ diagnostics/ recovery/ queue/ projection/
└── assets/prompts/system/*.md     10 个分层规则块
```

**架构铁律**（`docs/ARCHITECTURE_RULES.md`）：
1. `:core:model` 纯 Kotlin，禁 `android.*`；`architecture-policy.json` 是唯一事实源，`architectureCheck` 接 `preBuild`。
2. 业务层只能依赖 Repository 接口，禁直连 DAO。
3. 文件行数棘轮：>400 行登记 `.architecture-baseline.json`，只许缩减。

> ⚠️ 这三条**对 Rust 项目同样适用**：crate 边界 + `maxFileLines` 门禁 + 模型层纯数据。

---

## 2. 逐项对照：三套 Runtime 的同一件事怎么做

| 关注点 | oh-my-pi (TS) | 太墟 (Kotlin) | **Rust 落地建议** |
|---|---|---|---|
| **主循环** | 双层 while + 事件流（`agent-loop.ts:1294`） | `runLoopRounds` + `RoundBudget` + `turnRunner`（`HarnessLoop.kt:1211`） | 双层 `while` + `tokio::select!`；预算对象 `RoundBudget` |
| **工具抽象** | `Tool`/`AgentTool` 接口 + `execute()` | `enum HarnessTool`（20 个）+ `ToolExecutor` 分派 | **enum + match**（太墟式）优于 trait 对象：静态、可穷举、零动态分发 |
| **工具结果** | `{content,details,isError,useless}` | `ToolResult{success,output,metadata,imageAttachments,...}` | struct，**失败不抛异常**，返回结构化结果让模型自纠 |
| **并发门** | `shared`/`exclusive` + `Promise.allSettled` | mutation 互斥 / read-only 4 并发 | `Semaphore` + 互斥标记 |
| **任务** | 无 `trait Task`；mode + workpool | **`AgentStateMachine`** 持久化状态机（QUEUED→RUNNING→…） | 状态机 + CAS 转换表（`AgentTaskTransitionPolicy` 是纯函数，可单测） |
| **审批** | `approval` tier | 三档 normal/high/critical + `ApprovalPolicyEngine` | `enum Tier` + 策略表；PLAN 模式硬拦截 |
| **prompt 双层** | `import ... with {type:"text"}` + override | **assets/*.md + `{{VAR}}` 严格校验**（`PromptAssetLoader` 缺变量直接 `check` 失败） | `include_str!` + 手写 `{{VAR}}` 渲染（缺变量报错，不静默） |
| **prompt 分层** | system-prompt.md 模板 + 变量 | **L0(core/tools) 常驻 + L1(proot/权限) 常驻 + L2(按任务路由)** | 分层拼接；L2 用规则路由，未命中可由 `load_rule` 自取 |
| **记忆** | `MemoryBackend` 互斥多后端 | **pinned（system 稳定前缀） + recall（用户轮前缀，BM25）正交分层** | 见 §3，这是太墟最值得抄的设计 |
| **session** | messages + agentDir | 会话树 + Lane + 回合协调 + 检查点/rewind | 会话树（支持分支/rewind） |
| **状态持久化** | 文件 | Room（DAO ← Repository 接口隔离） | SQLite（你已有 `ne-db`） |
| **隔离** | `agentDir` | workspace + projectType 检测 | vault 根 + 项目类型检测 |

---

## 3. 太墟最值得抄的三个设计（oh-my-pi 没有或更弱）

### 3.1 prefix cache 契约 —— prompt 稳定性铁律

`SystemPromptBuilder.build()` 的核心契约（注释里写得极清楚）：

> **system prompt 不含逐轮变化的字节**。记忆召回与任务计划看板都外移为「用户轮前缀块」，每用户轮只算一次并持久化，轮内冻结。相邻两轮 system prompt 字节级一致，除非发生：用户显式事件、计划首次出现/转非 active、PLAN↔BUILD 切换、压缩。

**为什么重要**：system prompt 任何逐轮变化都会**击穿其后全部对话的 KV cache**。这是真金白银的 token/延迟成本。

**Rust 落地**：
- system prompt 只放**稳定段**（人格/工具说明/规则/技能目录）。
- 逐轮变的（记忆召回、计划看板）→ 挂到**最新 user 轮的文本前缀**，且**持久化冻结**。
- 工作区段用 `WORKSPACE_PROMPT_STAMP_FILES` 的**文件字节戳**驱动，**不含目录 mtime**（否则模型每建个文件就重算整段）。
- 规则路由命中**全会话累计**（一旦命中常驻），不逐轮重算。

### 3.2 pinned / recall 正交分层

- **pinned**：常驻 system prompt 稳定前缀，最高权威，「始终遵循」。
- **recall**：BM25 按需召回，挂 user 轮，**低权威声明**包裹（`<recalled_memory>...低权威背景资料...`）。
- 检索：**BM25 + CJK 字符 bigram**（真词重叠，不是散字 LIKE）；`project` 加权 1.2、`session` 0.9；按 `updatedAt` 年龄降权（降权不删除）。
- **泛化轮次抑制**：`"继续"/"ok"/"好的"` 不触发召回（避免噪声）。
- **无命中不注入**（对比旧方案"无命中就注入最近记忆"= 噪声 + token 浪费）。

**Rust 落地**：这正是你 harness 里 `recalled_memory` 那段的来源！BM25 纯函数可直接移植（几十行）。

### 3.3 持久化任务状态机 + CAS

`AgentStateMachine` + `AgentTaskTransitionPolicy`：
- 状态：`QUEUED → RUNNING → {WAITING_APPROVAL, RECOVERING, SUSPENDED, FAILED, COMPLETED, CANCELLED}`
- 转换表是**纯函数对象**（可穷举单测），非法转换直接 `require` 失败。
- 持久化用**CAS**（`expectedStatuses` → `nextStatus`），stale 转换被拒并记日志 → **进程恢复不会误报成功**。
- `checkpoint(id, round, maxRounds, progress)` 每轮落进度。

**Rust 落地**：`enum TaskStatus` + `fn is_allowed(from,to)->bool` + `UPDATE ... WHERE status IN (...)`（ne-db 已有事务）。

---

## 4. 太墟的工具清单（20 个，可作你的起点参考）

```
read write edit base process host download
memory plan scratchpad history_search history_read
build_script invoke_subagent mcp load_rule load_skill
compress ask_user render_surface
```
对照你 harness 现状：`read/write/edit/base/process/host/download` 你都有；你要新增的是 `memory / plan / scratchpad / load_rule / load_skill / invoke_subagent / compress / ask_user / render_surface`。

**关键**：`load_rule` / `load_skill` 是「**按需自取**」机制——L2 规则和技能正文不常驻，模型需要时自己拉。这解决了「提示词太长」与「模型看不到规则」的两难。

---

## 5. 太墟 vs oh-my-pi：该跟谁学什么

| 学什么 | 跟谁学 |
|---|---|
| 主循环骨架、停止语义、tool_use/result 配对 | **oh-my-pi**（更细，4000 行 loop） |
| prompt prefix cache 契约、pinned/recall、L0/L1/L2 分层 | **太墟**（文档化最全） |
| 持久化任务状态机 + CAS | **太墟** |
| 工具按需加载（load_rule/load_skill） | **太墟** |
| 会话树 / rewind / 检查点 | **太墟** |
| 多后端 provider / 模型角色 | 两者皆有；你已有 `ne-engine::providers` |
| 子智能体 Lane 并发 | **太墟**（SubagentOrchestrator + Lane 契约） |
| 压缩 / compaction | **太墟**（CompactionSummarizer + anchor 解析） |

**都不学的**：oh-my-pi 的 TS 工程化（Bazel/Dockerfile.robomp 等）；太墟的 Android 宿主层（Koin/Room/Compose/PRoot/PTY）。

---

## 6. 对现有 ne-* 树的具体映射

| 太墟/oh-my-pi 概念 | Rust 落地 | 现状 |
|---|---|---|
| ProviderClient + 模型角色 | `ne-engine::providers`/`llm`/`keypool` | ✅ 复用 |
| HarnessLoop 主循环 | `ne-agent::core::r#loop` | 🆕 |
| enum HarnessTool + ToolExecutor | `ne-agent::core::tool::{Tool, ToolExecutor}` | 🆕 |
| ToolRoundDispatcher | `ne-agent::core::tool::dispatch` | 🆕 |
| ApprovalPolicyEngine | `ne-agent::core::approval`（Tier 三档） | 🆕 |
| SystemPromptBuilder + PromptRouter | `ne-agent::core::prompt`（include_str! + 分层） | 🆕 |
| MemoryRecallSelector（BM25） | `ne-agent::core::memory::recall` | 🆕 |
| pinned 记忆 | `ne-agent::core::memory::pinned` | 🆕 |
| AgentStateMachine | `ne-agent::core::task_state`（+ 转换表单测） | 🆕 |
| SessionTreeStore | `ne-agent::core::session` | 🆕 |
| ContextWindowPolicy | `ne-agent::core::context`（token 估算+压缩） | 🆕 |
| 小说流水线 | `ne-agent::tasks::narrative` 包 `ne-engine`+`ne-qc` | ♻️ |

---

## 8. M5 上下文预算契约（`ContextWindowPolicy.kt` 1074 行，抄契约不抄体量）

**常量（太墟原值）**
```
RESERVED_OUTPUT_TOKENS   = 8192      预留输出
TOOL_SCHEMA_RESERVE_TOKENS = 7200    tools 数组不在这批 messages 里，必须显式预留
MAX_SYSTEM_PROMPT_FRACTION = 0.60    system 最多占窗口 60%
MIN_SYSTEM_PROMPT_TOKENS = 512
KEEP_RECENT_TOOL_RESULTS = 4         折叠时保护最近 N 条工具结果
DEFAULT_FOLDING_RATIO_PERCENT = 100  折叠线 = (窗口 - 预留输出 - 工具schema预留) * 比例
```

**token 估算（关键：CJK 单独分桶）**
```kotlin
cjk/1.8 + ascii/2.5 + punctuation/2.8，下限 1
// 全角区 0x2E80..0x9FFF、韩文 0xAC00..0xD7AF、全角标点 0xFF00..0xFFEF 全按 CJK 价
// 理由：中文标点实际 ~1 token/字，按 ASCII 标点 /2.8 会系统性低估中文上下文
```

**老轮次工具结果截断（`truncateStaleToolResults`）—— 预算线之前就生效**
- **受保护**：最后一条 user 消息之后的全部结果（当前工作轮）∪ 最近 4 条结果
- 其余超过工具类型阈值的，按 `compactToolOutput` 压缩 + 附 `history_read(message_id=...)` 指针
- **不改消息条数/顺序**（NATIVE 协议下丢消息会产生非法 transcript），只替换正文
- 理由：一轮 2.7KB 快照随对话累积，既烧 token 又稀释注意力
- `projectForUsage`：面板估算必须与实际发送同口径（实测未截断时虚高 3 倍）

**给 ne-agent 的落地**（`ContextWindowPolicy` 同类，放 `ne-agent/src/context.rs`）：
- `estimate_tokens(text) -> usize`（CJK 三桶）
- `folding_limit(window, reserve_output, tool_schema_reserve) -> usize`
- `truncate_stale_tool_results(msgs, keep_recent=4) -> Vec<Msg>`
- 与 `runtime.yaml` 的 `context.max_prompt_tokens / reserve_tokens` 对齐

---

## 9. 一个必须做的决策

太墟的 harness 复杂度很高（47535 行 Kotlin，含 dual agent / speculation / checkpoint / rewind / workflow / browser CDP）。**建议分两期**：

- **v1（最小可用内核）**：主循环 + 工具总线（read/write/edit/base/process）+ prompt 分层 + pinned/recall + 会话 + vault + 小说任务。目标：`ne-agent novel --chap-id N` 跑通。
- **v2（增强）**：审批三档、协作工具（plan/scratchpad/ask_user）、load_rule/load_skill 按需加载、会话树/rewind、compaction、子智能体。

**不要 v1 就上 dual agent / speculation / browser CDP** —— 那些是太墟特定宿主需求（Android 逆向/浏览器自动化），与小说生成无关。
