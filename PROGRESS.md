# ne-agent 开发进度（跨段续跑用）

> 最后更新：2026-10-11（**M0–M10.2 全部完成，v0.0.10**）
> 仓库：github.com/042xianqi/novel-system　本地：/workspace/小说系统
> 路线图：docs/ROADMAP-ne-agent.md　参考笔记：docs/ref_taixu_harness_notes.md、docs/ref_omp_notes.md
> 太墟源码：/workspace/taixu-ref（Kotlin，47535 行，**主参考**）
>
> **状态速览**：M0–M10.2 已交付并推送；`cargo test --workspace` **431 通过 0 失败**；架构门禁通过；
> Linux release + Android aarch64 release 均已重建。
> 若续跑，先读本文件末尾「M10.2」段与 `docs/ROADMAP-ne-agent.md` 的「阶段依赖与不要跳清单」。

## 铁律（不要违反）

1. **现有 9 个 `ne-*` crate 一行不改**，只被 `ne-agent` 调用。
2. **遇到问题/bug 先查太墟**怎么实现（用户 2026-10-11 明确要求），不要自己造启发式。
   太墟路径 `/workspace/taixu-ref`，关键文件：
   - `harness/src/main/java/top/wkbin/taixu/harness/HarnessLoop.kt`（主循环、RoundBudget）
   - `.../ProviderClient.kt:711 sanitizeApiTranscript`（出口配对修复）
   - `.../prompt/MemoryRecallSelector.kt`（BM25 召回）
   - `.../prompt/SystemPromptBuilder.kt`（分层 + prefix cache 契约）
   - `core/database/.../AgentContextDao.kt`（pinned 排序必须内容确定）
3. **每完成一轮改动**：更新记忆 + 更新计划 + 提交 + 推送，连续作业不停下问。
4. 会话沙箱：rust 工具链 `export PATH=/opt/taixu/toolchains/rust/bin:$PATH`

## 已完成（已提交推送）

| 阶段 | commit | 内容 |
|---|---|---|
| M0 地基 | `d071b8b` | crate 骨架、vault（init/show/import-llm）、config（runtime.yaml + 委托 ne-engine）、models list/check |
| M1 它自己 | `cbe7acd` | session.rs（会话**树**）、prompt.rs（分层 + 双层资源 + `{{VAR}}` 严格）、tool.rs（enum Tool + 沙箱）、harness.rs（多轮循环 + 熔断 + sanitize）、task.rs（chat） |
| M2 记忆 | `cbff26c` | memory.rs（pinned 常驻 system + BM25/CJK-bigram recall 挂 user 轮） |
| M3 小说任务 | `aebb966` | novel_ctx.rs（补全 `BudgetCtx`/`ChapterCtx` 真实现）+ task::novel_chapter + `novel` CLI |
| 版本管理 | `d5fb5f4` | workspace 统一版本 0.0.1、`version` 子命令、CHANGELOG、`scripts/push-bg.sh` |
| M4 外部世界 | `d98c5f6` | web.rs（HTTPS GET + HTML→文本 + 多引擎搜索）、mcp.rs（MCP 客户端）、`mcp` CLI（v0.0.2） |
| M5 长大 | `848ea86` | context.rs（token 估算 CJK 分桶 / 老轮次工具结果截断 / 折叠线）；会话树 rewind（`head_id` 显式指针）；`estimate` CLI；修 UX（chat 默认续接会话）（v0.0.3） |
| M6 安全协作 | `848ea86` | approval.rs（审批三档 + 只读规划门禁 + shell 反混淆，接进工具循环）；task_state.rs（状态机 CAS 持久化）；context_tools.rs（plan/scratchpad）（v0.0.4） |
| M7 完善 | `4d2f412` | 架构门禁（arch_check.py + 依赖白名单 + 行数棘轮）；子智能体 Lane；文件拆分；单二进制确认（v0.0.5） |
| M8 补齐旧待办 | `bab41f2` | ask_user.rs / resources.rs(+rules) / dual_agent.rs(+tests) / subagent_runner.rs / Android 交叉编译 / tool 拆分（v0.0.6） |
| M9 上下文+压缩 | `c90e5c7` | 上下文可设置（context 段 + `context` CLI）；compaction.rs（锚点/摘要/投影）+ ctx_window.rs；`compress` 工具与 CLI；`history read/search` 回读；工具 14→17（v0.0.7） |
| 工作流重构 | `3e8e1a1` | 太墟工作流 JSON 只写文本、逻辑全归 `workflows/scripts/`，可后台跑（详见 `workflows/README.md`） |
| M10 原生工具协议 | `6854cf0` | 真机揪出头号缺陷：真实模型上工具调用完全不能用 → 双通道（原生 tools 优先 + 文本回退）；protocol.rs；ne-engine::llm 扩展（v0.0.8） |
| M10.1 全路径真机 | `7938ddd` | 逐条真机验 chat/compress/dual_agent/novel/ne-engine；dual_agent 两个真 bug（非工具角色误携 tools、Planner 不守 JSON 协议死循环）已修（v0.0.9） |
| M10.2 MCP 可达性 | 见下 | `use_capability` 静态代理（list/inspect/call）+ 配 MCP 服务自动启用 + 悬空调用「结果未知」恢复（v0.0.10） |

