-- ============================================================
-- C 批（长尾 3 张）
-- ============================================================

-- 1. 作者预留「自由变量池」—— 演算≠定死命运，人的出口
CREATE TABLE IF NOT EXISTS free_var (
    var_id        TEXT PRIMARY KEY,           -- Var-XXX
    code          TEXT,
    name          TEXT,
    what_is_it    TEXT,                       -- 是什么（合法未知量）
    constraint_range TEXT,                    -- 波动多大？绝不能突破哪几条 L0
    who_knows     TEXT,                       -- 谁知道它存在；谁试图捕捉/控制/抹除
    can_disturb   TEXT,                       -- 能扰动什么
    cannot_disturb TEXT,                      -- ⚠ 不能扰动什么（防万能解）
    release_window TEXT,                      -- Vol-X ~ Vol-Y
    caution       TEXT,                       -- 慎用铁律（禁机械降神）
    status        TEXT DEFAULT '封存',         -- 封存|可逐步释放|暂不启用
    source_card   TEXT,
    source_sha    TEXT,
    basis         TEXT DEFAULT '推测',
    note          TEXT
);

-- 2. 单场冲突推演预案 —— 博弈逻辑（不是打斗描写）
CREATE TABLE IF NOT EXISTS conflict_plan (
    conflict_id   TEXT PRIMARY KEY,           -- C-XXX
    code          TEXT,
    location_id   TEXT,                       -- R-XXX
    parties       TEXT,                       -- JSON：[{id,side,goal,chips,bottom_line,sacrifice_cap}]
    trigger_spot  TEXT,                       -- 直接契机
    deep_conflict TEXT,                       -- 深层矛盾（利益|理念|旧仇|信息差|被人挑动）
    strategies    TEXT,                       -- JSON：各方可选策略集+收益/代价/失败后果
    path_alpha    TEXT,                       -- 主路径
    path_beta     TEXT,                       -- 次可能
    path_omega    TEXT,                       -- 极端崩盘路（触发条件）
    must_change   TEXT,                       -- 冲突后必须改变的东西（不能打完归零）
    constraints   TEXT,                       -- 红线（L0 / OOC / 泄密）
    source_card   TEXT,
    source_sha    TEXT,
    basis         TEXT DEFAULT '待核',
    note          TEXT
);

-- 3. 欲望/恐惧/盲点 之外的「性格基线快照」（随卷变化）
CREATE TABLE IF NOT EXISTS char_baseline_snap (
    snap_id       TEXT PRIMARY KEY,
    code          TEXT,
    char_code     TEXT,                       -- CHAR-99-001
    volume        TEXT,
    chapter       INTEGER,
    baseline_personality TEXT,
    value_priority TEXT,                      -- 该时点的优先级排序（可能已偏移）
    trigger_abnormal TEXT,
    shift_reason  TEXT,                       -- 相比上一快照为何偏移
    source_card   TEXT,
    source_sha    TEXT,
    basis         TEXT DEFAULT '实值',
    note          TEXT
);
CREATE INDEX IF NOT EXISTS idx_cbs_char ON char_baseline_snap(char_code, volume);
