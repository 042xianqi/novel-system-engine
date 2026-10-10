-- ============================================================
-- 《残响之河》情绪动力引擎 P0 · 4 表 DDL
-- 生成：2026-09-30 · k6 4表DDL+灌库
-- 用法：
--   灌库：python3 db/emuload.py --db /path/novel.db
--   建表：python3 -c "import sqlite3; con=sqlite3.connect('novel.db'); con.executescript(open('db/emotion_schema.sql').read())"
--
-- 铁律：
--   * 演算层 0 LLM 0 随机 可重放；本 DDL 只建表不灌数据
--   * 0 写盘 02_资料库（只读提取）；提不出写 null+「待标定」禁编
--   * 10 维情绪向量列名 = g4 定案英文名（与 k5 engine.EMO_DIMS 同序）
--   * basis 三态：实值 / 推断 / null默认值（emuload.py 校验拒空值）
--   * OTHER 前缀拒灌（emuload.py 校验）
-- ============================================================

-- ============================================================
-- char_emotion：角色情绪向量快照（按章记录）
-- k5 emotion_engine.to_char_emotion_rows(new_state) 产出行
-- 列：char_code, chapter, 10 维列, dominant, strength
-- 唯一索引(角色,章)：同一角色同一章只允许一条情绪快照
-- ============================================================
CREATE TABLE IF NOT EXISTS char_emotion (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    char_code   TEXT NOT NULL,              -- CHAR-99-001
    char_name   TEXT,                       -- 陈寂
    chapter     INTEGER NOT NULL,           -- 章号
    -- 10 维内在情绪向量（0-100，g4 口径英文名）
    anger       REAL,
    fear        REAL,
    grief       REAL,
    joy         REAL,
    anxiety     REAL,
    calm        REAL,
    shame       REAL,
    awe         REAL,
    compassion  REAL,
    hope        REAL,
    -- 外在表现向量（E_show = E_inner × Suppress，0-100）
    anger_show  REAL,
    fear_show   REAL,
    grief_show  REAL,
    joy_show    REAL,
    anxiety_show REAL,
    calm_show    REAL,
    shame_show   REAL,
    awe_show     REAL,
    compassion_show REAL,
    hope_show    REAL,
    dominant    TEXT,                       -- 主导情绪维度名（最大项；全 0 时 NULL）
    strength    REAL,                       -- 向量总强度 √(Σ e²)
    suppress    REAL,                       -- 压抑系数 0-1（S7 用）
    basis       TEXT,                       -- 标定来源标记（k1 灌入时记录）
    CHECK (dominant IN ('anger','fear','grief','joy','anxiety','calm','shame','awe','compassion','hope') OR dominant IS NULL),
    UNIQUE(char_code, chapter)
);

CREATE INDEX IF NOT EXISTS idx_char_emotion_char ON char_emotion(char_code);
CREATE INDEX IF NOT EXISTS idx_char_emotion_chap ON char_emotion(chapter);

-- ============================================================
-- char_memory：角色记忆条目（事件→记忆 衰减记录）
-- k5 emotion_engine.to_char_memory_rows(new_state) 产出行
-- 列：char_code, event_id, source_chapter, dim, e0, e_t
-- 一条记忆 = 一个角色 + 一个事件 + 一个维度
-- ============================================================
CREATE TABLE IF NOT EXISTS char_memory (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    char_code   TEXT NOT NULL,              -- CHAR-99-001
    event_id    TEXT,                       -- EVT-100（来源事件编号；测试事件可空）
    source_chapter INTEGER,                 -- 产生记忆的章号
    dim         TEXT,                       -- 记忆归属维度（g4 10 维英文名）
    e0          REAL,                       -- 记忆初值 E_0（0-100）
    e_t         REAL,                       -- 当前章残响 E_t = E_0·e^(−λ·t)；NULL = 未衰减
    lambda_mem  REAL,                       -- 记忆衰减档（0.95/0.9/0.85/0.7）
    basis       TEXT,                       -- 标定来源标记
    CHECK (dim IN ('anger','fear','grief','joy','anxiety','calm','shame','awe','compassion','hope') OR dim IS NULL),
    UNIQUE(char_code, event_id, source_chapter, dim)
);

CREATE INDEX IF NOT EXISTS idx_char_memory_char ON char_memory(char_code);
CREATE INDEX IF NOT EXISTS idx_char_memory_evt  ON char_memory(event_id);

