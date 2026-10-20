# 《run_world_gov 拆分预案》

> 对象：`crates/ne-agent/src/main.rs` 中的 `run_world_gov`（约 **2164 行**，占全仓的 3%）
> 状态：**预案（未动代码）**。按主人裁定，动它之前先出方案、确认可变借用风险。
> 生成：2026-10-20

---

## 0. 为什么它是"最硬的骨头"

| 事实 | 数值/证据 |
|---|---|
| 单函数行数 | ≈ 2164 行（`fn run_world_gov` 起始于 `main.rs:1293` 时代；平移后行号已前移） |
| 与其它函数的关系 | `main.rs` 中 **5 个私有助手只服务于它**：`gov_enqueue`、`world_gov_emit`、`wg_probe_hash`、`read_optional_json`、`build_prompt_vars`、`wg_latest_snapshot`、`safe_export_path` |
| 被谁调用 | **只有 `run()` 分发器**（`Cmd::WorldGov`）→ 外部无耦合，内部高度内聚 |
| 架构门禁 | `architecture-policy.json` 规定 `maxFileLines: 700`，当前 `main.rs` 仍 2791 行 → 仍未达标 |

**结论**：它是**内聚但臃肿**——这既是坏消息（改起来危险），也是好消息（只要保持对外签名 `run_world_gov(vault, lib_root, cmd, json, quiet) -> Result<()>` 不变，内部怎么切都不影响调用方）。

---

## 1. 六阶段职责划分（按数据流，不是按行数硬切）

| # | 阶段 | 职责 | 输入 → 输出 | 预估行数 |
|---|---|---|---|---|
| ① | **scan 扫描** | 遍历资料库、收集卡/章/大纲现状、算指纹（`wg_probe_hash`）、落快照（`wg_latest_snapshot`、`safe_export_path`） | `lib_root` → `WorldSnapshot` | ~300 |
| ② | **govern 治理** | 依扫描结果跑门禁/合规基线、生成待办、写任务队列（`gov_enqueue`）、发事件（`world_gov_emit`） | `WorldSnapshot` → `Vec<Task>` | ~450 |
| ③ | **report 报告** | 汇总为 JSON 报告、`structured_log` 埋点、打印 | `Vec<Task>` → `Value` | ~250 |
| ④ | **reflow 回流** | 把治理结论回写到库（备注/标签/风险清单），必要时建快照 | `Value` → `ReflowStats` | ~350 |
| ⑤ | **audit 审计** | 合规基线复核、异常汇总、与 `audit_material` 对接 | `ReflowStats` → `AuditReport` | ~350 |
| ⑥ | **finish 收尾** | 锁释放、退出码、JSON 输出（`--json`）、终端摘要 | `AuditReport` → `Result<()>` | ~250 |

合计 ≈ 1950 行（原 2164 行的其余部分是函数头部注释、参数解析与阶段间胶水）。

> ⚠️ 阶段名是我按**当前代码的实际数据流**归纳的，实施第一步必须先做**行号锚定**（把 2164 行按注释块/空行/阶段标志切出真实边界），不能照搬这张表的行数。

---

## 2. 可变借用风险分析（这是主人最关心的点）

### 2.1 风险等级：**中高**，但可控

`run_world_gov` 内部大概率存在这三类**跨阶段可变状态**：

| 风险类型 | 表现 | 拆分会遇到的问题 |
|---|---|---|
| **R1 可变累积器** | 一个 `let mut stats = ...` 被 ①②④⑤ 反复 `+=` 改写 | 提取成独立 `fn` 时必须把它作为 `&mut` 参数传递，否则借用断裂 |
| **R2 借用中的再借用** | 例如 `for x in items.iter_mut() { ... 调用需要 &items ... }` | 机械提取会导致「cannot borrow as mutable more than once」 |
| **R3 闭包捕获** | 阶段内用闭包捕获外部可变变量（如 emit 日志的 `|m| stats.push(m)`） | 闭包搬进新 `fn` 后捕获失效，需显式传参 |

### 2.2 应对策略（四步，绝不一步到位）

**第一步：行号锚定与阶段边界确认（只读）**
- 输出一张「阶段 → 起始行 → 结束行 → 该阶段出现的 `let mut` 变量清单」的表。
- **不写任何代码**，先让主人看到真实的借用分布。