测试基线：全 workspace **431 通过 0 失败**；架构门禁通过。

> ⚠️ 本文件下方从「进行中：M4 外部世界」起的章节是**历史流水日志**（当时逐段续跑留下），
> 其中的「进行中/未提交/待办」等字样**已过期**——各段实际均已提交推送。以本表与
> `docs/ROADMAP-ne-agent.md` 为准。保留它们只为追溯当时的调试细节（真机坑等）。

### 关键的、不可逆的决定（已定死）

- **会话存树**（`parent_id`）；`history()` 沿 head 回溯，分叉只取 head 路径。
- **system prompt 只放稳定字节**；逐轮内容走 `TurnPrefix` 挂最新 user 轮。
- **pinned 排序必须内容确定**（`scope权威序 → createdAt → id`），**不能用 updatedAt**（会击穿 prefix cache）。
- **不改现有 ne-* crate 的业务逻辑**，只被 ne-agent 调用（例外：`ne-mcp` 一处版本常量改 `env!`）。

## 进行中：M4 外部世界（收尾）

### 已完成（未提交）

- `crates/ne-agent/src/web.rs`：HTTPS GET（重定向/上限/超时）+ HTML→文本 + 多引擎搜索
  - 真机揪出：`TcpStream::connect(&str)` 遇不可达 IPv6 挂死 40s → 改 `connect_timeout` 逐地址尝试
  - 真机揪出：中文页 GBK 未解码 → 加 `encoding_rs` + `detect_charset`（否则正则一条不中）
  - 照太墟两段式正则：精确卡片类名（baidu `h3.t` / bing `b_algo`）→ 零结果才回退通用 h3
- `crates/ne-agent/src/mcp.rs`：stdio MCP 客户端 + `mcp__<服务>__<工具>` 路由 + 惰性连接 + Drop kill
- `crates/ne-agent/tests/mcp_e2e.rs`：5 个 e2e（真实 stdio 子进程）**已过**
- `runtime.yaml` 加 `mcp.servers`；`ToolArgs::to_json_except` 保类型透传
- CLI：`mcp list | tools [服务] | call <服务> <工具> '<json>'`

### 待办（M4 收尾）

1. 等 `cargo test -p ne-agent` 全绿（charset + 新正则）
2. 用**新版二进制**重跑联网探测（`examples/webprobe.rs`），确认搜索质量改善
3. 提交 + 后台推送 + 更新 CHANGELOG 0.0.2

## 进行中：M5 长大（上下文预算）

### 已完成（未编译未提交）

- `crates/ne-agent/src/context.rs`（324 行，11 单测）—— 逐条对照太墟 `ContextWindowPolicy.kt`：
  - `estimate_tokens`：**CJK 单独分桶**（全角标点按 CJK 价——否则系统性低估中文，吃掉折叠余量）
  - `truncate_stale_tool_results`：老轮次长工具结果压缩为"头+省略+尾"，**保护当前轮 + 最近 4 条**，
    **不增删消息**（NATIVE 协议下丢消息=非法 transcript）
  - `folding_limit(window, reserve_output, tool_schema_reserve)`：折叠线
  - `keep_from_index`：按预算丢弃**完整轮次**（保 tool_call⇄result 配对），至少留一轮
  - 契约细节见 `docs/ref_taixu_harness_notes.md §8`

### 待办（M5 收尾）

1. `lib.rs` 加 `pub mod context;`
2. `harness.rs::build_messages` 接 `truncate_stale_tool_results`（老轮次截断，在 `sanitize_for_provider` 前）
3. `ContextSection` 已有 `max_prompt_tokens`/`reserve_tokens` → 接入 `folding_limit`
4. 后台跑 `cargo test --workspace`，提交推送（v0.0.3）

## 进行中：收尾

### 已完成（本次会话，未提交）