-- ============================================================
-- event_template：事件模板（k3 提取产物：46 条）
-- 列：event_id, name, trigger, i, affected_roles, emotion_delta
-- i = 事件基础强度 0-1（k3 全 null → 默认 0.5，本表存 null 标记待标定）
-- emotion_delta = 10 维 delta 模板（JSON 字符串，k3 全 null）
-- affected_roles = 受影响角色编号列表（JSON 数组字符串）
-- trigger = 触发条件描述（30 实值 + 11 前序拼接）
-- ============================================================
CREATE TABLE IF NOT EXISTS event_template (
    id                  TEXT PRIMARY KEY,  -- EVT-100
    name               TEXT NOT NULL,      -- 事件名
    trigger            TEXT,               -- 触发条件描述（实值/前序拼接/NULL 待标定）
    trigger_basis      TEXT,               -- 实值 / 前序拼接 / null默认值
    i                  REAL,               -- 事件基础强度 0-1（NULL = 待标定，引擎默认 0.5）
    i_basis            TEXT,               -- 标定来源
    affected_roles     TEXT,               -- JSON 数组：受影响角色（k3 存角色名）
    emotion_delta      TEXT,               -- JSON 对象：{dim: delta_val}（NULL = 待标定）
    emotion_delta_basis TEXT,             -- 标定来源
    lambda_tier        TEXT,               -- 记忆衰减档 "0.95"/"0.9"/"0.85"/"0.7"（P0 默认 "0.85"）
    basis              TEXT,               -- 来源说明（k3 basis 字段全文）
    source_card        TEXT,               -- 来源卡路径（02_资料库/事件/EVT-xxx.md）
    created_at         TEXT DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_event_tpl_name ON event_template(name);

-- ============================================================
-- world_state：文明份额状态（按卷+章记录）
-- 列：civilization, volume, chapter, share
-- P0 建表结构就绪；灌库依赖 k3 事件 trigger 提取的文明名
-- share = 该文明在当前章节的势力份额 0-100（NULL = 待标定）
-- ============================================================
CREATE TABLE IF NOT EXISTS world_state (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    civilization TEXT NOT NULL,             -- 文明名（玄脉宗/铸星联邦/...）
    volume      TEXT,                       -- 卷01
    chapter     INTEGER,                    -- 章号
    share       REAL,                       -- 势力份额 0-100（NULL = 待标定）
    notes       TEXT,                       -- 备注
    UNIQUE(civilization, volume, chapter)
);

CREATE INDEX IF NOT EXISTS idx_world_state_vol ON world_state(volume, chapter);
CREATE INDEX IF NOT EXISTS idx_world_state_civ ON world_state(civilization);

-- ============================================================
-- W1 全表族扩展（k1 DDL 总建 · 2026-10-01）
-- 全量提取总方案：25 张表族 + chapter_log 审计表
-- 公共列统一约定（15 新状态表 / 知识层 3 表 / chapter_log）：
--   code          TEXT NOT NULL   -- 卡编号主键（PK 或 UNIQUE 一部分）
--   chapter       INTEGER NOT NULL -- 章锚点：静态表 ch=0（初值快照），动态表 ch=N（tick 回写）
--   basis         TEXT           -- 标定来源三态：实值 / 推断 / null默认值
--   source_card   TEXT           -- 02_资料库 相对路径（冷备份溯源）
--   source_sha    TEXT           -- 源卡 sha256 前 12 位（与 09_冷备份 manifest.sha256 同口径，弃 md5）
--   created_at    TEXT DEFAULT (datetime('now'))
-- 存量 4 表（char_emotion/char_memory/event_template/world_state）保持原结构不动
-- 灌库映射：02_资料库 19 类实卡 → 各表 ch=0 静态层一次灌
-- ============================================================

-- ============ 15 类状态表族 ============

-- 世界观（L0 总纲级静态快照，6 张无编号卡，code=文件名 slug）
CREATE TABLE IF NOT EXISTS lore_state (
    code          TEXT PRIMARY KEY,
    name          TEXT,
    layer         TEXT,
    core_law      TEXT,
    mainline      TEXT,
    status        TEXT,
    chapter       INTEGER NOT NULL DEFAULT 0,
    basis         TEXT,
    source_card   TEXT,
    source_sha    TEXT,
    created_at    TEXT DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_lore_name ON lore_state(name);

-- 文明（CIV 主卡；子层 ORG/SYS/CULT 挂 civ_sub_state）
CREATE TABLE IF NOT EXISTS civ_state (
    code          TEXT PRIMARY KEY,
    name          TEXT,
    realm         TEXT,
    type          TEXT,
    baseline_share REAL,
    stance        TEXT,
    chapter       INTEGER NOT NULL DEFAULT 0,
    basis         TEXT,
    source_card   TEXT,
    source_sha    TEXT,
    created_at    TEXT DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_civ_state_name ON civ_state(name);
CREATE INDEX IF NOT EXISTS idx_civ_state_chap ON civ_state(chapter);

-- 文明子层（ORG 组织 / SYS 制度 / CULT 文明内文化，挂在 civ_code 下）
CREATE TABLE IF NOT EXISTS civ_sub_state (
    code          TEXT PRIMARY KEY,
    civ_code      TEXT,
    sub_kind      TEXT,
    name          TEXT,
    summary       TEXT,
    chapter       INTEGER NOT NULL DEFAULT 0,
    basis         TEXT,
    source_card   TEXT,
    source_sha    TEXT,
    created_at    TEXT DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_civ_sub_civ ON civ_sub_state(civ_code);
CREATE INDEX IF NOT EXISTS idx_civ_sub_kind ON civ_sub_state(sub_kind);

-- 地图（LOC-*）
CREATE TABLE IF NOT EXISTS map_state (
    code          TEXT PRIMARY KEY,
    name          TEXT,
    civ           TEXT,
    space_layer   TEXT,
    ecology       TEXT,
    status        TEXT,
    chapter       INTEGER NOT NULL DEFAULT 0,
    basis         TEXT,
    source_card   TEXT,
    source_sha    TEXT,
    created_at    TEXT DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_map_name ON map_state(name);
CREATE INDEX IF NOT EXISTS idx_map_civ ON map_state(civ);

-- 科技（TECH-* / ART-99 科技段）
CREATE TABLE IF NOT EXISTS tech_state (
    code          TEXT PRIMARY KEY,
    name          TEXT,
    civ           TEXT,
    tier          TEXT,
    energy_source TEXT,
    principle     TEXT,
    side_effect   TEXT,
    state         TEXT,
    chapter       INTEGER NOT NULL DEFAULT 0,
    basis         TEXT,
    source_card   TEXT,
    source_sha    TEXT,
    created_at    TEXT DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_tech_name ON tech_state(name);
CREATE INDEX IF NOT EXISTS idx_tech_civ ON tech_state(civ);

-- 力量体系（ART-99 力量段 + 体系力量 YGEN）
CREATE TABLE IF NOT EXISTS power_state (
    code          TEXT PRIMARY KEY,
    name          TEXT,
    civ           TEXT,
    realm_strat   TEXT,
    cross_ref     TEXT,
    origin        TEXT,
    path          TEXT,
    side_effect   TEXT,
    chapter       INTEGER NOT NULL DEFAULT 0,
    basis         TEXT,
    source_card   TEXT,
    source_sha    TEXT,
    created_at    TEXT DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_power_name ON power_state(name);

-- 境界（0 实卡，占位建表；建卡即灌）
CREATE TABLE IF NOT EXISTS realm_state (
    code          TEXT PRIMARY KEY,
    civ           TEXT,
    layer_name    TEXT,
    layer_idx     INTEGER,
    upper_limit   TEXT,
    chapter       INTEGER NOT NULL DEFAULT 0,
    basis         TEXT,
    source_card   TEXT,
    source_sha    TEXT,
    created_at    TEXT DEFAULT (datetime('now'))
);

-- 装备（ART-99 装备段，总录+单件）
CREATE TABLE IF NOT EXISTS item_state (
    code          TEXT PRIMARY KEY,
    name          TEXT,
    civ           TEXT,
    tier          TEXT,
    material      TEXT,
    func          TEXT,
    state         TEXT,
    chapter       INTEGER NOT NULL DEFAULT 0,
    basis         TEXT,
    source_card   TEXT,
    source_sha    TEXT,
    created_at    TEXT DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_item_name ON item_state(name);

-- 装备条目（总录卡内 H3 子条目，两级；code 复合键：父卡号-条目名）
CREATE TABLE IF NOT EXISTS item_entry (
    code          TEXT PRIMARY KEY,
    parent_code   TEXT,
    name          TEXT,
    maker         TEXT,
    material      TEXT,
    spec          TEXT,
    func          TEXT,
    user          TEXT,
    price         TEXT,
    skill         TEXT,
    appear_ch     TEXT,
    maintain      TEXT,
    damage_thr    TEXT,
    basis         TEXT,
    source_card   TEXT,
    created_at    TEXT DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_item_entry_parent ON item_entry(parent_code);

-- 丹药（MTC-*）
CREATE TABLE IF NOT EXISTS elixir_state (
    code          TEXT PRIMARY KEY,
    name          TEXT,
    category      TEXT,
    origin        TEXT,
    rarity        TEXT,
    use           TEXT,
    civ           TEXT,
    chapter       INTEGER NOT NULL DEFAULT 0,
    basis         TEXT,
    source_card   TEXT,
    source_sha    TEXT,
    created_at    TEXT DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_elixir_name ON elixir_state(name);

-- 天材地宝（ART-99 天材段 11 张；前缀 ART-99-004~015，TRE 旧体系不建）
CREATE TABLE IF NOT EXISTS treasure_state (
    code          TEXT PRIMARY KEY,
    name          TEXT,
    grade         TEXT,
    origin        TEXT,
    effect        TEXT,
    civ           TEXT,
    chapter       INTEGER NOT NULL DEFAULT 0,
    basis         TEXT,
    source_card   TEXT,
    source_sha    TEXT,
    created_at    TEXT DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_treasure_name ON treasure_state(name);

-- 诡异（ANC-*，动态表 tick 回写）
CREATE TABLE IF NOT EXISTS anomaly_state (
    code          TEXT PRIMARY KEY,
    name          TEXT,
    source        TEXT,
    ecotone       TEXT,
    source_civ    TEXT,
    danger        INTEGER,
    counter       TEXT,
    active        INTEGER DEFAULT 0,
    chapter       INTEGER NOT NULL,
    basis         TEXT,
    source_card   TEXT,
    source_sha    TEXT,
    created_at    TEXT DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_anomaly_chap ON anomaly_state(chapter);
CREATE INDEX IF NOT EXISTS idx_anomaly_name ON anomaly_state(name);

-- 跨文明关系（ORG/SYS/CULT 99 段，动态表）
CREATE TABLE IF NOT EXISTS civ_relation_state (
    code          TEXT PRIMARY KEY,
    a_civ         TEXT,
    b_civ         TEXT,
    mode          TEXT,
    state         TEXT,
    chapter       INTEGER NOT NULL,
    basis         TEXT,
    source_card   TEXT,
    source_sha    TEXT,
    created_at    TEXT DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_civrel_chap ON civ_relation_state(chapter);
CREATE INDEX IF NOT EXISTS idx_civrel_a ON civ_relation_state(a_civ);

-- 文化（实体文化卡，CULT 前缀；模板/规范文档卡归知识层不进本表）
CREATE TABLE IF NOT EXISTS culture_state (
    code          TEXT PRIMARY KEY,
    name          TEXT,
    civ           TEXT,
    form          TEXT,
    kind          TEXT,
    chapter       INTEGER NOT NULL DEFAULT 0,
    basis         TEXT,
    source_card   TEXT,
    source_sha    TEXT,
    created_at    TEXT DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_culture_name ON culture_state(name);

-- 时间（TIME-*）
CREATE TABLE IF NOT EXISTS timeline_state (
    code          TEXT PRIMARY KEY,
    phase         TEXT,
    t_point       TEXT,
    key_events    TEXT,
    chapter       INTEGER NOT NULL DEFAULT 0,
    basis         TEXT,
    source_card   TEXT,
    source_sha    TEXT,
    created_at    TEXT DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_timeline_phase ON timeline_state(phase);

-- 规则占位表（势力/功法 0 实卡合并占位，建卡即灌；type 枚举 CHECK）
CREATE TABLE IF NOT EXISTS rule_state (
    code          TEXT PRIMARY KEY,
    name          TEXT,
    type          TEXT,
    civ           TEXT,
    content       TEXT,
    active        INTEGER,
    chapter       INTEGER NOT NULL DEFAULT 0,
    basis         TEXT,
    source_card   TEXT,
    source_sha    TEXT,
    created_at    TEXT DEFAULT (datetime('now')),
    CHECK (type IN ('势力','功法','系统规则','护栏') OR type IS NULL)
);
CREATE INDEX IF NOT EXISTS idx_rule_type ON rule_state(type);

-- ============ 知识层 3 表 ============

-- 勘误知识（115 条，8 域；verdict 四态消费：canon 硬门禁/pending 告警/drift 禁用值/note 记账）
CREATE TABLE IF NOT EXISTS kb_knowledge (
    kbid          INTEGER PRIMARY KEY,
    domain        TEXT NOT NULL,
    card_code     TEXT,
    canon         TEXT NOT NULL,
    basis         TEXT,
    verdict       TEXT NOT NULL,
    layer         TEXT,
    tag           TEXT,
    source        TEXT NOT NULL,
    version       TEXT,
    chapter       INTEGER NOT NULL DEFAULT 0,
    source_card   TEXT,
    source_sha    TEXT,
    created_at    TEXT DEFAULT (datetime('now')),
    CHECK (verdict IN ('canon','pending','drift','note')),
    UNIQUE(domain, card_code, canon, tag)
);
CREATE INDEX IF NOT EXISTS idx_kb_domain ON kb_knowledge(domain);
CREATE INDEX IF NOT EXISTS idx_kb_tag ON kb_knowledge(tag);
CREATE INDEX IF NOT EXISTS idx_kb_verdict ON kb_knowledge(verdict);

-- 技法知识（60 条；量化条目 engine_hook 挂门禁，非量化 null 供 LLM 解释层）
CREATE TABLE IF NOT EXISTS technique_items (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    code          TEXT NOT NULL,          -- 技法 id（技01/G1/R1/...）
    series        TEXT,                  -- G/R/P/D/I/H/W/L/Y/T/S
    name          TEXT NOT NULL,
    applies_to    TEXT,                  -- 章/卷/全书/开篇
    quantified    TEXT,                  -- JSON（可空）
    source        TEXT,                  -- 原有42/新增18
    engine_hook    TEXT,                 -- 驱动哪张表/门禁；null=不进硬门禁
    chapter       INTEGER NOT NULL DEFAULT 0,
    basis         TEXT,
    source_card   TEXT,
    source_sha    TEXT,
    created_at    TEXT DEFAULT (datetime('now')),
    UNIQUE(code)
);
CREATE INDEX IF NOT EXISTS idx_technique_series ON technique_items(series);

-- 结构节拍（S1-S10：卷18章骨架/单元10万字/黄金三章/章内4段 500:800:300:400）
CREATE TABLE IF NOT EXISTS structure_beats (
    code          TEXT PRIMARY KEY,      -- S1~S10
    name          TEXT NOT NULL,
    value         TEXT,                  -- 节拍内容
    applies_to    TEXT,                  -- 卷/单元/章
    source        TEXT,
    chapter       INTEGER NOT NULL DEFAULT 0,
    basis         TEXT,
    source_card   TEXT,
    source_sha    TEXT,
    created_at    TEXT DEFAULT (datetime('now'))
);

-- ============ 57 维质检维度表 ============
-- R-EX-03：57 维 7 组 × 双档线 95/98 + floor 70/75/70；quality_gate 全维独立达标
CREATE TABLE IF NOT EXISTS scoring_dimensions (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    code          TEXT NOT NULL,          -- 维度编号
    dim_name      TEXT NOT NULL,          -- 维度名
    grp           TEXT,                  -- 7 组名
    excellent_line REAL,                 -- 优秀线（95 档）
    superior_line  REAL,                 -- 卓越线（98 档）
    floor         REAL,                  -- 地板线（70/75/70）
    check_method  TEXT,                  -- 检查方法
    ref_benchmark TEXT,                  -- R-EX-04：对比超越 6 维基准挂无量界宇宙基准
    chapter       INTEGER NOT NULL DEFAULT 0,
    basis         TEXT,
    source_card   TEXT,
    source_sha    TEXT,
    created_at    TEXT DEFAULT (datetime('now')),
    UNIQUE(code)
);

-- ============ 章节审计表（append-only） ============
-- L5 tick 唯一写端：每章 commit 后追写一行 + content-hash 版本指纹
CREATE TABLE IF NOT EXISTS chapter_log (
    seq           INTEGER PRIMARY KEY AUTOINCREMENT,
    volume        TEXT NOT NULL,
    chapter_no    INTEGER NOT NULL,
    tick_kind     TEXT,                  -- gate_pass/gen_done/quality/dynamics/state_writeback
    payload       TEXT,                  -- JSON
    content_hash  TEXT,                  -- 章全文 sha256[0:12] 版本指纹
    engine_md5    TEXT,                  -- 引擎版本指纹（白名单门）
    created_at    TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(volume, chapter_no, seq)
);
CREATE INDEX IF NOT EXISTS idx_chapter_log_vol ON chapter_log(volume, chapter_no);
CREATE INDEX IF NOT EXISTS idx_chapter_log_tick ON chapter_log(tick_kind);

-- ============ 叙事素材层（外部典库·引经据典/小故事/意象比喻，W2 扩容）============
-- 数据源 = data/kb_quotes.json / kb_anecdotes.json / kb_imagery.json（外部搜真实典库，非 LLM 生成）
-- 检索键 = scene_tag（场景）+ emotion_tag（情绪）→ build_prompt 第 6 类注入按本章场景/情绪抽 2-3 条
-- 公共列沿用：chapter/basis/source_card/source_sha（静态素材 chapter=0，basis=外部典库）

-- 引经据典（经典原文/成语典故/名人名言/科学定律，带场景+情绪标签）
CREATE TABLE IF NOT EXISTS kb_quotes (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    quote         TEXT NOT NULL UNIQUE,   -- 原文/名句（UNIQUE：增量灌库幂等键，OR IGNORE 去重防重复）
    source        TEXT,                  -- 出处（诗经/论语/史记/科学定律/…）
    meaning       TEXT,                  -- 可化用义（残响双系贴性解读）
    scene_tag     TEXT,                  -- 离别/忠义/执迷/代价/权谋/战斗/文明兴衰/哲理/希望/孤独/信念/政治
    emotion_tag   TEXT,                  -- 怅惘/敬/悲/怒/希望/讽/…
    insert_pos    TEXT,                  -- 章首/人物插叙/战斗前/文明背景
    themes        TEXT,                  -- JSON 数组（可省）
    civs          TEXT,                  -- JSON 数组（相关文明，可省）
    chapter       INTEGER NOT NULL DEFAULT 0,
    basis         TEXT,                  -- 外部典库
    source_card   TEXT,
    source_sha    TEXT,
    created_at    TEXT DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_kquotes_scene ON kb_quotes(scene_tag);
CREATE INDEX IF NOT EXISTS idx_kquotes_emo   ON kb_quotes(emotion_tag);

-- 小故事（寓言/轶事/传说，可作章节引子/人物插叙）
CREATE TABLE IF NOT EXISTS kb_anecdotes (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    anecdote      TEXT NOT NULL UNIQUE,   -- 故事摘要（UNIQUE：幂等键，防重复灌）
    source        TEXT,                  -- 出处（伊索/希腊神话/柏拉图/物理史…）
    moral         TEXT,                  -- 寓意（残响双系贴性解读）
    scene_tag     TEXT,
    emotion_tag   TEXT,
    insert_pos    TEXT,
    domain        TEXT,                  -- 西方/东方/科学
    chapter       INTEGER NOT NULL DEFAULT 0,
    basis         TEXT,
    source_card   TEXT,
    source_sha    TEXT,
    created_at    TEXT DEFAULT (datetime('now'))
);

-- 意象比喻（残响系专属比喻锚：潮汐/回声/熵增/共振/星图…）
CREATE TABLE IF NOT EXISTS kb_imagery (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    imagery       TEXT NOT NULL UNIQUE,   -- 意象（潮汐/回声/残响…）（UNIQUE：幂等键）
    literal       TEXT,                  -- 本义
    metaphor      TEXT,                  -- 可比喻义（文明兴衰/记忆余波/同源殊途…）
    emotion_tag   TEXT,
    use_scene     TEXT,                  -- 触发场景
    chapter       INTEGER NOT NULL DEFAULT 0,
    basis         TEXT,
    source_card   TEXT,
    source_sha    TEXT,
    created_at    TEXT DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_kimagery_scene ON kb_imagery(use_scene);

-- kb_rules（2026-10-01 庆余年拆解写法规则库·引擎 chapter_engine.py 实读）
CREATE TABLE kb_rules (
    rule_id      INTEGER PRIMARY KEY AUTOINCREMENT,
    rule_type    TEXT NOT NULL,     -- technique / dimension / opening
    code         TEXT NOT NULL,     -- 技法/维度 code
    name         TEXT,
    rule_text    TEXT,             -- 写法规则（人读摘要）
    example      TEXT,             -- 庆余年原文例证句（参照，不抄）
    shards       INTEGER,          -- 几片用到（命中广度）
    scene_tag    TEXT,            -- 适用场景
    source       TEXT DEFAULT '庆余年全量拆解',
    basis        TEXT DEFAULT '规则层（学骨）',
    UNIQUE(rule_type, code)
);
