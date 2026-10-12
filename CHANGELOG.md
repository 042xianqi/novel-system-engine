# 版本历史

本项目采用 `MAJOR.MINOR.PATCH`，**从 0.0.1 起步**。
`0.0.x` 阶段 = 快速迭代期（接口可能变）；`0.1.0` 起进入可用里程碑。

版本号统一在根 `Cargo.toml` 的 `[workspace.package] version` 定义，
所有 crate 用 `version.workspace = true` 继承（**单一事实源，不各写各的**）。
`ne-agent version` / `ne-agent --version` / `ne-mcp` 握手均从此取值。

## 记录规则

- 每次有实质进展 → 递增 PATCH 位（0.0.1 → 0.0.2 …），并与该次提交一起落。
- 每完成一个路线图里程碑（M0–M7）→ 递增 MINOR 位，打 git tag。
- 破坏性接口变更（对已发布契约）→ 递增 MINOR 或 MAJOR 视影响而定。

| 版本 | 日期 | 里程碑 | 内容 |
|---|---|---|---|
| 0.0.13 | 2026-10-12 | M10.5 | `ne_cardgen` 内联式【待建卡】兜底：卷01/03-10 用 `> - 名字（…，无XX卡）` 清单式，但**卷02 全卷无清单**、改用内联式（`…实体名【…待建卡…】`）→ 原实现对该卷返回 0 张卡（漏报）。新增 `inline_card_jobs`：**仅当无 `>` 清单时**启用（口径互斥不重叠），名字取 `【` 前最后一个强分隔段，过**类别白名单**（`·`具名 / 角色词 / 短地点词；显式排除 `章*`、含 `§#/` 等噪声）。实测卷02 由 0 → 38 张真卡（加百列·黑刃 / 观察仪值守修士 / 鬼街 …），卷01 仍 9 张不变。新增 3 个单元测试；`cargo test --workspace` 481 通过 0 失败 |
| 0.0.12 | 2026-10-12 | M10.4 | 修 `ne_cardgen` 抽卡误报：`extract_card_jobs` 正则 `^>?`（`>` 可选）会把细纲**正文**里的普通 `- xxx（yyy）` 行也当待建卡（卷01 实测匹配 542 行、真卡仅 9 张；卷02 匹配 1397 行、真卡 0、全误报）。**必须要求行首 `>` 前缀**（【待建卡】清单专属标记）。同步修 `ne-cardgen` 与 `ne-mcp`（后者为免重量级依赖而自实现的等价副本，两份口径必须一致）。修复后卷01 抽出 9 张真卡、无脏名字（原先是 `**目标**：把白鸦` / `柒玖）【卡·REL-99-004】…`）。新增回归测试 `body_lines_without_quote_prefix_are_ignored`；`cargo test --workspace` 478 通过 0 失败 |
| 0.0.11 | 2026-10-12 | M10.3 | 真流式 + 思考过程：`ne-engine::llm` 加 SSE 流式路径（`LlmParams.stream`，`call_stream` + `SseAccumulator` 增量解析、`reasoning_content`→思考增量、`delta_sink` 进程级回调）；`http::post_json_stream` 增量解帧（chunked 状态机，边收边回调，区分传输分帧与 SSE 业务分帧）；`task.rs` 用 `<<<NE_REASONING>>>`/`<<<END_NE_REASONING>>>` 显式定界，思考→stderr、正文→stdout 实时转发。**修回归**：新增 `parse_nonstream_body`——当网关/测试桩无视 `stream:true` 直接回整体 JSON 时，SSE 累加器一无所获则按非流式结构回退解析（此前 5 个 e2e target 因 `empty content` 全挂）。修后 `cargo test --workspace` 477 通过 0 失败 |
| 0.0.10 | 2026-10-11 | M10.2 | MCP 可达性缺口：新增静态 `use_capability` 代理工具（`tool/capability.rs`，action `list`/`inspect`/`call`，schema 字节稳定不击穿 KV cache）；**配了已启用 MCP 服务时 `resolve_enabled_tools` 自动启用该代理**（此前 MCP 工具名动态、不进工具面，模型无从发现与调用）；悬空工具调用占位改为结构化 JSON + 稳定错误码 `unknown_tool_result`（如实标注"结果未知、未核验严禁重放"，对齐太墟 v0.22.0）并修多悬空调用互相覆盖；vault 模板注释修正（原"MCP 工具直接可调"为误导）。真机（glm-5.3-flash）验 list→inspect→call→MCP echo 全通 |
| 0.0.9 | 2026-10-11 | M10.1 | 全路径真机验证：真机揪出 dual_agent 两个真 bug 并修——①非工具角色误携 tools（`LlmParams::pure_chat()`，Planner/摘要器改用）②Planner 不守 JSON 协议导致死循环（重写解析器：完成信号识别 + 兜底累积 + 防撞 fallback id，对齐太墟 `PlannerProtocolParser`）。真机逐条验 chat/compress/dual_agent/novel/ne-engine |
| 0.0.8 | 2026-10-11 | M10 | 原生工具协议：真机暴露**头号缺陷**——真实模型上工具调用完全不能用（模型回"我没有文件读取工具"），根因是 harness 只走 v1 文本 `<tool_call>` 协议、从未注入 provider 原生 `tools`。修法双通道（原生 tools 优先 + 文本回退）：`ne-engine::llm` 加 `ToolCall`/`ToolDefinition`/`ToolCallMode` + `Message.tool_calls`/`tool_call_id`（对旧调用方逐字节兼容）、`protocol.rs`（`resolve`/`parse_reply`/`sanitize_for_provider`/`builtin_tool_definitions`）、`session` 落 meta 保配对。教训：此前"真机验收"用假 LLM，漏过全部协议类 bug |
| 0.0.7 | 2026-10-11 | M9 | 上下文可设置 + 压缩（为 UI 铺垫）：`context` 段新增 `auto_compaction`/`auto_threshold_percent`/`keep_recent_tool_results` 与 `ne-agent context`（查看/设置/`--json`，写回保留注释）；`compaction.rs`（锚点解析 before/after、`LlmSummarizer`/`MechanicalSummarizer` 降级、`project()` 投影为发模型与 UI 的唯一读口径）+ `ctx_window.rs`（窗口策略 + 自动压缩判定）；`compress` 工具与 `ne-agent compress`（`--show`/`--clear`/`--mechanical`）；`history read`/`history search` 使**被折叠原文可回读**（压了不丢）；工具 14→17 |
| 0.0.6 | 2026-10-11 | M8 | 补齐旧待办：`ask_user.rs`（多问 + 候选项 + 自定义，答案由外部回调回填，非交互环境明确交回主智能体）；`resources.rs` + `rules/*.md`（`load_rule`/`load_skill` 按需加载，system prompt 只放目录元数据，技能子资源越界防护）；`dual_agent.rs`（顺序版 Planner→Executor，保留 DAG 就绪判定与 REPLAN/级联失败语义）+ `subagent_runner.rs`（切断 task→tool→task 循环）；Android aarch64 交叉编译（`scripts/cross-build-android.sh`，仅依赖 bionic）；`tool.rs` 拆分为 `tool/{mod,fs,external_io,session_local}.rs`；工具 10→14 |
| 0.0.5 | 2026-10-11 | M7 | 完善：架构门禁（`scripts/arch_check.py` + crate 依赖白名单 + import 黑名单 + 行数棘轮）；子智能体 Lane（`subagent.rs` 顺序隔离 + 写租约 + `subagent` CLI）；文件拆分（web.rs → web_html/web_search）；单二进制确认（9.2MB，仅 libc）+ strip |
| 0.0.4 | 2026-10-11 | M6 | 安全与协作：`approval.rs`（审批三档 + 正交的只读规划门禁 + shell 反混淆，接进 harness 工具循环，无回调默认拒绝）；`task_state.rs`（任务状态机：纯转移表 + CAS 持久化，防进程恢复幻觉成功）；`context_tools.rs`（`plan`/`scratchpad` 持久化，plan.status 非法值保留原状）；`plan`/`scratchpad` 工具 + `task` CLI + `chat --plan/--approval` |
| 0.0.3 | 2026-10-11 | M5 | 上下文预算：`context.rs`（token 估算 CJK 分桶、老轮次工具结果截断、折叠线）；会话树 rewind（`head_id` 显式指针 + 迁移）；`estimate` CLI；修 UX 缺陷（chat 默认续接会话） |
| 0.0.2 | 2026-10-11 | M4 | 外部世界：`web.rs`（自写 HTTPS GET + HTML→文本 + 多引擎搜索，多引擎失败自动回退）、`mcp.rs`（stdio MCP 客户端 + `mcp__<服务>__<工具>` 路由 + 惰性连接）、`mcp` CLI。真机揪出并修：`TcpStream::connect` 遇不可达 IPv6 挂死 → 改 `connect_timeout` 逐地址尝试；中文页 GBK 未解码 → 加 `encoding_rs` + charset 探测。 |
| 0.0.1 | 2026-10-11 | M0–M3 | 引入版本管理；整理既有成果为可迭代基线（vault/config/CLI、会话树+prompt+工具+主循环+chat、pinned+BM25 记忆、novel 任务补齐全仓缺失的 BudgetCtx/ChapterCtx） |
