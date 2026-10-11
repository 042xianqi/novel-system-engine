# 进度存档：资料库数据层重构

> 更新：2026-10-11 · 状态：**S10 ✅ 已完成**（用户已放行）· 流水线运行时已恢复并验证

---

## ✅ S10 已完成（2026-10-11，用户放行）

提交：`214493a`（清理+脚本）· 回滚：`tools/s10_rollback.sh`（可逆，已实测）

**运行唯一真值源 = 设备端 `novel.db`**；`02_资料库` 仅存在于归档备份，**不再作为运行时输入**。

| 步骤 | 动作 | 结果 |
|---|---|---|
| 1 | 备份 DB | `09_冷备份/pre_s10_20261011_0148/novel.db`（959 行 / 27 空正文） |
| 2 | 导出被删行 | `09_冷备份/pre_s10_20261011_0148/removed_27_placeholder_rows.json` |
| 3 | 删 DB 27 行空正文占位 | `materials` **959 → 932**，空正文 **27 → 0** |
| 4 | 归档删除候选目录 | `删除候选_20261002/` → `pre_s10_.../deleted_candidates_20261002/`（20 文件 SHA 校验一致） |

**两套备份（永久保留，不删）**：
- `09_冷备份/pre_s10_20261011_0148/` —— S10 前 DB + 被删行 JSON + 归档目录
- `09_冷备份/pre_renumber_20261010_1505/` —— 重编号前 818 卡快照 + 细纲 + funnel.yaml

**验收（归一化内容比对）**：

- `pre_renumber_..._20261010_1505/02_资料库` 626 张卡片 → 删行后仍 **626/626 在库，零丢失**。
- 19 张删除候选（蓝星篇 18 + ORG-99-012）**内容不在库**——本就是废弃草稿，符合方案A。
- `cargo test --workspace` 全通过；`PRAGMA integrity_check` = ok。
- `neim verify` 现正确地报「⚠ 未比对…932 行全部跳过」exit 3（不再假阳性）。

### S10 后：流水线运行时目录已恢复

细纲/funnel.yaml **不在 DB**（`chapters` 表仅 2 行），故需恢复：

```bash
tools/s10_restore_runtime.sh    # 恢复 03_叙事规划（含细纲）+ 05_管线框架
```

- `03_叙事规划/` —— 1800 章细纲（10 卷 × 180 章）✅ 已恢复
- `05_管线框架/funnel.yaml` —— funnel 工具读它 ✅ 已恢复（`root` 已修正为真实路径）
- `02_资料库/` —— **故意不恢复**（卡片正文 100% 已在 `materials`，由 DB 回退供给）

---

## 🔧 管线改造（2026-10-11，commit 待推）

| 项 | 改动 | 验证 |
|---|---|---|
| **卡片 DB 化** | `ne_injection` 新增 `CardSource`：**磁盘优先 → DB 回退**（读 `materials.content`），含 `renumber_map` 旧编号兜底 | 单测 3 项 + 真机 `db_only` 跑通 |
| **前缀表修复** | `locate_card` 的 `前缀→目录` 表补齐**新前缀**（原缺 `PILL`/`LOCA`/`FORE`/`CLTR` 等），否则磁盘有卡也报 missing | 单测抓出并修复 |
| **funnel.yaml** | 从冷备份恢复 + `root` 修正为真实路径 + 标注 L5 现状（第2章缺） | `funnel` 工具跑通：7 层，L0–L4 done |
| **章节缺口识别** | 新增 `chapter_plan`：检测编号缺口，给「补缺口 / 向后续写」两模式，**默认补缺口** | 实测卷01 → `first_gap=2, suggest_append=4` |

**MCP e2e**：50/50 通过（含新增 `card_source` / `chapter_plan` 字段）。

---

## 📌 新增文档与脚本（2026-10-11）