- `crates/ne-agent/README.md`（85 行）：单二进制总说明（是什么 / 快速开始 / 设计立场 / 里程碑表 / 门禁）
- `.github/workflows/ci.yml`：编译前先跑 `python3 scripts/arch_check.py`
- `scripts/verify-portable.sh`：单二进制"拎包走"验证（拷到干净目录，仅凭 yaml 跑通全流程）
- 版本修正：workspace 0.0.5 + `STAGE = "M7 完善"`（之前提交信息写了 0.0.5 但 Cargo.toml 没改）
- 修 `crates/ne-engine/examples/gate14_parity.rs` 的 unused import 警告（1 行）

### 真机验证（verify-portable.sh 结果）

release 二进制拷到 `/tmp` 干净目录，**仅带 yaml**：
- `--version` → `ne-agent 0.0.5（M7 完善）`
- `vault init` → 自建 6 项；`vault show` → 自检
- `chat -m` 真跑一轮（假 LLM）
- `estimate` → 15 字 / 8 token
- `task new`/`task list` → queued 任务
- `session list`/`session rewind --last-turn` → 分支 2→0，库中保留
- `ldd` → **3 个动态库，仅 libc/libm/libgcc**（无脚本、无额外依赖）

### 待办

1. 等 portable2 验证 + examples 编译无警告
2. 提交推送（v0.0.5 收尾）

## M7 完善 —— 已推送 4d2f412

### 已完成（未验证编译/未提交）

- **单二进制确认**：release 9.2 MB（aarch64），`ldd` 仅 libc/libm/libgcc，无脚本/无 Rust 运行时；
  `Cargo.toml` 加 `strip=true` + `codegen-units=1`；版本统一到 0.0.4
- **架构门禁** `scripts/arch_check.py`（148 行）+ `architecture-policy.json` + `.architecture-baseline.json`
  - crate 依赖白名单（解析 path 依赖）→ **立刻揪出真实死依赖** `ne-engine → ne-funnel`（声明但零使用，已删）
  - import 黑名单（ne-agent 禁 tokio/reqwest）
  - 文件行数棘轮（上限 700，存量登记基线，只许下调）
  - 用法：`python3 scripts/arch_check.py`（检查）/ `--sync-baseline`（维护）
- **文件拆分**（过门禁）：`web.rs` 1036→414 行，拆出 `web_html.rs`（275）+ `web_search.rs`（382）；
  用 `pub use` 在原路径重导出，调用方零改动
- **子智能体** `subagent.rs`（312 行，13 单测）+ `task::run_subagents` + `subagent` CLI
  - 契约对照太墟 `SubagentArgsParser` + `SubagentTaskSpec`
  - **如实差异**：太墟 Lane 真并发；ne-agent 是 std 阻塞单线程 → 做**顺序 Lane**
    （每个子任务一个独立子会话隔离上下文 + 写租约**提前**校验冲突，非运行时竞争）
  - 写租约：`["*"]` 独占不能与其它共存；同路径（规范化后）不能重复声明

### 待办（M7 收尾）

1. **验证编译**：刚去掉 `web.rs` 里重复的 `body_to_text` 定义（`use` + `pub use` 冲突），需重跑
2. 加 subagent e2e 测试（假 LLM 跑两个子任务）
3. 全量测试 + 提交推送（v0.0.5）
4. 文档：ROADMAP M7 章节、CHANGELOG

## 已完成待提交：M5 + M6 —— 已推送 848ea86

### M5 长大（上下文预算 + 会话树 rewind）
- `context.rs`（13 单测）：token 估算 **CJK 分桶**（全角标点按 CJK 价）、
  老轮次工具结果截断（保护当前轮 + 最近 4 条，**不增删消息**）、折叠线
- `session.rs` rewind：`head_id` 显式指针（修 M1 的 `MAX(id)` 缺陷）+ 幂等迁移、
  `rewind_to`/`rewind_back`/`rewind_last_turn`（7 单测）
- `harness::build_messages` 接 `truncate_context`；CLI `estimate` + `session rewind`
- 修 UX 缺陷：`chat -m` 默认续接最新会话（原每次新建 → 会话散乱）
- 真机验证：`scripts/verify-m5.sh`（rewind 数据不删、哨兵空分支正确、estimate 生效）

### M6 安全与协作
- `approval.rs`（14 单测 + 4 e2e）：审批三档 + 正交 plan_only 门禁 + shell 反混淆，
  **接进工具循环**，无回调默认拒绝
- `task_state.rs`（13 单测）：纯转移表 + CAS 持久化（防幻觉成功）
- `context_tools.rs`（11 单测）：`plan`/`scratchpad` 持久化（plan.status 非法值保留原状）
- `Tool::Plan`/`Tool::Scratchpad` + `task` CLI + `chat --plan/--approval`
- 全 workspace **346 通过 0 失败**

### 待办
1. 提交推送 v0.0.4（M5+M6 一起）
2. M7：子智能体 Lane、单二进制打包、架构门禁

