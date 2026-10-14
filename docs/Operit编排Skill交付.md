# novel-writer-suite 交付记录（2026-10-14）

## 交付物

位于 `/storage/emulated/0/Download/Operit/skills/novel-writer-suite/`：

| 文件 | 内容 |
|---|---|
| `SKILL.md` | 技能元数据库（name/description）+ 契约 + 硬约束 |
| `references/VERIFICATION.md` | 运作前提核实（3 处不符） |
| `scripts/mcp_call.mjs` | ne-mcp stdio JSON-RPC 同步桥（已实测） |
| `workflows/*.dwf.ts` | 4 个 DWF 脚本（路线 B） |
| `workflows/novel-native.workflow.json` | 原生可视化工作流（路线 A，已校验 JSON） |

## 运行前提核实（3 处不符）

1. **当前安装版 Operit 无 DWF 的 TS 门面。**
   证据：base.apk 中 `CreateWorkflow` 命中的只有 `CreateWorkflowDialog`（原生可视化工作流 UI）；
   `world.run` / `EvalWorkflowSnippet` / `zcode-workflow` / `run-settled` 均 0 命中；
   APK 未携带 `app.asar` / `glm/zcode.cjs`。→ journal/phase/artifact/escalation/hole
   都不可用；DWF 脚本无法在本机 Eval/Run。
2. **ne-mcp 实际暴露 7 个工具**（funnel / ne_cardgen / ne_inject / ne_check /
   ne_materials / ne_db / ne_db_query），而需求文档列的 6 个 `novel_*` 尚未实现。
3. **工作流门面无常驻工具原语**，只有 `world.run(cmd, args)`，命令名需为编译期字面量。
   → 每条 MCP 调用 = 一次子进程；已提供 mcp_call.mjs 桥。

## 两条路线

- **路线 A（当前可跑）**：原生可视化工作流，execute 节点 `actionType: super_admin:terminal`
  调 ne-agent CLI / mcp_call.mjs。冲突挂起粒度 = 整个工作流。
- **路线 B（需前置）**：DWF 脚本已按契约写好；需先在 ne-mcp 补齐 6 个 `novel_*` 工具，
  并需要 Operit 提供 DWF 运行环境。冲突挂起粒度 = 单分支。

## 保留的硬约束

- Skill 只做编排封装，业务逻辑全在外部 MCP/CLI；
- MCP 不调度、不建事件日志表、不 fork shell（桥是子进程，不是 MCP fork）；
- 冲突返回 `{"ok":false,"kind":"await_user_input"}`，由编排层升级；
- 停止工作流用界面终止，不杀外部 ne-mcp 进程。

## 待人工决策

两条路线的 escalation 粒度不同（A=整体、B=单分支）。若要细粒度，必须补前置。