- `docs/撰稿Agent配置.md` — 《残响之河》撰稿 Agent 配置（§0 风险说明 → §6 启动指令 → §7 边界），
  **纠正外部旧版四处硬伤**（书名/主角/MCP工具名/表名）。
- `tools/s10_rollback.sh` — S10 回滚脚本（已实测；**修复了 `--dry-run` 仍落盘的 bug** + 幂等防呆）。
- `tools/s10_restore_runtime.sh` — S10 后恢复运行时目录（细纲/funnel.yaml；02_资料库 故意不恢复）。

---

## ⚠ 待用户决策：细纲与两套剧情 → ✅ 已按选项A 执行（2026-10-11）

**取证结论**：正式细纲 `卷01.md` 为「青牛镇/太玄宗」开局；旧开局词
`渊汐城`/`沉渊巷` 在 **1800 章细纲中出现 0 次**，仅 卷10 一处「沉渊巷旧口径已作废」备注。
→ **旧开局作废，选项A 成立。**

**执行（`tools/purge_old_opening.py --apply`，可回滚）**：

| 动作 | 结果 |
|---|---|
| 归档 | `pre_s10_20261011_0148/old_chapter_dump/`（chapters JSON + foreshadow JSON + 磁盘正文副本） |
| 备份 | `novel.db.before-chapter-purge-20261011_022646` |
| 删 `chapters` | 2 → **0**（第1、3章，旧开局 渊汐城/沉渊巷） |
| 删 `foreshadow` | 12 → **0**（**全部**锚定旧开局章标题，逐条比对正式细纲标题无一相符） |

**为何连伏笔一起清**：12 条伏笔会经 `ne_inject` 注入新第1章上下文包，Agent 将看到
「沉渊巷/无编码体」→ 正是禁止的撕裂源。留着它，选项A 不成立。

**验证**：`chapter_plan` 重算 → `generated_chapters=[]`, `has_gap=false`, `default_target=1`；
注入包旧开局词泄漏扫描 **0**（`推演面板` 除外，见下）；`integrity_check=ok`。

**⚠ 一处自我纠错**：脚本初版误将 `推演面板` 列为旧开局标记。经取证，
`推演面板` 是**正式设定**（EVT-99-005 定稿·D4正典：金手指=推演面板U0–U8+存在之基；
卷01.md 出现 5 次、全文「推演」147 次）。已从标记中剔除；**未造成误删**
（被删行命中的是 渊汐城/沉渊巷/无编码，与推演面板无关）。

### 📌 后续备案（未执行）

- 12 条作废伏笔**需按新开局重建**（`FORE-*` 表当前为空）。
- **细纲入库工程**（独立立项）：把 1800 章细纲批量写入 `chapters.outline`，
  完成后 `ne-mcp` 可彻底去掉磁盘细纲依赖。**本次不做**（涉及 ID 对齐、版本冲突、解析容错）。
- `materials` 284 行新旧编号重复卡片：**暂不清理**，待细纲入库工程后单独做去重。

## ⚠ 已记录的数据质量项（未擅自动手）

- `materials` 932 行中 **284 行是重复行**：同一张卡同时存旧编号（`MTC-001`）与新编号（`PILL-99-001`），
  内容差 4 字（H1 的 `CODE｜` 前缀）。待清理。

---

## 任务目标

用户要求：综合其提供的模板 → 已有的完善、没有的建立 → 建好框架后**全量导入 DB（按格式）**
→ **重新分配编号** → **建索引目录** → **原数据文档删除**（用户明确：*全部导入后审查确认再删*）。

---

## 已完成

### 1. 记录类（已提交 d5ed003）

- `docs/specs/叙事模板全集.md` — 用户给的 8 模板(A–H) + 17 模板(I–Y) + 7 补丁表(24–30)
- `docs/DECISIONS-narrative.md` — 6 决策 + 3 门禁设计
- `docs/sql/*.sql` — 分批建表脚本
- `docs/specs/编号规范-v3.md` — 全库重编号方案（**新**）

### 2. 代码