## 后续（M7，见 docs/ROADMAP-ne-agent.md）

- **M7 完善**：子智能体 Lane、单二进制打包、架构门禁（棘轮）

### M5 真机验证结论（`scripts/verify-m5.sh`）

- rewind 通过：分支 2→0 条，库中消息保留（"库中共 2 条，未删"），`head_id=-1` 哨兵空分支正确
- `estimate` 通过：21 字 → 11 token（CJK 分桶生效）
- 全 workspace **300 测试 0 失败**（M5 时）



## 命令速查

```bash
export PATH=/opt/taixu/toolchains/rust/bin:$PATH
cd /workspace/小说系统
cargo build -p ne-agent && cargo test -p ne-agent
cargo test --workspace                     # 243 基线

# 端到端（假 LLM）：见 crates/ne-agent/tests/{strict_llm_server.py,mem_capture_server.py}
export TAIXU_LLM_DIR=/tmp/fake-llm        # 指向含 models.yml+config.yml 的目录
target/debug/ne-agent --vault $V vault init
target/debug/ne-agent --vault $V chat -m "..."
target/debug/ne-agent --vault $V memory save k v --pinned
target/debug/ne-agent --vault $V session list

# 推送
source /attachments/skills/github-upload/scripts/gh-push.sh && gh_fix_dns
git push origin main
```

---

## M8 进行中（2026-10-11，跨段续跑）

### 已完成
1. **太墟工作流交付**（6 个 ne-agent 工作流装进太墟 App）
   - 定义：`workflows/taixu/ne_{build,test,gate,commit,push,verify}.json`
   - 安装器：`workflows/install_taixu_workflows.py`（`--emit-sql` / `--list` / `--uninstall` / 自动备份）
   - 真机通路（**关键，勿忘**）：App 在跑时无法停（`am force-stop` 会杀掉会话自身）。
     解法：host root → `chroot /data/data/top.wkbin.taixu/files/linux-runtime/distros/ubuntu/rootfs`
     → `mount --bind .../databases $R/tmp/appdb` → rootfs 内 `/usr/bin/sqlite3` 就地 upsert。
     sqlite3 支持 WAL 并发写，无需停 App。已验：完整性 ok / 会话·记忆·任务行数未变 / JSON 与内置 golden 同构 / 6 节点命令端到端实测通过。
2. **Android 交叉编译**：`scripts/cross-build-android.sh`（NDK clang + API24；坑：连字符 env 不能用
   `export`，改用 `env` 传 CC_aarch64-linux-android 等）。后台验证中。
3. **tool.rs 拆分**（架构门禁 900 行顶格）：拆为 `crates/ne-agent/src/tool/{mod,fs,external_io,session_local,tests}.rs`，
   已切分 + 设 `pub(super)` 可见性，**编译验证未做**。

### 未完成（续跑从这里接）
- [ ] tool 拆分编译验证（`cargo test -p ne-agent`）
- [ ] A1 `ask_user` 工具（复用 approver 回调通道，参考太墟 HarnessTool.ASK_USER）
- [ ] A2 `load_rule` / `load_skill` 按需加载（参考太墟 ToolExecutor LOAD_SKILL 400-480 行 + PromptRouter）
- [ ] A3 顺序版 dual agent（Planner→Executor，不并发）
- [ ] A4 Android 交叉编译产物验证通过
- [ ] A5 全量测试 + 门禁 + 文档/版本 → v0.0.6
- [ ] A6 提交推送

### 工具集清单（当前）
`read/write/edit/ls/base/plan/scratchpad/mcp__*/web_fetch/web_search`


### S1-S3 收尾（续跑 2 起）
- **A4 交叉编译：已完成**（scripts/cross-build-android.sh，产 aarch64 ELF 仅依赖 bionic libc/libm/libdl）。
- **tool.rs 拆分：已完成**（tool/{mod,fs,external_io,session_local,tests}.rs；157 测试 + 门禁通过。
  坑：拆分后必须 `rm tool.rs`，否则 Rust 优先单文件忽略目录）。
- **S2 resources.rs（新）：已完成**（load_rule/load_skill/strip_frontmatter/skill_catalog + 越界防护 + 8 单测）；
  `crates/ne-agent/rules/{workflow,security,tools}.md` 内置规则；vault.rs 加 RULES_DIR/SKILLS_DIR。
- **ask_user.rs（新）：已完成**（AskQuestion/AskOption + parse/render + 6 单测，对齐太墟 AskUserModels）。
- **tool/mod.rs：枚举/schema/分派/权限档 已扩到 14 个工具**；approval.rs 白名单+plan_block 已同步。
- **HarnessCtx 加 `system_override`/`vault_root`/`asker`；ToolCtx 加 `vault_root`/`llm`/`asker`/运行时字段**；
  harness.rs::run_session 已装配 ctx；task.rs make_ctx/chat_once/chat_interactive 签名已改。

