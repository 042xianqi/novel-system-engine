# ne-agent 路线图（从"松散脚本"到"单二进制智能体"）

> 主参考：太墟 harness（抄契约，不抄体量）；oh-my-pi 只补「主循环停止语义」与「单二进制打包」。
> 铁律：现有 9 个 `ne-*` crate **不动业务语义**，只被调用。
>
> **2026-10-11 修订（M10）**：原措辞是"一行不改"。真机验证暴露 `ne-engine::llm`
> 作为**通用 LLM 调用层**缺 `tools` 通道，导致工具调用在真实模型上完全不工作（见 M10）。
> 经用户指示"不知道的问题参考太墟"，按太墟 `ProviderClient` 契约对 `ne-engine::llm`
> 做**向后兼容的功能扩展**（加字段/加解析，不改既有行为）。修订后的铁律：
> **不改业务语义与既有行为；允许为补齐通用基础设施（如 LLM 调用层的 tools 通道）而扩展。**

---

## 全局原则（贯穿始终，不随阶段变）

1. **一个入口**：所有能力最终只从 `ne-agent <子命令>` 进。
2. **一个库**：一个项目 = 一个 vault 目录（`runtime.yaml` / `models.yaml` / `agent.db` / `workspace/`）。
3. **两个不可逆决定必须在 M1 定死**：会话存储结构（树）、prompt 分层边界（system 稳定 / 逐轮挂 user 轮）。
4. **契约优先**：工具返回结构化结果不抛异常；prompt 缺变量报错不静默；任务状态机转换表单测。
5. **每阶段可独立验收**：跑得起来 + 有测试，才算完成。

---

## M0 · 地基（当前所在）

**做什么**
- `ne-agent` crate 建骨架（`Cargo.toml` / `lib.rs` / `main.rs`）
- `vault` 模块：目录创建、`runtime.yaml` + `models.yml` + `config.yml` 读写
- `config` 模块：复用 `ne-engine::providers` 解析 provider + 模型角色

**验收**
- `ne-agent vault init` 能建出完整 vault 目录
- `ne-agent models list` 能列出现有 provider 与角色（读你熟悉的 models.yml 格式）
- `cargo test` 绿

**产出**：能"认出自己"，还不能对话。

### ✅ 已完成（2026-10-11）

```
crates/ne-agent/
├── Cargo.toml
└── src/
    ├── lib.rs      （模块导出）
    ├── vault.rs    （Vault：init/show/llm_dir/is_placeholder_file  + 4 单测）
    ├── config.rs   （RuntimeConfig：runtime.yaml + 委托 ne-engine  + 5 单测）
    └── main.rs     （CLI：vault init/show/import-llm、models list/check）
```

- **零 tokio**，与现有 crate 一致；**零 omp 依赖**。
- 子命令可用：`vault init|show|import-llm`、`models list|check`；支持 `--vault`/`--json`/`$NE_AGENT_VAULT`。
- 真机验收 4 条路径全对：空 vault+`$TAIXU_LLM_DIR` → 解析成功；空 vault 无环境变量 → 回退全局；源为空时 `import-llm` 给 ⚠；vault 自填 → 用 vault。
- 全 workspace：**157 测试 0 失败**（原有 148 未动 + 新增 9）。
- 途中揪出并修掉一个真 bug：空壳 `models.yml` 被当成有效配置，静默挡住了 `$TAIXU_LLM_DIR` 与全局配置（已加回归测试）。


---

## M1 · 它自己（第一刀，最关键）— ✅ 已完成（2026-10-11）

**做什么**
- `session`：会话表（**树结构**：`id / parent_id / role / content / created_at / token_usage`）+ 持久化 + 恢复
- `prompt`：分层组装 —— system 段（稳定）+ 逐轮块（挂 user 轮）+ `{{VAR}}` 严格校验
- `harness::loop`：主循环（双层，含停止语义与 tool_call⇄result 配对）
- `tool`：`enum Tool` + 结构化 `ToolResult`，先内置 `read/write/edit/ls/base`
- `task::chat`：`ne-agent chat` 交互式对话，**重开能接着上次聊**

### ✅ 交付

