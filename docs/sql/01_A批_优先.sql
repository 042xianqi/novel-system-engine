-- ============================================================
-- A 批（优先 8 张）：补最痛空白，服务剧情演算引擎
-- 约定：统一带 code / source_card / source_sha / basis 四列
-- 可重复执行（IF NOT EXISTS）
-- ============================================================

-- 1. 剧情逻辑块（L3 子层，1 块 ≈ 1–3 章）—— 现有体系无中间层
CREATE TABLE IF NOT EXISTS plot_block (
    block_id      TEXT PRIMARY KEY,           -- B-Vol01-005
    code          TEXT,                       -- 同 block_id（对齐 materials.code 约定）
    belong_vol    TEXT,                       -- Vol-01
    seq_in_vol    INTEGER,                    -- 卷内序号
    location_id   TEXT,                       -- R-XXX / LOC-XXX
    chars_involved TEXT,                      -- P-XXX,CHAR-XXX 逗号分隔
    pre_condition TEXT,                       -- 因果前驱（前置输入）
    logic_chain   TEXT,                       -- A(契机)→B(行动反应)→C(结果)
    state_output  TEXT,                       -- JSON：状态输出（引擎跟踪）
    branch_main   TEXT,                       -- 主路径
    branch_alt    TEXT,                       -- 旁路
    branch_bad    TEXT,                       -- 极端坏路
    constraints   TEXT,                       -- 块约束（不能违 L0 哪条 / 不能 OOC 谁）
    link_prev     TEXT,                       -- 上一块
    link_next     TEXT,                       -- 下一块
    word_actual   INTEGER DEFAULT 0,          -- 实际落笔字数（0=未写）
    source_card   TEXT,
    source_sha    TEXT,
    basis         TEXT DEFAULT '待核',
    note          TEXT,
    created_at    TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at    TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- 2. 单章要点（L3.5）—— 现有 chapters 表只有 outline 单字段，无结构
CREATE TABLE IF NOT EXISTS chapter_beat (
    ch_id         TEXT PRIMARY KEY,           -- Ch-Vol01-012
    code          TEXT,
    title         TEXT,
    anchor_prev   TEXT,                       -- 承接上一章哪个块/状态
    task_info     TEXT,                       -- 信息任务（放线索/埋暗线）
    task_action   TEXT,                       -- 行动任务
    task_mental   TEXT,                       -- 心理任务
    scene_env     TEXT,                       -- 场景
    atmosphere    TEXT,                       -- 氛围
    flow_desc     TEXT,                       -- 过程简述（写流向）
    ending_hook   TEXT,                       -- 收尾落点
    forbidden_here TEXT,                      -- 本章红线
    word_actual   INTEGER DEFAULT 0,
    draft_path    TEXT,                       -- 正文文件路径（04_正文/...）
    link_block    TEXT,                       -- 归属块 B-XXX
    source_card   TEXT,
    source_sha    TEXT,
    basis         TEXT DEFAULT '待核',
    note          TEXT,
    created_at    TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at    TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- 3. 人物内核档案 —— 现有 materials.personality 全库 0 行有值，G06 形同虚设
CREATE TABLE IF NOT EXISTS character_core (
    code          TEXT PRIMARY KEY,           -- CHAR-99-001（与 materials.code 对齐）
    name          TEXT,
    alias         TEXT,
    first_block   TEXT,                       -- B-XXX 初次登场
    deep_desire   TEXT,                       -- 深层欲望（≠表面目标）
    deep_fear     TEXT,                       -- 深层恐惧
    blind_spot    TEXT,                       -- 认知盲点（自己看不见）
    value_priority TEXT,                      -- JSON 数组，有序！决定选择
    baseline_personality TEXT,                -- 性格基线
    trigger_abnormal TEXT,                    -- 什么条件会反常
    care_list     TEXT,                       -- 羁绊-在意
    owe_list      TEXT,                       -- 羁绊-亏欠
    hate_list     TEXT,                       -- 羁绊-仇恨/博弈
    know_explicit TEXT,                       -- 明确知道
    know_suspect  TEXT,                       -- 有所怀疑
    know_none     TEXT,                       -- 完全不知道
    milestone     TEXT,                       -- 变化里程碑（JSON 数组）
    source_card   TEXT,
    source_sha    TEXT,
    basis         TEXT DEFAULT '待核',
    note          TEXT,
    created_at    TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at    TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- 4. 待审设定池 —— 防 AI 乱塞设定的闸门（现有完全缺失）
CREATE TABLE IF NOT EXISTS pending_setting (
    tmp_id        TEXT PRIMARY KEY,           -- S-TMP-XXX
    code          TEXT,
    name          TEXT,                       -- 设定名称/脑洞
    brief         TEXT,
    inspiration   TEXT,                       -- 灵感来源
    q1_l0_conflict TEXT,                      -- 三问自检 1：是否与 L0 冲突
    q2_why_absent TEXT,                       -- 三问自检 2：早就存在为何前文没出现
    q3_chain_effect TEXT,                     -- 三问自检 3：连锁改变哪些逻辑
    suggest_vol   TEXT,                       -- 建议安放区间 VolX-VolY
    anchor_plan   TEXT,                       -- 预埋锚点方案
    review_status TEXT DEFAULT '搁置',         -- 搁置 | 修改后可用 | 已入库
    final_id      TEXT,                       -- 入库后分配的正式 ID
    source_card   TEXT,
    source_sha    TEXT,
    basis         TEXT DEFAULT '推测',
    note          TEXT,
    created_at    TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- 5. 伏笔预埋单元（弱提示）—— 现有 foreshadow 只记"大悬念"，无细粒度
CREATE TABLE IF NOT EXISTS foreshadow_hint (
    hint_id       TEXT PRIMARY KEY,           -- Fb-XXX
    code          TEXT,
    fs_id         TEXT,                       -- 关联 foreshadow.id（可空＝独立弱提示）
    hint_type     TEXT,                       -- 环境细节|随口一语|反常行为|残缺记载|物品异象|人物下意识反应|传说碎片
    first_seen    TEXT,                       -- Ch-XXX / B-XXX
    surface_meaning TEXT,                     -- 表层意思（当时看到的）
    underlying_meaning TEXT,                  -- 底层含义（后期可兑现）
    strength      INTEGER,                    -- 1弱 2中 3强
    payoff_window TEXT,                       -- Vol-X ~ Vol-Y
    payoff_plan   TEXT,                       -- 兑现方式预案
    payoff_mode   TEXT,                       -- direct|corroborate|misdirect
    depends_on    TEXT,                       -- 依赖条件（须先回收/放出的线索）
    consistency_checked INTEGER DEFAULT 0,    -- 是否已校验"当时场景下合理"
    status        TEXT DEFAULT '未触发',       -- 未触发|已触发待兑现|已兑现|废弃
    source_card   TEXT,
    source_sha    TEXT,
    basis         TEXT DEFAULT '待核',
    note          TEXT
);
CREATE INDEX IF NOT EXISTS idx_fh_fs     ON foreshadow_hint(fs_id);
CREATE INDEX IF NOT EXISTS idx_fh_strength ON foreshadow_hint(strength);
CREATE INDEX IF NOT EXISTS idx_fh_status ON foreshadow_hint(status);

-- 6. 主题&母题跟踪 —— 防止几百万字跑题
CREATE TABLE IF NOT EXISTS plot_theme (
    theme_id      TEXT PRIMARY KEY,           -- T-01
    code          TEXT,
    core_question TEXT,                       -- 全书核心追问
    stage_layer   TEXT,                       -- 本阶段在回答哪一层
    raising       TEXT,                       -- 正在抛出的问题
    partial_answer TEXT,                      -- 已给出的部分答案（非最终）
    counter_examples TEXT,                    -- 反面案例（谁走了另一条路）
    avoid_path    TEXT,                       -- 需要避免的歪路
    motifs        TEXT,                       -- 可埋意象/符号
    valid_from_vol TEXT,
    status        TEXT DEFAULT '正常推进',      -- 正常推进|需要微调|已偏移
    source_card   TEXT,
    source_sha    TEXT,
    basis         TEXT DEFAULT '推测',
    note          TEXT
);

-- 7. 演算引擎快照库 —— 定期存世界状态，用于回溯/重放/对比分支
CREATE TABLE IF NOT EXISTS world_snapshot (
    snap_id       TEXT PRIMARY KEY,           -- Snap-0001
    code          TEXT,
    anchor_t      TEXT,                       -- 时序锚点 T+XX
    anchor_block  TEXT,                       -- 对应块 B-XXX
    brief_state   TEXT,                       -- 世界简况
    char_snap_ref TEXT,                       -- 人物状态偏移引用
    fac_snap_ref  TEXT,
    reg_snap_ref  TEXT,
    pit_snap_ref  TEXT,
    branch_path   TEXT,                       -- 当前推演分支
    is_canon      INTEGER DEFAULT 1,          -- 1=正史 0=备选分支
    source_card   TEXT,
    source_sha    TEXT,
    basis         TEXT DEFAULT '实值',
    create_time   TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    note          TEXT
);

-- 8. 作者操作&修改日志 —— 千万字级别必做（久了会失忆"当初为什么这么改"）
CREATE TABLE IF NOT EXISTS author_log (
    log_id        INTEGER PRIMARY KEY AUTOINCREMENT,
    code          TEXT,
    op_type       TEXT,                       -- 新建|修改|废弃|分支测试|回填设定
    target_type   TEXT,                       -- 人物/势力/块/坑/设定
    target_id     TEXT,
    before_content TEXT,
    after_content TEXT,
    reason        TEXT,                       -- 为什么改（非常重要）
    related_vol   TEXT,
    source_card   TEXT,
    source_sha    TEXT,
    basis         TEXT DEFAULT '实值',
    op_time       TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);