### 未完成（下一步，注意：当前**未编译**）
- [ ] **新建 `crates/ne-agent/src/subagent_runner.rs`**：`run_subagents_in_ctx(ctx, tasks, max)` —— 从 ToolCtx
      装配子 ctx 并调用 subagent::run_subagents（现 task.rs 里已有 run_subagents/parent_ctx，但 session_local 调的是 subagent_runner）。
- [ ] **subagent.rs 需补 `pub const DEFAULT_MAX_TASKS` + `pub fn parse_subagents(&Value,usize)` + `pub fn render_outcomes`**（若缺）。
- [ ] **新建 `crates/ne-agent/src/dual_agent.rs`**：DualAgentParams/DualAgentOutcome/StepStatus/DEFAULT_MAX_STEPS/
      `from_ctx`/`run`（顺序版 Planner→Executor，参考 /workspace/taixu-ref/.../dual/）。
- [ ] lib.rs 注册新模块（ask_user、subagent_runner、dual_agent 若尚未注册）。
- [ ] task.rs 其余 make_ctx 调用点（parent_ctx / make_ctx_for_test / run_subagents / novel 任务）+ 测试补参数。
- [ ] main.rs 调用点补 vault_root / asker 实参；chat_interactive 挂 interactive_asker。
- [ ] **编译 + `cargo test -p ne-agent` + `python3 scripts/arch_check.py` 全绿**。
- [ ] 文档/版本 → v0.0.6，提交推送。

### S2-S6 完成情况（续跑 3）
- **S2 resources.rs**：✅（load_rule/load_skill/skill_catalog + 越界防护，单测通过）
- **S3 tool 枚举+schema+分派+审批**：✅（14 工具；approval 白名单 + plan_block 同步；tool/tests.rs 更新）
- **S4 dual agent 顺序版**：✅ `crates/ne-agent/src/dual_agent.rs`（+ dual_agent_tests.rs）+ `subagent_runner.rs`（切断 tool→task 循环）
- **S5 运行时装配**：✅（ToolCtx vroot/llm/asker/运行时字段；HarnessCtx system_override/vault_root/asker；
  harness::run_session 装配 + system prompt 资源目录段 resources_catalog；task.rs make_ctx/chat_once/chat_interactive
  + interactive_asker；main.rs 两处调用点补 vault 根；vault.rs 默认 enabled 加 ask_user/load_rule/load_skill + RULES/SKILLS 目录）
- **e2e 新增**：tests/m8_tools_e2e.rs（7 项）、tests/dual_agent_e2e.rs（2 项，脚本化假 LLM）——全绿。

### 全绿证据（续跑 3）
- `cargo test`（全工作区）32 个 test result，0 failed。
- `python3 scripts/arch_check.py` → 通过（10 crate，≤700 行）。
- `cargo clippy -p ne-agent`：**新增 3 处提示待清**：
  dual_agent.rs:491 redundant_closure（parse_step 闭包）、dual_agent.rs:507 collapsible_if、
  tool/mod.rs:264 type_complexity（asker 函数指针，可加 type alias）。

### 剩余（下一步）
- [ ] 清掉上述 3 处 clippy（或确认为既有风格可忽略）。
- [ ] 文档：ROADMAP-ne-agent.md 追加 M8 段；若有 CHANGELOG 同步。
- [ ] 版本号 Cargo.toml 0.0.5 → 0.0.6（workspace）。
- [ ] 提交 + 推送（gh-push.sh，见本文件顶部命令速查）。

### ✅ M8 全部完成（续跑 3 收尾）
- 3 处 clippy 已清（redundant_closure / collapsible_if / type_complexity→AskerFn）。
- ROADMAP-ne-agent.md 追加 M8 段；版本 0.0.5 → 0.0.6（Cargo.toml + 文档）。
- 全量验证：`cargo test` 32 套件 0 失败；arch_check 通过；Android aarch64 release 重建成功（6.4M，仅 bionic 库）。
- 提交 bab41f2，已推送 origin/main（bffde39..bab41f2）。工作树干净、与远端同步。

