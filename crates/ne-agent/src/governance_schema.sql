-- 治理附加 schema（2026-10-14）
-- 原则：只新增、只幂等、不改既有表逻辑。
-- risk_index：所有 audit/library 工具的 WARN/HIGH-RISK 入库点。

CREATE TABLE IF NOT EXISTS risk_index (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    risk_type  TEXT NOT NULL,          -- layer_health | library | compliance | refs
    volume     TEXT,                   -- 卷（可空＝全库）
    chapter    INTEGER,                -- 章（可空）
    source     TEXT NOT NULL,          -- 产生该风险的工具/命令
    message    TEXT NOT NULL,          -- 风险描述
    level      TEXT NOT NULL DEFAULT 'WARN', -- WARN | HIGH-RISK | OK
    ts         TEXT NOT NULL DEFAULT (datetime('now')),
    resolved   INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_risk_unresolved ON risk_index(resolved, risk_type, volume);

-- 层级治理索引：library-ctl init 产出的标准索引（只建索引，不动原文件）。
CREATE TABLE IF NOT EXISTS library_index (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    layer      TEXT NOT NULL,          -- L0 | L0.5 | L1 | L1x | GUARD | ANCHOR | L2 | L3 | L4
    category   TEXT,
    key        TEXT NOT NULL,
    source     TEXT,                   -- 相对路径或 DB 来源
    chars      INTEGER DEFAULT 0,
    types      TEXT DEFAULT '[]',      -- JSON：该条目所含 code 前缀集合
    title      TEXT DEFAULT '',        -- H1 标题
    tags       TEXT DEFAULT '[]',      -- JSON：标签集合
    dirty      INTEGER NOT NULL DEFAULT 0, -- 1 = 缺 category/key/type 等脏标记
    dirty_note TEXT,
    ts         TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(layer, key)
);
