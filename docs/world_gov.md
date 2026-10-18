# world-gov · 世界观认知治理层（第 1 批交付）

> 日期：2026-10-14 ｜ 基线：`f567dda` 之后新增 ｜ CI：见最新 `RESULT=`

## 一、定位（与 library-ctx 严格分工）

| 模块 | 角色 | 边界 |
|---|---|---|
| `library-ctl` | 档案保管员 | 管文件/结构/索引/数字数；**不做任何语义判断** |
| `world-gov` | 世界观法务官 | 带法理基线理解内容、语义级审计；只出**证据+建议**，不独裁、不自改 |

## 二、法理契约（硬编码，全模块统一引用）

```text
L0（宇宙顶层总纲）
  ≫ L0.5（世界脊梁 / 核心基调）
  ≫ L1（全系列主线）
  ≫ L1x（跨卷长线 / 暗线）
  ≫ ANCHOR（跨卷锚点：跨卷不可破约束）
  ≫ GUARD（护栏：禁止事项、边界红线）
  ≫ L3（单卷细纲、单卷目标）
  ≫ L4（资料卡：角色/势力/物品/规则等实例素材）
```

- 上层 = 法理来源；下层可细化、实例化，**不能反向否定上层**。
- 每张 L4 卡必须可回溯：它遵从哪一条上层约束。
- "干净" = 逻辑自洽、层级不逆、引用有理、定义无自噬（≠文件名不乱）。

## 三、子命令（本轮 3 个可用 + 2 个预留）

```bash
B=/root/novel-system/target/release/ne-agent
LIB='${NOVEL_LIB_ROOT}/'

# 1) 建立法理基线（只存摘要/指纹，不存原文；--dry-run 沙盘不落盘）
$B --work-root "$LIB" world-gov bootstrap --dry-run   # 沙盘
$B --work-root "$LIB" world-gov bootstrap              # 写 07_勘误工坊/world_baseline.json

# 2) 单卡深度检视（默认零模型产探针包；--semantic 显式开才调模型；--dry-run 只打印 prompt）
$B --work-root "$LIB" world-gov inspect-card --key CHAR-10-001
$B --work-root "$LIB" world-gov inspect-card --key CHAR-10-001 --semantic --dry-run
$B --work-root "$LIB" world-gov inspect-card --key CHAR-10-001 --semantic   # 真调模型（未放行勿跑）

# 3) 法理溯源链（只读）
$B --work-root "$LIB" world-gov trace --key CHAR-10-001

# 4/5) 预留（调用即报错说明，属第 2 批）
$B --work-root "$LIB" world-gov scan-library --semantic --slice 1/4
$B --work-root "$LIB" world-gov check-outline --volume 卷01
```

## 四、铁则实现

- **绝不侵入**主生成/LTM注入/短期记忆/快照/prompt 组装链路（纯新增模块，零修改既有函数行为）。
- 语义类能力一律**显式开关** `--semantic`；默认只产**探针包 probe_pack**（零模型）。
- `--dry-run` 沙盘：不写库、不落盘。
- 判冲突必须附【原文片段 + 上层基线原话】；解析失败/拿不准一律 `UNCERTAIN`，不强行归为风险（有单测锁定）。
- `world_baseline.json` 只存摘要（≤160 字/条）、边界线索、FNV-1a 指纹，**不存完整原文**（实测 8 条 8958 字节）。

## 五、risk_index 增强（两库分离）

| 列 | 含义 |
|---|---|
| `is_semantic` | 0=结构脏点（library-ctl） / 1=世界观法理脏点（world-gov） |
| `law_basis` | 判定依据：违反/依据的上层条目 key 与原话 |

- `audit-risk-list --json` 新增 `structural_count` / `semantic_count` 分组。
- 真库回归：旧 2 条悬空引用完好（structural=2 / semantic=0），零破坏。
- 幂等加列由 `open_gov` 自动完成，旧库打开即升级，无需手工迁移。

## 六、单测（8 个，全过）

指纹稳定性/敏感性、基线指纹随条目变更、边界线索封顶 12、
候选法理按优先级降序（L0 在前）、语义 JSON 直读+文本内嵌 JSON 双解析、
乱码必须 UNCERTAIN、语义风险入语义桶且幂等、法理链顺序契约。

## 七、第 2 批路线（本轮未实现，CLI 已占位）

- `scan-library --semantic [--slice N/M]`：语义脏点五查（真重复/违背上层/同层自噬/隐形悬空/法理倒转），产 `world_semantic_issues.md+json`。
- `check-outline --volume 卷名`：卷细纲法理评估（越界/自洽/L4 支撑），只出报告。
- `audit-chapter-compliance` 增强：复核带 world-gov 基线，每条警告回溯"违背哪层哪条"。
- 触发条件：模型实跑单独放行后实施。