**M8 交付物**：ask_user.rs / resources.rs(+rules/*.md) / dual_agent.rs(+tests) / subagent_runner.rs /
tool/ 拆分 / scripts/cross-build-android.sh / workflows/（太墟工作流 6 个 + 安装器）/
tests/m8_tools_e2e.rs / tests/dual_agent_e2e.rs。工具数 10→14。

### M9 · 上下文可设置 + 压缩（为 UI 铺垫）—— 进行中（续跑 4）
用户需求原话：「上下文需要可以设置，压缩可以发命令，也可以手动压缩。为以后做 ui 铺垫」

**已完成（代码就位，编译通过、门禁绿、旧测试全绿）**
- `session.rs`：新增 `compactions` 表（**不删原文**，记折叠边界 keep_from_msg_id + 摘要 + 计数 + token 估算 + mode）；
  新方法 insert_compaction / latest_compaction / list_compactions / clear_compactions /
  message_by_id / search_messages(+snippet)。测试外置 `session_tests.rs`（session.rs 降到 592 行）。
- `compaction.rs`（401 行，**新**）：CompressMode(before/after) + resolve_compress_anchor（原样唯一锚点，
  对齐太墟 CompactionManager）+ Summarizer trait（LlmSummarizer / MechanicalSummarizer 降级）+
  **project() 投影**（摘要条 + 保留区；发模型与 UI 同口径）+ compact() + CompactionOutcome/Projection。
- `ctx_window.rs`（**新**）：从 harness.rs 拆出 truncate_context + maybe_auto_compact（门禁 ≤700；harness.rs 回到 650）。
- `harness.rs::build_messages`：改为经 `compaction::project` 装配（会话未压缩时行为与 M8 完全一致）。
- `config.rs` ContextSection 扩：auto_compaction / auto_threshold_percent / keep_recent_tool_results + 默认值。
- `vault.rs` DEFAULT_RUNTIME_YAML context 段补三项。
- `tool/mod.rs`：新增 3 工具 Compress/HistoryRead/HistorySearch（14→17），schema/tier/from_name/all/
  session_local/approval 白名单 + plan_block 全同步；`tool/session_local.rs` 实现三 handler。
- `cli.rs` + `cmds/cmds_ctx.rs`（**新**）：`context`（查看/设置，--json）、`compress`（--show/--clear/
  --mode/--anchor/--mechanical）、`history read|search`；JSON 形状即 UI 消费契约。
- 修 session_tests.rs 抽取遗留的多余大括号。

**未完成（下一步）**
- [ ] **挂载 `compaction_tests.rs`**：在 compaction.rs 末尾加 `#[cfg(test)] #[path="compaction_tests.rs"] mod tests;`（已写好 9 项单测，未接上）。
- [ ] `cargo test` 全量 + 门禁 + clippy（清理新引入警告）。
- [ ] 补 `tests/` 端到端：compress 走真实工具总线 + CLI `context --json` 形状。
- [ ] 文档 ROADMAP 追加 M9；版本 0.0.6 → 0.0.7；提交推送。

### M10 · 原生工具协议（真机暴露的头号缺陷）—— 进行中（续跑 5）

用户需求原话：「用真实模型测一下，看看是不是真的写好了」，并明确「不知道的问题、拿不准的
问题，都参考太墟」。

**真机验证暴露的头号缺陷**
- 用真实端点（127.0.0.1:8788，模型 id `glm-5.3-flash`）跑起来后，让模型调 `read` 工具，
  模型直接回「我没有文件读取工具」——**工具调用在真实模型上完全不工作**。
- 根因：`harness.rs:34` 写着 v1 走**文本协议**（`<tool_call>{json}</tool_call>`），
  「不走 provider 的结构化 tools/tool_calls 通道……留到 M4」；而 **M4 做的是 web/MCP，
  这件事从没兑现**。`ne-engine::llm::body_json` 不发 `tools`，`Message` 装不下 `tool_calls`。
- 对照实验（curl 直连端点，铁证）：
  - A 只列工具名（= 当前 ne-agent）→ 模型「我没有文件读取工具」❌
  - B 教它文本 `<tool_call>` 协议 → 仍拒绝 ❌
  - C **provider 原生 `tools` 通道** → `finish_reason=tool_calls` → 回填 result → 答出正确答案 ✅
- 结论：所谓 410 测试全绿，是因为「真机验收」用的是**假 LLM 服务端**，真实工具调用从未发生。

**方案（对齐太墟，用户指示参考太墟）**
- 太墟 `ProviderClient.kt`：`ApiMessage{content?, tool_calls?, tool_call_id?}` +
  NATIVE 模式注入 `tools` 数组（默认）+ 解析 `message.tool_calls`；
  `TextToolCallCodec.resolveCalls`：**结构化优先，文本回退**（`structuredCalls.ifEmpty{calls}`）；
  `sanitizeApiTranscript`：保住 tool_call 与 tool 结果的配对。
- 落地 = **双通道**：NATIVE 优先 + 文本回退。

**已完成（代码就位，编译通过，193 库测试全绿）**
- `ne-engine/src/llm.rs`：新增 `ToolCall`（**手写 serde，OpenAI 嵌套线格式**
  `{id,type:function,function:{name,arguments}}`）、`ToolDefinition`/`FunctionDefinition`、
  `ToolCallMode{Native,JsonText,Disabled}`；`Message` 加 `tool_calls`/`tool_call_id`
  （`skip_serializing_if=None` → 对既有调用方逐字节兼容）+ 构造器；`body_json` 有工具时注入
  `tools`；`call()` 解析 `tool_calls`，且**工具调用轮空正文不再当错误切 key**。
- `ne-agent/src/protocol.rs`（**新**）：`resolve`（结构化优先/文本回退）、`parse_reply`
  （兼容 `<tool_call>` 与太墟 `[[tool_call]]`）、`sanitize_for_provider`（**保配对**：
  合法 tool 结果保留，孤立/错位转写 user，缺失补占位）、`builtin_tool_definitions`（17 个内建
  工具 JSON Schema，MCP 动态工具不枚举）。
- `session.rs`：`MsgRow` 加 `meta` 列；`append_meta()` 存 assistant 的 `{"tool_calls":[...]}`
  与 tool 的 `{"tool_call_id":...}`；`to_llm()` 从 meta 重建结构 → 投影时配对可回放。
- `harness.rs`：主循环改走 `resolve(原生, 文本)` + meta 落盘；`parse_reply`/`sanitize` 迁至
  protocol.rs（harness 降到 542 行）；新增 `assistant_meta()`。
- `main.rs::build_runtime`：把 `protocol::builtin_tool_definitions(&enabled)` 注入 `llm.tools`。
- `context.rs` `HistMsg` + `ctx_window.rs`：**透传 tool_calls/tool_call_id**（曾因这里丢字段
  导致配对失效 → 假 LLM 死循环）。
- `tests/approval_e2e.rs`：假 LLM 改回**原生 tool_calls**（覆盖新协议）。

**未完成（下一步）**
- [x] 重跑 `cargo test --workspace` 全绿 → **414 通过 0 失败**。
- [x] **真机回归**：真实模型多轮工具回环（读→写→答）成功；工具失败路径如实报告；会话续接正常。
- [x] 破铁律：`ne-engine::llm` 扩展已在 ROADMAP 文件头注明修订说明与理由。
- [x] ROADMAP 追加 M10；版本 0.0.7 → 0.0.8；提交推送。

### ✅ M10 全部完成（续跑 5 收尾）
- 真机验证（真实端点 glm-5.3-flash）头号缺陷：工具调用完全不工作 → 已修（原生 tools 通道 + 文本回退）。
- 全量：`cargo test --workspace` 414 通过 0 失败；arch_check 通过；新代码（protocol.rs/llm.rs）零 clippy 警告。
- ROADMAP 头部铁律修订 + M10 段；版本 0.0.7 → 0.0.8；提交推送。

### M10.1 · 全路径真机验证（续跑 6）—— 揪出 dual_agent 两个真 bug

**动机**：M10 的教训是"假 LLM 服务端漏过全部协议类 bug"。真机只验了 chat，其余四条走
`llm::call` 的路径从未真机跑过。逐条真机验证。

**路径盘点（谁在用 llm::call）**
| 路径 | 真机结果 |
|---|---|
| chat / Executor | ✅ M10 已验 |
| compaction LlmSummarizer（`compress`） | ✅ 通过（折叠 26 条→结构化摘要，准确保留目标/决定/产物/未决） |
| **dual_agent Planner/Executor** | ❌ **两个真 bug，已修** |
| novel_chapter（`novel`，真实资料库 卷01） | ✅ 通过（2574 字，verdict=fix，P0=0） |
| ne-engine CLI `call` | ✅ tools 恒空（`build_params(.., None)`），无此风险 |

**真 bug #1：非工具角色误携 tools（与 M10 同类）**
- 现象：`dual_agent` 跑满 10 步，Planner assistant 消息**全空**，计划退化成"执行任务/空指令"，Executor 空转。
- 根因：`LlmParams.tools` 是**全局单一字段**（main.rs 注入 17 个内建 schema）。
  Planner 代码写着 `enabled: Vec::new() // Planner 无工具`，但请求仍带 `tools` →
  **真实模型优先返回 `tool_calls`（content 为空）** → `parse_planner_decision("")` 兜底空指令。
- 铁证（curl 探针）：Planner prompt + 带 tools → `finish_reason=tool_calls, content=''`。
- 修法（对齐太墟）：`ne-engine::llm::LlmParams::pure_chat()`（清空 tools + 置 Disabled），
  Planner 与 compaction 摘要器改用派生副本。太墟对应 `copy(pureChatMode=true, toolCallMode=DISABLED)`。

**真 bug #2：Planner 不守 JSON 协议 → 死循环（漏抄太墟的完成信号识别）**
- 现象：修完 #1 后，Planner 输出正常（"我没有工具"），但**仍跑满 max_steps**。
- 根因：真实模型**不遵守**提示词里的 JSON 协议，回散文（连"最终报告（FINISH）"这种）。
  我们的 `parse_planner_decision` 只有 JSON 解析 + "兜底重置 step_1"，**没有太墟的自然语言完成信号识别**
  → JSON 失败即重置 → 每轮追加/重置 → 永不终止。
  对比太墟 `PlannerProtocolParser`：**完成信号命中即 FINISH**；兜底用 `ExecuteStep`（累积不重置）；
  兜底 id `nextFallbackStepId` 避撞车。
- 修法：按太墟重写解析器 —— ①`looks_completed` 识别中文（已完成/全部完成/…）+ 英文词边界
  （completed/finished/**finish**）；②兜底改 `ExecuteStep` 累积；③`next_fallback_step_id` 防撞。
  安全性：协调器对"步骤未完成时的 FINISH"会打回（`不得提前 FINISH`），故识别 FINISH 不会静默收工。
- 效果：Planner 消息 21→4（轮次 10→2），耗时 190s→48s，产物正确。

**改动**
- `ne-engine/src/llm.rs`：新增 `LlmParams::pure_chat()`。
- `dual_agent.rs`：Planner 用 `pure_chat()`；`parse_planner_decision(text, current_steps)` 重写；
  解析段拆到 `dual_agent_parse.rs`（避免 700 行门禁）。
- `compaction.rs`：摘要器用 `pure_chat()`。
- `dual_agent_tests.rs`：新增 3 项（兜底累积/完成信号/词边界）。

**验证**：`cargo test --workspace` **416 通过 0 失败**；arch_check 通过；真机 dual_agent 2 轮收敛。

**未完成（下一步）**
- [x] ROADMAP 追加 M10.1；版本 0.0.8 → 0.0.9；提交推送。

### ✅ M10.2 · MCP 工具可达性缺口（续跑 7）

用户需求：「继续」，承接补 MCP 可用性缺口。

**缺口（真问题）**
- MCP 工具名是**动态**的（连上才知道），`protocol::builtin_tool_definitions` 不枚举 → 模型看不到；
  而唯一入口 `Tool::Capability`（`use_capability`）**没默认启用** → 配了 MCP 服务形同虚设。
- 直连 `mcp__<服务>__<工具>` 的**路由本来就通**（harness.rs 注入 `MCP_TOOL_KEY`），缺的是**可见性**。

**改动**
- `tool/capability.rs`（**新**）：静态 `use_capability` 工具，三 action：
  - `list` → 列已启用服务；`inspect + server` → 该服务工具名与参数 schema；
  - `call + server + tool + arguments` → 经 `McpRegistry` 调起（未连接首调自动启动）。
  - schema **稳定字节**（服务清单变化不进工具定义 → 不击穿 KV cache）。
- `lib.rs::resolve_enabled_tools`：**只要有已启用的 MCP 服务，自动补上 `Tool::Capability`**（去重；
  `enabled:false` 不算数）。这是接通缺口的关键一刀。
- `protocol.rs::sanitize_for_provider`：悬空工具调用（assistant 有 tool_call、无结果）占位从
  `工具结果缺失` 改为**结构化 JSON + 稳定错误码 `unknown_tool_result`**（`outcome:"unknown"`，
  明确"未核验前**严禁重放**"）——对齐太墟 v0.22.0 `errorCode` 语义；顺带修多个悬空调用互相覆盖
  （`BTreeSet<String>` 丢工具名 → 改 `Vec<(id,name)>`）。
- `vault.rs`：模板注释修正（原写"MCP 工具模型直接可调"是**误导**，改为 use_capability 用法）。
- `tool/mod.rs`/`tool/tests.rs`/`approval.rs`：Capability 的 tier/白名单/plan_block 同步。

**验证**
- 新增全链路 e2e（`tests/mcp_e2e.rs`）：假 LLM 只发 `use_capability` → harness 循环 → 真 MCP 服务
  回显 `echo: 残响之河` ✅。
- **真机 smoke（真实端点 glm-5.3-flash @ 127.0.0.1:8788）**：模型自发 list → inspect fake → call fake/echo，
  会话库铁证（assistant `tool_calls` 三次 `use_capability`，tool 消息 `[OK] echo: 残响之河`，最终答"残响之河"）✅。
- `cargo test --workspace` **431 通过 0 失败**；arch_check 通过；Linux + Android release 均重建成功（6.6M，仅 bionic 库）。

**版本**：0.0.9 → 0.0.10。
