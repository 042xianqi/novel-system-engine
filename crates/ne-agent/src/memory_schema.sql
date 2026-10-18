-- 小说智能体 · 记忆子系统 schema（2026-10-14）
--
-- 设计原则（对应需求文档 §一 / §五）：
--   * long_term_memory  ：不变的静态设定（世界观铁则/角色档案/锚点元/分卷大纲）
--   * mid_term_snapshot ：变化的剧情状态（每 30 章强制快照，支持人工回滚）
--   * short_term_memory ：临近事件（8 章窗口，超限推入中期快照队列）
--   * chapter_meta      ：章节溯源存档（原始细纲/压缩细纲/正文/token 统计）
--   * system_config     ：运行参数（token 上限等，可人工调整）
--
-- 隔离原则：本 schema 只新增表，**不改动 novel.db 既有 65 表**。
-- 原始 md 仅作为一次性导入源，运行期不再读取（见 §五.2）。

-- ── 长期记忆：静态设定（AI 生成链路只读，仅人工 api 可写）
CREATE TABLE IF NOT EXISTS long_term_memory (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    category    TEXT NOT NULL,              -- 世界观铁则 | 角色静态档案 | 锚点元 | 分卷大纲 | 顶层设定
    key         TEXT NOT NULL,              -- 条目标识（如「金手指时序铁则」「陈寂」）
    content     TEXT NOT NULL,              -- 自然语言约束文本
    priority    INTEGER NOT NULL DEFAULT 100, -- 越小越优先（裁剪时先保留）
    source      TEXT DEFAULT '',            -- 导入来源（md 路径，仅溯源用）
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at  TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(category, key)
);

-- ── 中期快照：动态剧情状态（每 30 章）
CREATE TABLE IF NOT EXISTS mid_term_snapshot (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    snapshot_id   TEXT NOT NULL UNIQUE,     -- SNAP-0001
    upto_chapter  INTEGER NOT NULL,         -- 快照覆盖到第几章
    payload       TEXT NOT NULL,            -- JSON：char_states / anchors / items
    warnings      TEXT NOT NULL DEFAULT '[]', -- JSON array，模糊歧义标记
    created_at    TEXT NOT NULL DEFAULT (datetime('now'))
);

-- ── 短期记忆：临近事件（8 章窗口）
CREATE TABLE IF NOT EXISTS short_term_memory (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    volume      TEXT NOT NULL,
    chapter     INTEGER NOT NULL,
    summary     TEXT NOT NULL,              -- 结构化压缩要点（非原文）
    pushed      INTEGER NOT NULL DEFAULT 0, -- 1 = 已推入中期快照队列
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(volume, chapter)
);

-- ── 章节元数据：溯源存档（原始细纲绝不进 prompt）
CREATE TABLE IF NOT EXISTS chapter_meta (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    volume             TEXT NOT NULL,
    chapter            INTEGER NOT NULL,
    raw_outline        TEXT,                -- 原始 L3 细纲（仅存档）
    compressed_outline TEXT,                -- 压缩版（进 SYS_LAYER_2）
    content            TEXT,                -- 生成正文
    snapshot_id        TEXT,
    input_tokens       INTEGER DEFAULT 0,
    output_tokens      INTEGER DEFAULT 0,
    warnings           TEXT DEFAULT '[]',
    status             TEXT NOT NULL DEFAULT 'ok',
    created_at         TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(volume, chapter)
);

-- ── 系统配置：运行参数
CREATE TABLE IF NOT EXISTS system_config (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    note       TEXT DEFAULT '',
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- 索引（按章号查询是主路径）
CREATE INDEX IF NOT EXISTS idx_stm_vol_chap ON short_term_memory(volume, chapter);
CREATE INDEX IF NOT EXISTS idx_meta_vol_chap ON chapter_meta(volume, chapter);
CREATE INDEX IF NOT EXISTS idx_snap_upto ON mid_term_snapshot(upto_chapter);
CREATE INDEX IF NOT EXISTS idx_ltm_cat ON long_term_memory(category, priority);