```
crates/ne-agent/src/
├── session.rs   会话树（parent_id）+ 持久化 + 按 head 回溯分支历史   + 4 单测
├── prompt.rs    分层组装 + 双层资源(include_str! + override) + {{VAR}} + 8 单测
├── tool.rs      enum Tool + 结构化 ToolResult + 路径沙箱      + 8 单测
├── harness.rs   主循环 + 停止语义 + 出口 sanitize + 配对      + 10 单测
├── task.rs      chat 任务（交互 / 单次）
├── main.rs      +chat / session / tools 子命令
└── prompts/     root_system.md · chat_rules.md（编译内嵌）
```

**两个不可逆决定已定死**：
1. 会话存**树**（`parent_id`）；`history()` 沿 head 回溯，分叉时只取 head 路径（有测试）。
2. system prompt 只放**稳定字节**；逐轮内容走 `TurnPrefix` 挂最新 user 轮（有测试断言 system 不受影响）。

**修正（对照太墟后）**：初版想用「输出长度是否顶到 max_tokens」猜 `finish_reason` 决定要不要执行工具。
查太墟后确认**这是错的**——太墟的 OpenAI 路径只看 `toolCalls.isNotEmpty()`（`HarnessLoop.kt:1309`），
根本不依赖 finish_reason。已改为同策略。

**新增出口防线**（学太墟 `sanitizeApiTranscript:711`）：内部把工具结果记为 `role=tool`，
发送前统一转写为 user 文本——因为文本协议下没有结构化 `tool_calls` 与之配对，
直接发 role=tool 会被真实服务端 400 拒绝。

**真机验收**（非单测）：
- 假 LLM 服务端跑通：普通对话 / 工具调用多轮 / `--json` / `session list|show` / `--resume` 续接。
- **严格模式验证**：服务端见到 `role=tool` 即返 400；实跑两轮请求 `hasToolRole:false`，0 次违规。
- 全 workspace **230 测试 0 失败**（原有 192 未动 + 新增 38）。

**修正的 bug**：`{{VAR}}` 渲染按字节索引取 char → 中文乱码（`ä½\u{a0}å¥½`）；已改子串推进 + 回归测试。
`vault init` 默认 `tools.enabled` 含不存在的 `process`（实际叫 `base`）→ chat 一启动即失败；已修 + 回归测试。


---

## M2 · 记忆 — ✅ 已完成（2026-10-11）

**做什么**
- `memory::pinned`：常驻 system 段的稳定记忆（最高权威）
- `memory::recall`：**BM25 + CJK 字符 bigram** 召回（纯函数，逐条对照太墟移植）
- 泛化轮次抑制（"继续"/"ok" 不触发召回）；**无命中不注入**
- 记忆表落 vault

### ✅ 交付（crates/ne-agent/src/memory.rs，13 单测）

逐条对照太墟：
| 太墟 | 本实现 |
|---|---|
| `AgentContextEntities.kt:29` 数据模型 | `Memory{scope,owner_id,kind,key,value,pinned,expires_at,...}` |
| `AgentContextDao.kt:37` `getPinnedMemories` | `MemoryStore::pinned()` —— **排序内容确定** |
| `MemoryRecallSelector.selectRecall` | `select_recall()` —— BM25 + CJK bigram 纯函数 |
| `SystemPromptBuilder.kt:178` pinned 常驻 | `render_pinned_section()` 拼进 system 末尾 |
| `MemoryRecallSelector.renderBlock` | `render_recall_block()` —— XML 包裹 + 低权威声明 |

**抄到的关键细节**（不看太墟想不到）：
- **pinned 排序必须内容确定**（`scope权威序 → createdAt → id`），**不能用 updatedAt**。
  太墟 DAO 注释专门警告：保存会刷新 updatedAt（哪怕内容未改），按它排序会让"模型重复保存
  同一 pinned"重排列表、改变提示词字节、**击穿 prefix cache**。有回归测试 `pinned_order_is_content_deterministic`。
- **pinned 与 recall 正交**：pinned 不参与召回（`candidates` 里 `pinned=0`），避免重复占预算。
- **无命中不注入**：不做"无命中就注入最近记忆"（那是噪声 + token 浪费）。
- **CJK bigram**：让"调用链"命中"调用链路分析"这类真实词重叠，同时避免散字误命中。
- **续期不删除**：`touch()` 只刷新 `last_verified_at`（新鲜度信号）。

