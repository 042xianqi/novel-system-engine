# ne-agent 开发进度（跨段续跑用）

> 最后更新：2026-10-11（M3 进行中）
> 仓库：github.com/042xianqi/novel-system　本地：/workspace/小说系统
> 路线图：docs/ROADMAP-ne-agent.md　参考笔记：docs/ref_taixu_harness_notes.md、docs/ref_omp_notes.md
> 太墟源码：/workspace/taixu-ref（Kotlin，47535 行，**主参考**）

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

测试基线：M4 后全 workspace **282 通过 0 失败**。

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

## 已完成待提交：M5 + M6

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