- `crates/ne-import/`（新 crate，20 项测试全过）
  - `scan_library` / `import_cards` / `record_ledger` / `verify_import`
  - `scan_docs` / `import_docs` / `verify_docs`
  - `renumber::{build_map, map_prefix, save_map, rewrite_refs, rewrite_files,
    rename_files, fix_h1_codes, ensure_h1_code, find_dangling}`
  - `bin/neim.rs` — 子命令：`scan | import | map | renumber | ensure-h1 |
    dangling | docs | verify | verify-docs | index`
- `crates/ne-db` — F1 修复（Dialect 双 schema 探测）
- `crates/ne-engine` — F3 修复（去卷章数硬编码）

### 3. 数据库（真实库 `/sdcard/.../资料库/novel.db`）

| 项 | 结果 |
|---|---|
| 表数 | 39 → **64**（业务 59 + FTS 影子 5；含 `sqlite_sequence` 则 65） |
| 卡片全文 | **0 张 → 959 行**（净字 1,527,651；932 行有正文 + 27 行空） |
| 文档 | 277 个 / 2,720,000 字级 |
| 对账（卡片） | ✅ **内容归一比对 626/626 全命中**（零缺口，2026-10-11 复核） |
| 对账（文档） | ✅ 通过 |
| 重编号 | 生成 `renumber_map` 969 行；⚠ **磁盘未改名**（见下） |
| H1 补编号 | 70 处 |
| 悬空引用 | 11 条（**= 重编号前基线，零新增**） |

### 4. 备份

- `/sdcard/.../资料库/09_冷备份/pre_renumber_20261010_1505/`
  - `novel.db`（原始 40 表版）+ `02_资料库`（818 卡）+ `03_叙事规划`（27 文件）

### 5. 文档层补全（2026-10-11 新增）

- `docs/全项目目录.md` — 仓库内部结构一览（9 crate 分工表 + docs/ + 真值库 + 续跑命令）
- `docs/数据库表速查.md` — 真库 64 表一览（行数 / 批次 / code 前缀）+ 新旧前缀映射 + **内容对账**

### 6. 修复：`neim verify` 假阳性（2026-10-11）

原实现把"读不到源文件"的行 `continue` 跳过，源目录不存在时 959/959 全跳过，
仍打印 **"✅ 对账通过：磁盘与 DB 字数一致"** —— 是"无法对账"却报"通过"。

修复：

- `ne_import::verify_import` 返回值改为 `VerifyReport { diffs, compared, skipped }`，
  把"比对了几行 / 跳过了几行"暴露给调用方。
- `neim verify`：`compared == 0 && skipped > 0` → 打印"⚠ 未比对"并 **exit 3**；
  有差异 → exit 1；真通过才打印 ✅。
- 新增回归测试 `verify_import_does_not_pass_when_source_missing`（ne-import 21 项全过）。

实测（源已不在）：`neim verify` → `⚠ 未比对…959 行全部跳过`，`exit=3`。

---

## ⚠ 2026-10-11 复核：与上文"已完成"不符的三处事实

对真库与磁盘重新取证后，发现上文数字**需修正**（详见 `docs/数据库表速查.md`）：

| 上文写的 | 实测 | 说明 |
|---|---|---|
| "626 文件全部改为 v3 编号" | **磁盘上没有任何 `CODE｜名称.md` 文件**（全盘命中 0） | 改名**未落盘**，只生成了 `renumber_map` 映射 |
| "对账（卡片）✅ 磁盘 == DB" | 内容归一比对：冷备份 **626/626 全部命中**，零缺口 | 这条**已用更强方法证实**（见下） |

**新增取证结论（2026-10-11）**：