**真机验收**：
- `memory save/list/forget/recall` 全部跑通；召回命中相关、无关与泛化轮次均不注入。
- **注入链路抓包验证**：pinned 出现在 system 末尾；recall 的 `<recalled_memory>` 只挂在
  有相关性的 user 轮前缀，无关问题不挂（服务端抓到的真实请求逐条确认）。
- 全 workspace **243 测试 0 失败**。

**CLI**：`memory list [--pinned] | save <key> <value> [--scope --kind --pinned] | forget <key> | recall <query>`


---

## M3 · 小说任务（把现有资产接进来）— ✅ 已完成（2026-10-11）

**做什么**
- `task::narrative`：包装现有 `ne-engine` + `ne-qc` + `ne-injection` + `ne-funnel`
  - 流程：细纲 → 注入包 → LLM 生成 → 门禁 → 入库（**不改业务，只包一层**）
- `ne-agent novel --chap-id N` 跑通

### ✅ 交付 —— 补上了全仓库缺失的半条链

**核心发现**：`ne_engine::budget::build_injection` 与 `ne_engine::chapter::build_prompt`
早已写好，但它们依赖的 `BudgetCtx` / `ChapterCtx` trait **全仓库只有测试里的 mock，
没有真实现** → **半条生成链是断的**（能力在，没人接上）。

`crates/ne-agent/src/novel_ctx.rs`（425 行，7 单测）就是那一半：
- `NovelCtx::open(root)` —— **只读**打开 `novel.db`（`SQLITE_OPEN_READ_ONLY`，与 ne-mcp/RepoCtx 同口径）
- `impl BudgetCtx for NovelCtx`：8 个方法（outline/material/read_lib_file/role_rows/
  char_card_rows/foreshadow_rows/event_rows/prev_tail）
- `impl ChapterCtx for NovelCtx`：4 个方法（injection 委托 budget、prompt_file 库根优先、
  kb_rules 无表如实返空、material_content）
- 兼容 `old_code`/`new_code` 兜底；`02_资料库` 已删 → 卡片走 DB content 回退；
  缺库优雅降级不 panic

`task::novel_chapter()` —— 主干串接（每步用现成能力，不重写业务）：
```
NovelCtx::open → chapter::build_prompt → llm::call → chapter::write_chapter → gates::run_all
```
门禁复用 `ne_engine::ctx::RepoCtx`（它已实现只读 `GateCtx`）→ **与 `ne-engine check-file` 同源，口径一致**。

**CLI**：`ne-agent novel --chap-id N [--volume 卷01] [--lib DIR] [--dry-run] [--prompt-only] [--json]`
（`--lib` 缺省用 vault 的 `workspace/`；`$NE_AGENT_LIB` 可指定）

**真机验收**（对真实资料库副本，非单测）：
- `--prompt-only`：第 1 章 prompt 3789 字，含细纲「目标/在场/阻碍/选择/钩子」——断链已接通。
- 完整生成：细纲 → 注入包 → prompt → 模型 → **落盘**（13501 字节）→ 门禁返回真实判决
  （`REJECT`，P0 `G09_info_density` 4500>4000 硬上限 —— 因假正文是循环文本，判决正确）。
- 全 workspace **250 测试 0 失败**。


---

## M4 · 外部世界 — ✅ 已完成（2026-10-11）

**做什么**
- `web::fetch_text` / `web::search`：网页抓取 + 多引擎搜索
- `mcp` 模块：通用 MCP 客户端（`mcp__<服务>__<工具>` 命名法）

### ✅ 交付

**`crates/ne-agent/src/web.rs`（19 单测）** —— 自写 HTTPS 客户端（零 tokio / 零 reqwest）
- `get()`：跟随重定向（上限 5，每跳重校 scheme）+ 响应体硬上限（2 MiB）+ 超时必设
- `fetch_text()`：抓取 → 去 script/style/注释 → 剥标签 → 解 HTML 实体 → 压空白
- `search()`：**照太墟 `/opt/taixu/scripts/websearch_mcp_server.py`** —— 免 API Key，
  直接取 Baidu/Bing/DDG 结果页正则解析；**首选引擎失败或零结果时自动回退其余引擎**

