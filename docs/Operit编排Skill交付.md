# novel-writer-suite 交付记录（2026-10-14，2026-10-15 修订）

> **2026-10-15 修订**：SKILL.md 契约已从「6 个不存在的 novel_* 工具」**对齐为真实的 15 个 ne-novel 工具**
> （7 引擎 + 8 world-gov）；新增 `references/系统功能总览.md`；README/落地核对同步更新。
> 详见本目录末尾「2026-10-15 修订」。

## 交付物

位于 `/storage/emulated/0/Download/Operit/skills/novel-writer-suite/`：

| 文件 | 内容 |
|---|---|
| `SKILL.md` | 技能元数据库（name/description）+ 契约 + 硬约束 |
| `references/VERIFICATION.md` | 运作前提核实（3 处不符） |
| `references/系统功能总览.md` | **【新】系统完整功能面（30 命令 + 15 工具 + 铁律）** |
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

---

## 2026-10-15 修订（配合对齐）

**修订动机**：SKILL.md 原契约列的是 6 个**不存在的** `novel_*` 工具，与 ne-novel 实际能力脱节。

**变更**：
1. `SKILL.md` §执行契约 → 改为真实两路：
   - **A. ne-novel MCP 15 工具**（7 引擎 + 8 world-gov，逐个列出入参）
   - **B. ne-agent CLI**（novel 生成 / score 质检 / fix 修复 / audit-* 审计 / sys-check 自检 / ne-engine sync 落库）
   - **C. 章节生成完整链路**：`ne_inject → 模型 → ne_check → fix → sync → wg_snapshot`
2. `SKILL.md` §落地核对 → 更新为「路线 A 推荐、路线 B 暂不可用」。
3. `README.md` 路线 B 说明同步修正。
4. 新增 `references/系统功能总览.md`（30 命令 + 15 工具 + 铁律，跨会话权威源）。

**未改动**：硬约束四条全部保留（Skill 只编排 / MCP 不调度不 fork / 冲突升级人工 / 不杀进程）。
**未动 MCP**：15 工具保持只读/质检定位，未新增任何工具（符合「不 fork shell、不做调度」硬约束）。

**配合关系（定稿）**：
```
Operit 编排层（工作流 / Skill）
   ├─ ne-novel MCP（mcp_call.mjs 桥）→ 只读/质检/检视（15 工具）
   └─ super_admin:terminal → ne-agent CLI → ne-engine（生成/治理/审计/落库）
职责：Operit 管编排/日志/并发/挂起；ne 引擎管全部小说业务逻辑。
```