- 冷备份 `pre_renumber_20261010_1505/02_资料库` 的 **626 张可导入卡片，内容 100% 已在 DB**（零缺口）。
- 全库**唯一真缺口 = 19 张**：`删除候选_20261002/蓝星文明篇/CHAR-70-001~018` + `蓝星文明/组织/ORG-99-012`。
- §四的"27 行空正文"实为 **8 行早期重复占位**（同内容已有 `CIVI-99-9xx` 正式行）+ **19 行真缺口**。
- `materials.file_path` 逐行判定：磁盘可定位 575 / 磁盘无但 DB 有正文 365 / 两头空 19。

**live 根下 `02_资料库` 已不存在**（只剩 `04_正文` / `09_冷备份` / `INDEX.md` / `ai-layer` / `novel.db` / `scripts`）。

---

## 剩余未做

### ① 映射表历史部分丢失 —— ✅ 已修复（`a0bdafd`）

`renumber_map` 主键从 `old_code` 改到 `old_path` 时被 `DROP` 重建，丢失的「旧→新」变更记录
已用内容归一哈希匹配**重建并固化**为 `neim map-rebuild` 子命令（`renumber.rs::map_rebuild`）。

**实测现状**（2026-10-11）：969 行 = **343 行 `old_code≠new_code`**（284 重建 + 59 无编号）+ 626 行同号保留。
校验 `ORG-99-001 → CIVI-99-001`、`EVT-99-001 → EVNT-99-001`、`MTC-011 → PILL-99-011`、
`TECH-99-003 → POWR-99-003` 均正确。

### ② 索引目录 —— ✅ 已完成（`a0bdafd`）

`catalog_index` 现有 **1210 file 行 + 99 dir 行**，其中**带 code 的卡片节点 932 个**
（卡片已纳入索引）。此前的"278 file 行 / 卡片未纳入"是旧状态。

### ③ 审查 + 删源 —— ✅ 前置校验已完成（2026-10-11）

见本文顶部「S10 前置清理」。**已执行**：备份、删 27 行空正文占位、归档删除候选目录、回滚脚本。
**待用户放行**：确认后即可宣告 S10 完成（`02_资料库` 已不在 live 根）。

### ④ 提交推送

`395920f`：docs 全项目目录 + 数据库表速查 + fix(neim) verify 假阳性。（已推送）

**本轮待提交**：
- 新增 `docs/撰稿Agent配置.md`
- 新增 `tools/s10_rollback.sh`（已实测）
- 更新 `PROGRESS.md`（S10 前置清理记录）

---

## 关键约束（续跑必读）

1. **删源前必须用户确认** —— 用户原话："全部导入 db 之后你审查一遍，确定全部导入之后就删除"
2. **WAL 三文件**：`novel.db` + `-wal` + `-shm` 备份须一起拷
3. **幂等**：`build_map` 已幂等（v3 前缀放行）；但 `TECH` 是特例（既是 v3 前缀又是待拆分旧前缀，按目录判定）
4. **无编号卡**用 900+ 号段（`CIVI-99-901` 等），避免与正式号冲突
5. **`.`bak` 文件**（202 个）是重编号安全备份，删源时可一并删

---

## 校验命令（续跑用）

```bash
R="/sdcard/Download/小说相关备份/projects/projects/系统/资料库"
cd /workspace/小说系统
cargo test --workspace                           # 全量（ne-import 21 项，含 verify 回归）

./target/release/neim scan         --root "$R"   # 卡片扫描（只读）
./target/release/neim dangling     --root "$R"   # 悬空引用（基线 11）

# ⚠ 下面两条依赖 `$R/02_资料库`，而该目录当前已不存在：
#   verify      → 现在会明确报「⚠ 未比对…959 行全部跳过」并 exit 3（修复前是谎报 ✅ 通过）
#   verify-docs → 同理
./target/release/neim verify       --root "$R"
./target/release/neim verify-docs  --root "$R"

# 若源已挪到冷备份，可指向备份卡目录对账：
BK="$R/09_冷备份/pre_renumber_20261010_1505"
./target/release/neim verify       --root "$BK"   # 注意：DB 仍在 $R/novel.db，见源码 --root 语义
```