**`crates/ne-agent/src/mcp.rs`（11 单测 + 5 e2e 测试）** —— MCP 客户端
- 协议：stdio + **newline-delimited JSON-RPC 2.0**（与 ne-mcp 同构）
- **太墟命名法**：工具暴露为 `mcp__<服务>__<工具>`（`HarnessMessage.kt:24` +
  `HarnessToolRoundRunner.kt:351`）——模型直接可调，无需先"选服务"
- `McpRegistry`：**惰性连接**（首次调用才 spawn 子进程）+ 按名缓存；`Drop` 时 kill

**关于「收编 ne-mcp」的判断**：ne-mcp 的 6 个工具（funnel/cardgen/inject/check/db/db_query）
全是**本地小说库操作**，而 M3 的 `novel` 任务已在**进程内直接调用**同样能力。
绕道 MCP 子进程是纯开销 → 不搬。MCP 通道留给**真正的外部服务**（如太墟的 mcp_websearch）。

**关键修正（真机揪出）**：初版 `TcpStream::connect(&str)` 在主机只解析到
**不可达 IPv6** 时挂死 40s+（实测百度/DDG）。改为 `connect_timeout` 逐个尝试
解析出的地址，每个设限，任一成功即用。

**参数透传**：`ToolArgs::to_json_except` —— 值按 JSON 解析（数字/布尔/数组原样），
失败才当字符串，让 MCP 工具收到正确类型而非全字符串。

**CLI**：`mcp list | tools [服务] | call <服务> <工具> '<json>'`

**验收**
- `web_fetch` 抓真实网页 → 200，HTML→文本正确
- MCP e2e：真实 stdio 子进程握手 + 列工具 + 调用 + 工具层路由，5 测试全过
- 全 workspace **277 测试 0 失败**

**产出**：它能主动获取外部信息。

---

## M5 · 长大（Runtime 硬骨头）— 🟡 主体完成（2026-10-11）

**做什么**
- `ContextWindowPolicy`：token 估算 + 输出预算（太墟 1074 行，最难的一块）
- `compaction`：超限时摘要下沉
- **prefix cache 契约落实**：工作区段用文件字节戳驱动（不用目录 mtime）
- `session` 树真正用起来：分支 / rewind（`ne-agent session rewind`）

### ✅ 已交付

**`crates/ne-agent/src/context.rs`（13 单测）** —— 逐条对照太墟 `ContextWindowPolicy.kt`
- `estimate_tokens`：**CJK 单独分桶**（全角标点按 CJK 价）—— 否则系统性低估中文，
  真实请求比估算大一截，悄悄吃掉折叠余量，最终 400/413 溢出
- `truncate_stale_tool_results`：老轮次长工具结果压缩为「头 + 省略 + 尾」，
  **保护当前轮 + 最近 4 条**，**不增删消息**（NATIVE 协议下丢消息 = 非法 transcript）
- `folding_limit(window, reserve_output, tool_schema_reserve)`：折叠线
  （tools 数组不在 messages 里，**必须显式预留** 7200 token）
- `keep_from_index`：按预算丢弃最早**完整轮次**（保 tool_call⇄result 配对），至少留一轮

**接线**：`harness::build_messages` → `truncate_context`（先截断老结果，再预算折叠）
→ `sanitize_for_provider`；用 `runtime.yaml` 的 `context.max_prompt_tokens/reserve_tokens`

**会话树 rewind**（`session.rs`）
- 发现 M1 缺陷：`head = MAX(id)` 在 rewind 后会指错 → 加**显式 `head_id` 列** + 幂等迁移
  （`ensure_column`，老库自动补列）
- `rewind_to(session, msg_id)`：改指针**不删数据**（对齐太墟 `rewindBefore`），
  校验消息属本会话（防跨会话串改）
- `rewind_back(session, n)` / `rewind_last_turn(session)`（撤掉最后一条 user）
- `head_id = -1` 哨兵表示"分支已清空"（撤销首条 user 时）
- CLI：`session rewind [--to <id> | --back <N> | --last-turn]`
- 7 个单测（含跨重开持久化、跨会话拒绝、分叉共存）

**CLI**：`estimate [FILE]`（token 估算调试）

### 待办
- ~~`compaction`（摘要下沉）~~ → **M9 已补**（LLM/机械摘要 + 投影 + 原文可回读）
- prefix cache 字节戳驱动：待确认模型是否支持 prompt caching（太墟是 Anthropic 专属
  `cache_control` 断点，ne-agent 走 OpenAI 兼容接口 → 需按实际 provider 决定）

