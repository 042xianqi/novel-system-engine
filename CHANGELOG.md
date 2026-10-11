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
| 0.0.5 | 2026-10-11 | M7 | 完善：架构门禁（`scripts/arch_check.py` + crate 依赖白名单 + import 黑名单 + 行数棘轮）；子智能体 Lane（`subagent.rs` 顺序隔离 + 写租约 + `subagent` CLI）；文件拆分（web.rs → web_html/web_search）；单二进制确认（9.2MB，仅 libc）+ strip |
| 0.0.4 | 2026-10-11 | M6 | 安全与协作：`approval.rs`（审批三档 + 正交的只读规划门禁 + shell 反混淆，接进 harness 工具循环，无回调默认拒绝）；`task_state.rs`（任务状态机：纯转移表 + CAS 持久化，防进程恢复幻觉成功）；`context_tools.rs`（`plan`/`scratchpad` 持久化，plan.status 非法值保留原状）；`plan`/`scratchpad` 工具 + `task` CLI + `chat --plan/--approval` |
| 0.0.3 | 2026-10-11 | M5 | 上下文预算：`context.rs`（token 估算 CJK 分桶、老轮次工具结果截断、折叠线）；会话树 rewind（`head_id` 显式指针 + 迁移）；`estimate` CLI；修 UX 缺陷（chat 默认续接会话） |
| 0.0.2 | 2026-10-11 | M4 | 外部世界：`web.rs`（自写 HTTPS GET + HTML→文本 + 多引擎搜索，多引擎失败自动回退）、`mcp.rs`（stdio MCP 客户端 + `mcp__<服务>__<工具>` 路由 + 惰性连接）、`mcp` CLI。真机揪出并修：`TcpStream::connect` 遇不可达 IPv6 挂死 → 改 `connect_timeout` 逐地址尝试；中文页 GBK 未解码 → 加 `encoding_rs` + charset 探测。 |
| 0.0.1 | 2026-10-11 | M0–M3 | 引入版本管理；整理既有成果为可迭代基线（vault/config/CLI、会话树+prompt+工具+主循环+chat、pinned+BM25 记忆、novel 任务补齐全仓缺失的 BudgetCtx/ChapterCtx） |
