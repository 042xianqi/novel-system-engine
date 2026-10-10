# 进度存档：资料库数据层重构

> 更新：2026-10-10 · 状态：**未完成，可续跑**

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
| 表数 | 40 → **65** |
| 卡片全文 | **0 张 → 626 张**（净字 1,011,343） |
| 覆盖率 | **0% → 100%** |
| 文档 | 278 个 / 2,716,800 字 |
| 对账（卡片） | ✅ 磁盘 == DB |
| 对账（文档） | ✅ 通过 |
| 重编号 | 626 文件全部改为 v3 编号 |
| H1 补编号 | 70 处 |
| 悬空引用 | 11 条（**= 重编号前基线，零新增**） |

### 4. 备份

- `/sdcard/.../资料库/09_冷备份/pre_renumber_20261010_1505/`
  - `novel.db`（原始 40 表版）+ `02_资料库`（818 卡）+ `03_叙事规划`（27 文件）

---

## 剩余未做

### ① 映射表历史部分丢失（**已知缺陷，需修**）

`renumber_map` 主键从 `old_code` 改到 `old_path` 时被 `DROP` 重建，
丢失了 365 条「旧→新」变更记录；重跑 `build_map` 时因幂等化（v3 前缀放行）
又只补回 59 条无编号卡。

**最后一步尝试用内容归一哈希匹配重建**（`/tmp/rebuild3.py`），
得 284 条，校验 `ORG-99-001 → CIVI-99-001`、`EVT-99-001 → EVNT-99-001`、
`MTC-011 → PILL-99-011`、`TECH-99-003 → POWR-99-003` 均**正确**。

**待办**：把该重建逻辑固化为 `neim map-rebuild` 子命令 + 落测试，
并核对总数（应为 306 条变更 + 59 无编号 = 365）。

### ② 索引目录

`INDEX.md` 已导出（278 文件 / 59 目录）。
`catalog_index` 有 278 file 行 + 59 dir 行 —— 但**卡片未纳入 catalog_index**，
需确认是否要把 626 张卡也建索引节点。

### ③ 审查 + 删源（**未做，需用户确认**）

用户要求"审查一遍确定全部导入之后才删除"。**尚未删任何源文件。**

待删（用户确认后）：
- `02_资料库/*.md`（818 个，含 202 个 `.md.bak`）
- 其余源（`03_叙事规划` / `05_管线框架` / `07_勘误工坊` / `00_系统配置`）

### ④ 提交推送

`ne-import` crate、`docs/specs/编号规范-v3.md`、`PROGRESS.md` 均**未提交**。

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
./target/release/neim scan      --root "$R"      # 卡片扫描
./target/release/neim verify    --root "$R"      # 卡片对账
./target/release/neim verify-docs --root "$R"    # 文档对账
./target/release/neim dangling  --root "$R"      # 悬空引用（应 11）
cargo test -p ne-import                          # 20 项
```