**第二步：同文件提取 `fn`（关键：保持在同一文件内）**
- 在 `main.rs` **内部**把每个阶段抽成 `fn wg_scan(...) -> Result<X>` 等私有函数。
- 此阶段**不改文件结构**，只改组织方式 → 一旦编译报借用错误，立刻能定位到具体阶段，回退成本极低。
- 每个阶段抽完立即 `cargo test -p ne-agent`（全量 665+ 测试），绿了才做下一个。
- **顺序建议**：先 `report`（③，最像纯函数、几乎无 `&mut`）→ 再 `audit`（⑤）→ 再 `scan`（①）→ 再 `finish`（⑥）→ 最后才是 `govern`（②）和 `reflow`（④）（这两个 `&mut` 最重）。
  > 理由：从"最不碰可变状态"的阶段开始，先用低成本成功建立节奏与信心，同时**尽早暴露**跨阶段共享状态的形状。

**第三步：引入阶段间状态结构体**
- 把跨阶段共享的可变状态收进一个 `struct WgCtx { snapshot, tasks, stats, ... }`，各阶段函数签名统一为 `fn wg_xxx(ctx: &mut WgCtx, ...) -> Result<()>`。
- 这一步**同时解决 R1 与 R3**：累积器成为结构体字段，闭包改为方法。
- R2 需要逐点人工处理（通常是先把索引/键收集到 `Vec`，再二次遍历）。

**第四步：搬家到独立模块**
- 新建 `crates/ne-agent/src/world_gov/` 目录：`scan.rs` / `govern.rs` / `report.rs` / `reflow.rs` / `audit.rs` / `finish.rs` + `mod.rs`（只放 `WgCtx` 与对外 `run()` 门面）。
- 每个文件目标 ≤ 450 行（留出增长余量，门禁是 700）。
- 搬完把 `main.rs` 的 `run_world_gov` 缩成 3~5 行门面：
  ```rust
  fn run_world_gov(vault, lib_root, cmd, json, quiet) -> Result<()> {
      world_gov::run(vault, lib_root, cmd, json, quiet)
  }
  ```
- **对外签名逐字不变** → `run()` 分发器与所有测试零改动。

### 2.3 不可触碰的红线

1. **不引入 tokio/async**：现有是 std 阻塞式，拆分不得借机改成 async（仓库铁律）。
2. **治理层只读铁则**：`reflow` 阶段虽然写库，但走的是既有 `governance_schema.sql` 表；拆分不得新增表或改变写入语义。
3. **`--json` 输出结构逐字不变**：`finish` 阶段产出被面板与脚本消费，字段不能动。
4. **不合并阶段**：即使某阶段只有几十行，也独立成 `fn`——保持"一阶段一函数"的可测性。

---

## 3. 验收标准

| 项 | 标准 |
|---|---|
| 编译 | `cargo check -p ne-agent` 零新增 warning |
| 测试 | `cargo test -p ne-agent -p ne-engine` 全绿（当前基线：879 测试 0 失败） |
| 行为 | `ne-agent world-gov --help` 与 `--json` 输出**逐字段一致**（对比拆分前后各跑一次，`diff` 验证） |
| 结构 | `main.rs` ≤ 700 行（达标门禁）；`world_gov/*` 每文件 ≤ 450 行 |
| 回归 | 端到端：`world-gov scan-library --json` 在真实资料库上跑通 |

---

## 4. 风险与回退

- **回退点**：每一步前 `cp main.rs` 到 `/workspace/_phase1_src/main.rs.<step>.bak`（本工程既有惯例）。
- **最坏情况**：若 `govern`/`reflow` 的借用纠缠到无法在不改语义的前提下拆分，**允许保留这两个阶段在原文件**——即"部分拆分"也是可接受的中间态，不追求一次到底。
- **禁止**：为了让借用通过而引入 `Rc<RefCell>` / `unsafe` / clone 大对象。若真需要，先停下向主人报告取舍。

---

## 5. 排期建议

| 步骤 | 内容 | 预估 |
|---|---|---|
| S1 | 行号锚定 + `let mut` 清单（只读，产出表） | 1 轮 |
| S2 | `report` 提取 + 测试 | 1 轮 |
| S3 | `audit` / `scan` / `finish` 提取 + 测试 | 2~3 轮 |
| S4 | `WgCtx` 结构体引入（解决 R1/R3） | 1~2 轮 |
| S5 | `govern` / `reflow` 提取（最难） | 2~3 轮 |
| S6 | 搬家到 `world_gov/` + 门禁收紧 | 1~2 轮 |

**总计约 9~12 轮**，每轮都以"编译 + 全量测试"收尾，任一轮失败即停下报告。