**产出**：从"能用"到"能长跑"（主体成立）。

---

## M6 · 安全与协作 — ✅ 主体完成（2026-10-11）

**做什么**
- `approval`：工具三档（normal / high / critical），PLAN 模式硬拦截
- `task_state`：持久化任务状态机 + CAS（转换表单测）
- 协作工具：`plan` / `scratchpad` / `ask_user`
- 按需加载：`load_rule` / `load_skill`

### ✅ 已交付

**`crates/ne-agent/src/approval.rs`（~470 行，14 单测 + 4 e2e）** —— 对照太墟 `ApprovalPolicyEngine.kt`
- **审批三档** `ApprovalMode::{Request, Assisted, FullAccess}`（未知值→assisted）
- **正交的只读规划** `plan_block`：`full_access + plan_only` **仍然只读**
  （刻意不复用 `is_routine_command`——它放行 `cargo build` 这类会写产物的命令）
- **决策不依赖模型措辞**（太墟原话："prompt 不参与决策"）
- shell 反混淆（去引号/反斜杠/压空白）+ 危险模式 + 例常白名单 + 安全读管道 +
  「下载 | 交给 shell」模式（`curl <url> | sh`，URL 在中间，固定子串匹配不到）
- **接进 harness 工具循环**：plan_only 硬拒 → 审批 →（**无回调默认拒绝**，安全默认，绝不静默放行）
- CLI：`chat --plan/--approval`；`runtime.yaml` 加 `approval:` 段

**`crates/ne-agent/src/task_state.rs`（~660 行，13 单测）** —— 对照太墟 `AgentStateMachine`
- **纯转移表** `TransitionPolicy`（8 状态，穷举单测，终态无出口）
- **CAS 转移**：`UPDATE ... WHERE id=? AND status IN (...)`，影响行数 0 → 陈旧转移被拒
  （太墟注释：*防进程恢复报出"幻觉的成功"*）
- `create_queued` / `checkpoint`（进度上限 0.99，未完成不显示 100%）/
  `active_for_session`（按状态优先级 running<recovering<waiting_approval<queued）/
  `recoverable`（auto_resume + 重试额度）
- CLI：`task list/new/show/set`

**`crates/ne-agent/src/context_tools.rs`（~380 行，11 单测）** —— 对照太墟 `AgentContextExecutor`
- `plan`：replace_active/get_active/advance/clear_active
- `scratchpad`：save/get/list/delete/clear（限额 128/8192/200）
- **关键教训**（太墟注释）：`plan.status` 是生命周期枚举（active/completed/cancelled），
  模型常塞阶段说明 → `PlanStatus::parse` 失败时**保留原状态**，不写坏记录
  （否则 `getActivePlan` 永久找不到记录）
- 为什么值得有：长任务跨多轮，历史会被折叠（M5），状态就丢了 → 持久化到库不受折叠影响

**工具面**：新增 `Tool::Plan`/`Tool::Scratchpad`（tier=Read，免审，会话局部，plan_only 下也放行
——拦 plan 工具等于自相矛盾）；默认 `tools.enabled` 加入二者

**测试**：全 workspace **346 通过 0 失败**

### 待办（非核心，可后置）
- `ask_user` 工具（需要交互通道，当前 chat 已有 approver 回调，可复用）
- `load_rule`/`load_skill` 按需加载

**产出**：可以放心交给它做事。

---

## M7 · 完善 — ✅ 主体完成（2026-10-11）

**做什么**
- `subagent`：子智能体 Lane（并发 + 隔离）
- 可选：dual agent（Planner/Executor）
- 打包：单二进制 + 跨设备（Android aarch64 / 桌面）
- 架构门禁：crate 边界检查 + 文件行数棘轮（学太墟 `architecture-policy.json`）
- 文档：`docs/` 与代码同步

### ✅ 已交付

**单二进制**
- release 9.2 MB（aarch64），`ldd` 仅 **libc / libm / libgcc**（标准 glibc），
  **无脚本、无 Rust 运行时、无动态框架** → 拷到有 glibc 的机器即可跑
- `Cargo.toml`：`strip=true` + `codegen-units=1`；workspace 版本统一 0.0.7

