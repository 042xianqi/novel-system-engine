# SQL 建表脚本（分批）

> ⚠️ **执行前必读**
> 1. **先备份 WAL 三文件**：`novel.db` + `novel.db-wal` + `novel.db-shm`
> 2. 脚本全部用 `CREATE TABLE IF NOT EXISTS`，**可重复执行**
> 3. **默认不执行** —— 需人工确认后手动跑
> 4. 现有库已有 40 张表；本目录**只含真空白的新表**，
>    概念重叠的表（map_state/power_state/…）走 `ALTER TABLE ADD COLUMN`，见 `09_现有表加列.sql`

## 批次

| 文件 | 批 | 表数 | 说明 |
|---|---|---|---|
| `01_A批_优先.sql` | A | 8 | 补最痛空白，服务演算引擎 |
| `02_B批_次优.sql` | B | 8 | 写到 300 万字后才用得上 |
| `03_C批_长尾.sql` | C | 3 | 长尾 |
| `09_现有表加列.sql` | — | — | 给已有表补字段（不新建） |

## 统一约定（所有新表都遵守）

```sql
code TEXT,         -- 编号，与 materials.code 对齐
source_card TEXT,  -- 源 Markdown 路径（若该记录源自卡片）
source_sha TEXT,   -- 源文件指纹，防漂移
basis TEXT,        -- '实值' | '推测' | '待核'
```

与现有 `*_state` 投影体系**同构** —— 这是硬要求，详见 `docs/DECISIONS-narrative.md` 决策 2。
