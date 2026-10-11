# ne-agent 路线图（从"松散脚本"到"单二进制智能体"）

> 主参考：太墟 harness（抄契约，不抄体量）；oh-my-pi 只补「主循环停止语义」与「单二进制打包」。
> 铁律：现有 9 个 `ne-*` crate 一行不改，只被调用。

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

## M3 · 小说任务（把现有资产接进来）

**做什么**
- `task::narrative`：包装现有 `ne-engine` + `ne-qc` + `ne-injection` + `ne-funnel`
  - 流程：细纲 → 注入包 → LLM 生成 → 门禁 → 入库（**不改业务，只包一层**）
- `ne-agent novel --chap-id N` 跑通
- 每章前后 `recall` / `consolidate` 接记忆

**验收**
- `ne-agent novel --chap-id 1` 产出正文并过门禁
- 与原 `ne-engine check-file` 结果一致（口径统一，别再出现 CLI/MCP 不一致那种 bug）

**产出**：**这才是你要的"整体"成立**——小说流水线不再是脚本，是一个主体的一项任务。

---

## M4 · 外部世界

**做什么**
- `tool::web_search` / `tool::web_fetch`：收编已装的 `mcp_websearch`
- `mcp` 模块：把现有 `ne-mcp` 的 6 工具收编为**内置工具**（不再是外部进程）
- 工具按需加载的雏形（长工具说明不常驻）

**验收**
- chat 中能"搜资料 → 引用 → 补充进上下文"
- `ne-agent tool <name>` 可单独调试任一工具

**产出**：它能主动获取外部信息。

---

## M5 · 长大（Runtime 硬骨头）

**做什么**
- `ContextWindowPolicy`：token 估算 + 输出预算（太墟 1074 行，最难的一块）
- `compaction`：超限时摘要下沉
- **prefix cache 契约落实**：工作区段用文件字节戳驱动（不用目录 mtime）
- `session` 树真正用起来：分支 / rewind（`ne-agent session branch / rewind`）

**验收**
- 长对话不崩、token 可控
- rewind 到某轮后重跑，结果自洽
- 相邻两轮 system prompt 字节级一致（除显式事件）

**产出**：从"能用"到"能长跑"。

---

## M6 · 安全与协作

**做什么**
- `approval`：工具三档（normal / high / critical），PLAN 模式硬拦截
- `task_state`：持久化任务状态机 + CAS（转换表单测）
- 协作工具：`plan` / `scratchpad` / `ask_user`
- 按需加载：`load_rule` / `load_skill`

**验收**
- 危险操作被拦，需显式批准
- 进程中断后恢复不误报成功（CAS 测试）
- 规则/技能不常驻也能被取到

**产出**：可以放心交给它做事。

---

## M7 · 完善

**做什么**
- `subagent`：子智能体 Lane（并发 + 隔离）
- 可选：dual agent（Planner/Executor）
- 打包：单二进制 + 跨设备（Android aarch64 / 桌面）
- 架构门禁：crate 边界检查 + 文件行数棘轮（学太墟 `architecture-policy.json`）
- 文档：`docs/` 与代码同步

**验收**
- 拷到别的机器，只要有 yaml + API 可访问就能跑
- 无脚本依赖
- 架构门禁接 CI

**产出**：成品。

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