**架构门禁** `scripts/arch_check.py`（148 行）+ `architecture-policy.json` + `.architecture-baseline.json`
- **crate 依赖白名单**（解析 path 依赖）→ **上线即揪出真实死依赖**
  `ne-engine → ne-funnel`（`[dependencies]` 声明但全仓零使用 → 已删）
- **import 黑名单**：ne-agent 禁 `tokio` / `reqwest`（架构决策：std 阻塞单线程 + 自写 HTTPS）
- **文件行数棘轮**：上限 700，存量超限登记基线，**只许下调不许上涨**
- 用法：`python3 scripts/arch_check.py` / `--sync-baseline`
- 环检测：确认全 crate 依赖图无环

**文件拆分**（过门禁）：`web.rs` 1036 → `web.rs` 414 + `web_html.rs` 275 + `web_search.rs` 382；
用 `pub use` 在原路径重导出，**调用方零改动**；顺带修了 `charset` 检测对 `charset="gbk"` 引号的处理

**子智能体 Lane** `subagent.rs`（312 行，13 单测）+ `task::run_subagents` + `subagent` CLI
- 契约对照太墟 `SubagentArgsParser` + `SubagentTaskSpec`（`McpModels.kt:113`）
- **如实差异**：太墟 Lane 是真并发（Android 宿主多面板）；ne-agent 运行时是
  **std 阻塞单线程**且写小说是长链顺序任务 → 做**顺序 Lane**（不并发）
- **隔离靠独立子会话**：每个子任务一个子会话（`parent_session_id` 指向父），
  父子/兄弟上下文互不可见 —— 父会话不被污染
- **写租约**：`["*"]` 独占不能与任何其它子任务共存；同路径（规范化后）不能重复声明
  ——在顺序模型里是**提前发现配置冲突**（如实标注，非运行时竞争防护）
- 子任务**不带审批回调** → 需审批的操作默认拒绝（安全默认）

**验收**
- ✅ 拷到别的机器，只要有 yaml + API 可访问就能跑（单二进制 + 标准 glibc）
- ✅ 无脚本依赖
- ✅ 架构门禁可接 CI（`python3 scripts/arch_check.py`，退出码即结果）
- `subagent` e2e：假 LLM 跑两子任务，验证顺序执行 + 会话隔离 + 父会话不被污染

### 未做（诚实清单）
- ~~**dual agent（Planner/Executor）**~~ → **M8 已补**（顺序版，见下）
- ~~跨设备交叉编译（Android aarch64）未做~~ → **M8 已补**（见下）

**产出**：成品。

---

## M8 · 补齐旧待办 — ✅ 已完成（2026-10-11）

**做什么**：把 M5/M6/M7 里挂着的四项"旧待办"补完，并把太墟的协作原语按需接进 ne-agent。

### ✅ 交付

**① `ask_user` 工具**（`ask_user.rs`，195 行；对齐太墟 `AskUserModels.kt`）
- 一次 1~4 问，每题可带候选项（label + 说明）+ 允许自定义输入；重复选项拒收
- **不是裸提问**：解析/校验/渲染与"谁提供答案"分离——`ToolExecutor` 只落 pending 请求，
  答案由外部交互回调（[`ToolCtx::asker`]）回填。非交互环境（子智能体 Lane / 脚本）
  明确报错并**交回主智能体**，不静默返回空答案。

**② `load_rule` / `load_skill` 按需加载**（`resources.rs`，372 行；`rules/*.md` 内置三块）
- system prompt 只放**目录元数据**（规则名清单 + 技能名/一句话），正文靠工具主动拉取
  ——与 prompt 分层同一理由：避免正文常驻击穿 KV cache、挤压真实上下文
- 双层资源：vault `<rules|skills>/` override 优先，编译内置回退
- `load_skill` 支持子资源（`references/…`）**越界防护**：拒 `..`/绝对路径/软链逃逸；
  技能名匹配**精确优先、模糊多命中即报错**（对齐太墟，防"选错技能"）

**③ 顺序版 dual agent**（`dual_agent.rs` 650 行 + `dual_agent_tests.rs`；对齐太墟 `harness/dual/`）
- Planner（纯规划人格，`system_override`）→ Executor（隔离子会话真跑工具）→ 精炼汇报回注 → 下一轮
- **如实差异**：太墟 Executor 按 DAG **并发**就绪步骤（`MAX_CONCURRENT_STEPS=3`）；
  ne-agent 运行时是 std 阻塞单线程、写小说是长链顺序任务，故**顺序执行**——保留 DAG 拓扑
  （`dependencies` 就绪判定）、每轮只跑一个就绪步。其余语义全部对齐：REPLAN 保留 COMPLETED、
  FAILED 可重试、级联失败、FINISH 前校验（未完成即打回）。
- 切断 `task → tool → task` 循环：`subagent_runner.rs` 单独承担"从 ToolCtx 重建 HarnessCtx"

**④ Android aarch64 交叉编译**（`scripts/cross-build-android.sh`）
- NDK clang + API24；产 ELF 仅依赖 **bionic 标准库**（libc/libm/libdl），单二进制可跑
- 坑：连字符环境变量（`CC_aarch64-linux-android`）不能 `export`，须用 `env` 传

**架构门禁**：`tool.rs` 900 行（顶格）→ 拆 `tool/{mod,fs,external_io,session_local,tests}.rs`；
`harness.rs`/`dual_agent.rs` 测试外置到 `*_tests.rs` 以免超 700。

**测试**：`tests/m8_tools_e2e.rs`（7 项，走真实工具总线）+ `tests/dual_agent_e2e.rs`（2 项，脚本化假 LLM）
——全绿。全工作区 `cargo test` 32 套件 0 失败；门禁通过。

**产出**：成品（v0.0.6）。

---

## M9 · 上下文可设置 + 压缩 — ✅ 已完成（2026-10-11）

**做什么**（用户原话：「上下文需要可以设置，压缩可以发命令，也可以手动压缩。为以后做 ui 铺垫」）

### ✅ 交付

**① 上下文可设置**（`context` 段扩展 + `ne-agent context` 命令）
- `runtime.yaml` 的 `context` 段新增 `auto_compaction` / `auto_threshold_percent` /
  `keep_recent_tool_results`；`ne-agent context` 查看、`--window/--reserve/--auto-compact/
  --auto-threshold/--keep-recent` 设置，`--json` 供 UI 读。
- **写回保留注释**：只做按键逐行替换，不做整体序列化（后者会抹掉用户手写的注释）。

**② 压缩可发命令**（`compress` 工具 + `ne-agent compress` 命令）
- 契约对齐太墟 `harness/compaction/`：`mode` = `before`（锚点轮之前）/`after`（当前轮之前），
  `anchor` 必须**原样、唯一**地摘自某条用户消息（≥8 字符），越界/歧义/无匹配各有明确报错。
- 摘要两条路：`LlmSummarizer`（模型重写成结构化摘要）/ `MechanicalSummarizer`（降级：保骨架
  不丢用户要点）。模型不可用时**自动降级且如实标注**。

**③ 手动压缩（人可操作）**
- `ne-agent compress`：`--show`（只看投影）/`--clear`（还原原始上下文）/`--mechanical`（离线）。
- **原文永不删除**：压缩只写一条 `compactions` 记录（折叠边界 + 摘要 + 计数 + token 估算），
  `messages` 表保持完整。

**④ 为 UI 铺垫：投影（projection）**
- `compaction::project()` 是"实际上下文"的**唯一读口径**——`build_messages`（发模型）与 CLI/UI
  走同一函数。UI 只要调它就能拿到"折叠了几条、摘要是什么、保留区有哪些、token 用量"。
- 全部 CLI 支持 `--json`，形状即 UI 消费契约（`estTokens`/`foldedCount`/`retained`…）。

**⑤ 压缩后可回读**（`history_read` / `history_search` 工具 + `ne-agent history` 命令）
- 被折叠的原文仍可**按 id 回读**、**按内容模糊搜索**（含跨分支），闭环"压了不丢"。

**模块**：`compaction.rs`（新，投影/摘要/锚点）、`ctx_window.rs`（新，从 harness 拆出的窗口策略
+ 自动压缩判定）、`cmds/cmds_ctx.rs`（新，context/compress/history 命令）；`session.rs` 加
`compactions` 表与 6 个方法；工具 14 → 17。

**测试**：`compaction_tests.rs`（9）+ `tests/m9_compaction_e2e.rs`（6，真实工具总线）。
全工作区 33 套件 0 失败；门禁通过。

**产出**：成品（v0.0.7）。

---

## M10 · 原生工具协议 — ✅ 已完成（2026-10-11）

**背景（真机暴露的头号缺陷）**
用真实端点（`glm-5.3-flash` @ 127.0.0.1:8788）验证时发现：让模型调 `read` 工具，
模型直接回「我没有文件读取工具」——**工具调用在真实模型上完全不工作**。

根因：`harness.rs` 走 v1 **文本协议**（`<tool_call>{json}</tool_call>`），
模块注释写着"不走 provider 的结构化 tools/tool_calls 通道……留到 M4"；
但 M4 做的是 web/MCP，**这件事从没兑现**。`ne-engine::llm::body_json` 不发 `tools`，
`Message` 结构装不下 `tool_calls`。此前"真机验收"用的是**假 LLM 服务端**，
所以 410 测试全绿却从未发生真实工具调用。

**做什么（对齐太墟双通道）**
- `ne-engine::llm`：`ToolCall`（手写 serde，OpenAI 嵌套线格式）、`ToolDefinition`、
  `ToolCallMode{Native,JsonText,Disabled}`；`Message` 加 `tool_calls`/`tool_call_id`
  （可选 + 空时省略序列化 = 向后逐字节兼容）；`body_json` 有工具时注入 `tools`；
  `call()` 解析 `tool_calls`（工具调用轮空正文不再当错误）。
- `ne-agent/src/protocol.rs`（新）：`resolve`（**结构化优先，文本回退**，对齐太墟
  `TextToolCallCodec.resolveCalls`）；`sanitize_for_provider`（**保住 tool_call⇄result 配对**，
  孤立/错位转写 user、缺失补占位，对齐 `sanitizeApiTranscript`）；`builtin_tool_definitions`
  （17 个内建工具 JSON Schema）。
- `session.rs`：`MsgRow.meta` 列 + `append_meta`，持久化工具调用配对，供投影回放。
- `context.rs`/`ctx_window.rs`：`HistMsg` 透传 tool 字段（曾因丢字段导致配对失效）。
- `main.rs`：把内建工具 schema 注入 `llm.tools`。

**验收（真机，非单测）**
- 真机多轮回环：`读 note.txt → 写 summary.txt → 作答`，`summary.txt` 内容 = "沈砚" ✅
- 工具失败路径：读不存在文件，模型如实报"读不到"而非编造 ✅
- 会话续接：下一句还记得上一轮文件名 ✅
- `cargo test --workspace` 414 通过 0 失败；架构门禁通过。

**模块**：`ne-agent/src/protocol.rs`（新）；`ne-engine/src/llm.rs`（扩展）；
`harness.rs`/`session.rs`/`context.rs`/`ctx_window.rs`/`main.rs`。

**代价与注意**：破了原"一行不改"铁律（见文件头修订说明）——`ne-engine::llm` 是
通用 LLM 调用层，本就必须支持 tools 才能承载 agent；扩展是向后兼容的，`ne-*` 既有
业务调用（`novel_chapter` 等）行为不变。

**产出**：v0.0.8。

---

## 阶段依赖与"不要跳"清单

```
M0 → M1 → M2 → M3 → M4 → M5 → M6 → M7
      ↑最关键         ↑"整体"成立点
```

**不要做（明确划掉）**
- PRoot / PTY / Shizuku / Binder 权限（那是太墟当逆向工具的需求）
- browser CDP / 虚拟屏
- dual agent、speculation（M7 之后再说，且很可能永远不需要）
- 动态插件系统（太墟也不走）
- 把 `ne-*` 推倒重写

---

## 时间与精力分配（诚实估计）

| 阶段 | 相对工作量 | 风险 |
|---|---|---|
| M0 | 小 | 低 |
| **M1** | **中** | **中（两个不可逆决定）** |
| M2 | 小 | 低（BM25 是纯函数） |
| M3 | 中 | 中（要接现有口径） |
| M4 | 小 | 低 |
| **M5** | **大** | **高（token/压缩是硬骨头）** |
| M6 | 中 | 中 |
| M7 | 大 | 中 |

**结论**：M1 和 M5 是两道坎。M1 过了就有"整体感"，M5 过了才算"成品"。
