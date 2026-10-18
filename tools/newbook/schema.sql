-- 通用建库 schema（从既有真库导出；FTS shadow 表已剔除，由虚表自动重建）
-- 用法: python3 tools/newbook/init_newbook.py <新库根>

CREATE TABLE anomaly_state (
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
CREATE TABLE author_log (
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
CREATE TABLE card_usage (
  code TEXT PRIMARY KEY, category TEXT, name TEXT,
  ref_count INTEGER, last_chapter INTEGER
);
CREATE TABLE catalog_index (id INTEGER PRIMARY KEY AUTOINCREMENT, node_path TEXT,
  node_type TEXT, category TEXT, code TEXT, title TEXT, summary TEXT, char_count INTEGER DEFAULT 0,
  sha TEXT, src_kind TEXT, imported_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP, UNIQUE(node_path));
CREATE TABLE chapter_beat (
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
CREATE TABLE chapter_log (
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
CREATE TABLE chapters (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    volume           TEXT NOT NULL,        -- 卷01
    chapter_no       INTEGER NOT NULL,     -- 3
    title            TEXT,                 -- 第一枚残响
    outline          TEXT,                 -- 细纲要点全文
    scene_type       TEXT,                 -- 日常/战斗/修炼/大场面/心理/社交/过渡
    status           TEXT DEFAULT 'pending', -- pending/generated/reviewed/done
    words            INTEGER DEFAULT 0,
    content_path     TEXT,                 -- 04_正文/卷01/第3章.md
    quality_result   TEXT,                 -- 质检结果 JSON
    techniques_used  TEXT,                 -- 本章实际用了哪几条技法（JSON 数组）
    created_at       TEXT DEFAULT (datetime('now')),
    updated_at       TEXT DEFAULT (datetime('now')),
    UNIQUE(volume, chapter_no)
);
CREATE TABLE char_baseline_snap (
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
CREATE TABLE char_emotion (
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
CREATE TABLE char_memory (
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
CREATE TABLE char_relation (
    rela_id       TEXT PRIMARY KEY,           -- Rela-XXX
    code          TEXT,
    a_id          TEXT,                       -- 主体（主动方）
    b_id          TEXT,                       -- 客体（被动方）
    rela_tag      TEXT,                       -- 血亲|师徒|知交|亏欠|利用|宿怨|亦敌亦友|未知联结|精神烙印|演算绑定
    baseline_attitude TEXT,                   -- -100~100 或 文字
    origin_event  TEXT,                       -- 催生关系的事件/块 ID
    a_view_b      TEXT,                       -- A 以为 B 是？知道 B 哪些秘密？有何错觉？
    b_view_a      TEXT,
    sensitive_trigger TEXT,                   -- 什么会剧烈改变关系（含数值）
    status        TEXT DEFAULT '稳定',         -- 稳定|正在漂移|濒临破裂|被刻意伪装
    history_log   TEXT,                       -- JSON 数组：Rm-01 触发事件|变化|新基线
    hidden_layer  TEXT,                       -- 第三方知道但 A/B 不知的联结
    source_card   TEXT,
    source_sha    TEXT,
    basis         TEXT DEFAULT '待核',
    note          TEXT
);
CREATE TABLE char_state (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    char_code   TEXT NOT NULL,    -- CHAR-99-001
    char_name   TEXT NOT NULL,
    volume      TEXT,
    chapter     INTEGER,
    realm       TEXT,             -- 境界
    location    TEXT,             -- 位置
    status      TEXT,            -- 存活/死亡/失踪/透明
    notes       TEXT,
    UNIQUE(char_code, volume, chapter)
);
CREATE TABLE character_core (
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
CREATE TABLE choice_case (
    cho_id        TEXT PRIMARY KEY,           -- Cho-XXX
    code          TEXT,
    scene_ref     TEXT,                       -- B-XXX / Ch-XXX
    actor_id      TEXT,                       -- P-XXX
    option_a      TEXT,                       -- JSON：可得/必失/长期隐患
    option_b      TEXT,
    option_c      TEXT,                       -- 可选：妥协/绕路（也有代价）
    actual_choice TEXT,                       -- 选了？悬置？跳出？
    after_feedback TEXT,                      -- 世界有没有按"选择有重量"反馈
    theme_mapping TEXT,                       -- 回应全书哪个核心追问
    verdict       TEXT,                       -- 合格(得失对称) | 警示(曾写飘了)
    source_card   TEXT,
    source_sha    TEXT,
    basis         TEXT DEFAULT '待核',
    note          TEXT
);
CREATE TABLE civ_relation_state (
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
CREATE TABLE civ_state (
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
CREATE TABLE civ_sub_state (
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
CREATE TABLE comm_link (
    com_id        TEXT PRIMARY KEY,           -- Com-XXX
    code          TEXT,
    comm_type     TEXT,                       -- 血脉共鸣|阵讯|演算信道|飞符|心灵丝
    origin_rule   TEXT,                       -- 依托 L0 哪条规则
    effective_range TEXT,
    energy_cost   TEXT,
    blind_zone    TEXT,                       -- 哪些 R-XXX 会屏蔽/扭曲
    interception_possible TEXT,               -- 可否截听；条件
    traceable     TEXT,                       -- 可否反向定位；代价
    weak_point    TEXT,
    common_restriction TEXT,
    exclusive_user TEXT,
    source_card   TEXT,
    source_sha    TEXT,
    basis         TEXT DEFAULT '待核',
    note          TEXT
);
CREATE TABLE conflict_plan (
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
CREATE TABLE culture_state (
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
CREATE TABLE dynamics_events (
    seq          INTEGER NOT NULL,
    novel_id     TEXT NOT NULL,
    domain       TEXT NOT NULL,       -- 01_力量/02_文明/...
    kind         TEXT NOT NULL,       -- finding.P0/bayes.mood/...
    subject      TEXT,
    at           INTEGER,             -- 世界时间坐标
    payload      TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    PRIMARY KEY (novel_id, seq)
);
CREATE TABLE elixir_state (
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
CREATE TABLE entity_mark (
    mk_id         TEXT PRIMARY KEY,           -- Mk-XXX
    code          TEXT,
    mark_name     TEXT,
    mark_type     TEXT,                       -- 契约|诅咒|烙印|因果牵|观测锚点
    caster_id     TEXT,
    target_list   TEXT,
    birth_block   TEXT,
    visible_sign  TEXT,                       -- 外在可察痕迹
    passive_effect TEXT,
    active_trigger TEXT,
    side_effect   TEXT,
    dissolve_cond TEXT,                       -- 解除条件
    forced_break_cost TEXT,
    who_can_detect TEXT,
    hidden_side   TEXT,                       -- 连施印者都未必知道的隐性效果
    status        TEXT DEFAULT '生效',         -- 生效|衰弱|已断|伪印
    link_ids      TEXT,
    source_card   TEXT,
    source_sha    TEXT,
    basis         TEXT DEFAULT '待核',
    note          TEXT
);
CREATE TABLE event_template (
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
CREATE TABLE events (
    id               TEXT PRIMARY KEY,    -- EVT-003
    name             TEXT NOT NULL,
    status           TEXT DEFAULT 'pending', -- pending/in_progress/resolved
    trigger_chapter  INTEGER,
    resolve_chapter  INTEGER,
    dependencies     TEXT,              -- JSON 数组
    notes            TEXT
);
CREATE TABLE foreshadow (
    id               TEXT PRIMARY KEY,     -- F-001
    content          TEXT NOT NULL,
    planted_volume   TEXT,
    planted_chapter  INTEGER,
    deadline_volume  TEXT,
    deadline_chapter INTEGER,
    status           TEXT DEFAULT 'pending', -- pending/resolved/hollow
    resolved_chapter INTEGER,
    evidence         TEXT,                 -- 支持证据（角色行为/对话暗示）
    posterior        REAL DEFAULT 0.5,     -- 贝叶斯后验概率
    lik_ratio        REAL DEFAULT 2.0
);
CREATE TABLE foreshadow_hint (
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
CREATE TABLE free_var (
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
CREATE TABLE ideology (
    ideo_id       TEXT PRIMARY KEY,           -- Ideo-XXX
    code          TEXT,
    name          TEXT,
    alias         TEXT,                       -- 蔑称/美称
    origin        TEXT,                       -- 谁发起？源自哪段历史 E-XXX
    core_tenets   TEXT,                       -- JSON 数组 3-5 条主张
    logic_closure TEXT,                       -- 逻辑闭环（怎么自圆其说）
    internal_flaw TEXT,                       -- 内在漏洞
    extremized    TEXT,                       -- 极端化后会变成什么样
    spread_method TEXT,                       -- 传播方式
    distribution  TEXT,                       -- 分布人群 F-XXX/P-XXX
    role_in_main  TEXT,                       -- 对主线的作用
    exploit_point TEXT,                       -- 可利用点 & 可破局点
    link_ids      TEXT,
    source_card   TEXT,
    source_sha    TEXT,
    basis         TEXT DEFAULT '推测',
    note          TEXT
);
CREATE TABLE import_ledger (id INTEGER PRIMARY KEY AUTOINCREMENT, src_root TEXT,
  src_path TEXT NOT NULL, src_chars INTEGER, src_sha TEXT, dst_table TEXT, dst_key TEXT,
  dst_chars INTEGER, verified INTEGER DEFAULT 0, imported_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
  UNIQUE(src_path, dst_table));
CREATE TABLE info_flow (
    inf_id        TEXT PRIMARY KEY,           -- Inf-XXX
    code          TEXT,
    origin_evt    TEXT,                       -- E-XXX / B-XXX
    core_message  TEXT,
    distortion_rule TEXT,                     -- 每传一层怎么变形
    channels      TEXT,                       -- 口传|传讯术|密谍|典籍|异象公现
    spread_delay  TEXT,                       -- T+? 开始扩散；速度
    lock_area     TEXT,                       -- 封锁在哪些 R-XXX
    suppressor    TEXT,                       -- 谁在刻意压消息
    version_1     TEXT,                       -- 民间版本
    version_2     TEXT,                       -- 精英版本
    version_3     TEXT,                       -- 秘传版本（接近真相）
    known_map     TEXT,                       -- 谁拿到哪个版本
    can_leak_condition TEXT,
    status        TEXT DEFAULT '未扩散',       -- 未扩散|扩散中|已固化|彻底封锁
    source_card   TEXT,
    source_sha    TEXT,
    basis         TEXT DEFAULT '待核',
    note          TEXT
);
CREATE TABLE item_entry (
    code TEXT PRIMARY KEY, parent_code TEXT, name TEXT, maker TEXT, material TEXT, spec TEXT,
    func TEXT, "user" TEXT, price TEXT, skill TEXT, appear_ch TEXT, maintain TEXT, damage_thr TEXT,
    basis TEXT, source_card TEXT, source_sha TEXT, created_at TEXT DEFAULT (datetime('now')));
CREATE TABLE item_state (
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
CREATE TABLE items (
    id             TEXT PRIMARY KEY,
    name           TEXT NOT NULL,
    owner          TEXT,
    status         TEXT DEFAULT 'active', -- active/lost/destroyed/owned
    last_chapter   INTEGER,
    notes          TEXT
);
CREATE TABLE kb_knowledge (
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
CREATE TABLE lie_chain (
    lie_id        TEXT PRIMARY KEY,           -- L-XXX
    code          TEXT,
    speaker_id    TEXT,                       -- P-XXX 撒谎者
    target_ids    TEXT,                       -- 讲给谁（逗号分隔）
    birth_block   TEXT,                       -- B-XXX
    lie_content   TEXT,
    real_fact     TEXT,
    motive        TEXT,                       -- 自保|利用|善意|被胁迫|自欺
    known_colluders TEXT,                     -- 知情并帮着圆谎
    believers     TEXT,                       -- 真信了
    doubters      TEXT,                       -- 已起疑
    expose_condition TEXT,                    -- 什么条件下戳破
    expose_consequence TEXT,                  -- 戳破后预期连锁
    current_state TEXT DEFAULT '完好',         -- 完好|被质疑|已戳穿|自行瓦解
    linked_fb     TEXT,
    linked_k      TEXT,
    source_card   TEXT,
    source_sha    TEXT,
    basis         TEXT DEFAULT '待核',
    note          TEXT
);
CREATE TABLE lore_state (
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
CREATE TABLE map_state (
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
CREATE TABLE materials (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    code         TEXT NOT NULL,          -- 编号：CHAR-99-001
    name         TEXT NOT NULL,          -- 名称：陈寂
    category     TEXT NOT NULL,          -- 大类：角色/事件/文明/...
    sub_category TEXT,                   -- 小类：主角/全库列传/...
    file_path    TEXT NOT NULL,          -- 相对路径：02_资料库/角色/CHAR-99-001.md
    summary      TEXT,                   -- 摘要（从 _目录/*.md 读）
    refs         TEXT,                   -- 引用→（JSON 数组）
    ref_by       TEXT,                   -- ←被引（JSON 数组）
    personality  TEXT,                   -- JSON：角色性格卡（仅角色卡有值）
    h1           TEXT,                   -- H1 标题
    tags         TEXT,                   -- 标记（H1无码/无胶囊/双轨/...）
    created_at   TEXT DEFAULT (datetime('now')),
    updated_at   TEXT DEFAULT (datetime('now')), content TEXT, content_chars INTEGER, content_sha TEXT, old_code TEXT, new_code TEXT, frontmatter TEXT, renumbered_at TEXT, stale INTEGER DEFAULT 0, stale_reason TEXT,
    UNIQUE(code, category)
);
CREATE VIRTUAL TABLE materials_fts USING fts5(
    code, name, summary, personality,
    content='materials', content_rowid='id'
);
CREATE TABLE mental_domain (
    m_id          TEXT PRIMARY KEY,           -- M-XXX
    code          TEXT,
    name          TEXT,
    creator_id    TEXT,                       -- P-XXX / An-XXX
    spawn_block   TEXT,
    outer_trigger TEXT,                       -- 外人怎么进入
    inner_rule    TEXT,                       -- 域内 L0 局部补丁
    perceived_world TEXT,                     -- 域内"假象世界"
    real_structure TEXT,                      -- 幻境真正骨架
    anchor_points TEXT,                       -- 破局锚点
    exit_ways     TEXT,                       -- 合法脱出路径
    forced_exit_cost TEXT,                    -- 强行破域代价
    residue_effect TEXT,                      -- 记忆/精神后遗症
    captive_list  TEXT,
    max_load      TEXT,
    linked_reg    TEXT,
    linked_an     TEXT,
    status        TEXT DEFAULT '休眠',         -- 休眠|激活|崩坏|被加固
    source_card   TEXT,
    source_sha    TEXT,
    basis         TEXT DEFAULT '待核',
    note          TEXT
);
CREATE TABLE narrative_l0 (id TEXT PRIMARY KEY, code TEXT, world_name TEXT, tone TEXT,
  geo_frame TEXT, energy_rule TEXT, life_death_rule TEXT, time_causal_rule TEXT, taboos TEXT,
  theme TEXT, core_question TEXT, world_undercurrent TEXT, content TEXT, source_file TEXT,
  source_sha TEXT, basis TEXT DEFAULT '实值', created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE narrative_line (id TEXT PRIMARY KEY, code TEXT, line_type TEXT, name TEXT,
  protagonist TEXT, initial_situation TEXT, inner_desire TEXT, outer_goal TEXT, phase_path TEXT,
  ending_direction TEXT, forbidden_directions TEXT, intersect_with TEXT, content TEXT,
  source_file TEXT, source_sha TEXT, basis TEXT DEFAULT '实值', note TEXT);
CREATE TABLE narrative_volume (id TEXT PRIMARY KEY, code TEXT, vol_name TEXT,
  position_tag TEXT, state_before TEXT, goal_external TEXT, goal_internal TEXT, key_knots TEXT,
  ending_gain TEXT, ending_loss TEXT, next_gap TEXT, forbidden_in_vol TEXT,
  ch_start INTEGER, ch_end INTEGER, ch_count INTEGER, word_est INTEGER DEFAULT 0,
  content TEXT, source_file TEXT, source_sha TEXT, basis TEXT DEFAULT '实值', note TEXT);
CREATE TABLE pending_setting (
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
CREATE TABLE plot_block (
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
CREATE TABLE plot_theme (
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
CREATE TABLE power_state (
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
CREATE TABLE realm_state (
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
CREATE TABLE renumber_map (
  old_path TEXT PRIMARY KEY, old_code TEXT, new_code TEXT, category TEXT,
  new_path TEXT, rule TEXT, refs_rewritten INTEGER DEFAULT 0,
  status TEXT DEFAULT 'pending', note TEXT);
CREATE TABLE rule_state (
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
CREATE TABLE scoring_dimensions (
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
CREATE TABLE structure_beats (
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
CREATE TABLE tech_state (
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
CREATE TABLE technique_items (
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
CREATE TABLE timeline (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    volume         TEXT,
    chapter        INTEGER,
    world_time     TEXT,               -- 汐潮历 3 年·秋·初
    event_summary  TEXT,
    UNIQUE(volume, chapter)
);
CREATE TABLE timeline_state (
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
CREATE TABLE treasure_state (
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
CREATE TABLE world_snapshot (
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
CREATE TABLE world_state (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    civilization TEXT NOT NULL,             -- 文明名（玄脉宗/铸星联邦/...）
    volume      TEXT,                       -- 卷01
    chapter     INTEGER,                    -- 章号
    share       REAL,                       -- 势力份额 0-100（NULL = 待标定）
    notes       TEXT,                       -- 备注
    UNIQUE(civilization, volume, chapter)
);
CREATE INDEX idx_anomaly_chap ON anomaly_state(chapter);
CREATE INDEX idx_anomaly_name ON anomaly_state(name);
CREATE INDEX idx_cbs_char ON char_baseline_snap(char_code, volume);
CREATE INDEX idx_chap_vol ON chapters(volume, chapter_no);
CREATE INDEX idx_chapter_log_tick ON chapter_log(tick_kind);
CREATE INDEX idx_chapter_log_vol ON chapter_log(volume, chapter_no);
CREATE INDEX idx_char_emotion_chap ON char_emotion(chapter);
CREATE INDEX idx_char_emotion_char ON char_emotion(char_code);
CREATE INDEX idx_char_memory_char ON char_memory(char_code);
CREATE INDEX idx_char_memory_evt  ON char_memory(event_id);
CREATE INDEX idx_char_vol ON char_state(char_code, volume);
CREATE INDEX idx_ci_cat ON catalog_index(category);
CREATE INDEX idx_civ_state_chap ON civ_state(chapter);
CREATE INDEX idx_civ_state_name ON civ_state(name);
CREATE INDEX idx_civ_sub_civ ON civ_sub_state(civ_code);
CREATE INDEX idx_civ_sub_kind ON civ_sub_state(sub_kind);
CREATE INDEX idx_civrel_a ON civ_relation_state(a_civ);
CREATE INDEX idx_civrel_chap ON civ_relation_state(chapter);
CREATE INDEX idx_culture_name ON culture_state(name);
CREATE INDEX idx_dyn_events_at  ON dynamics_events(novel_id, at);
CREATE INDEX idx_dyn_events_seq ON dynamics_events(novel_id, seq);
CREATE INDEX idx_elixir_name ON elixir_state(name);
CREATE INDEX idx_event_tpl_name ON event_template(name);
CREATE INDEX idx_fh_fs     ON foreshadow_hint(fs_id);
CREATE INDEX idx_fh_status ON foreshadow_hint(status);
CREATE INDEX idx_fh_strength ON foreshadow_hint(strength);
CREATE INDEX idx_fresh_due ON foreshadow(deadline_chapter, status);
CREATE INDEX idx_item_entry_parent ON item_entry(parent_code);
CREATE INDEX idx_item_name ON item_state(name);
CREATE INDEX idx_kb_domain ON kb_knowledge(domain);
CREATE INDEX idx_kb_tag ON kb_knowledge(tag);
CREATE INDEX idx_kb_verdict ON kb_knowledge(verdict);
CREATE INDEX idx_lore_name ON lore_state(name);
CREATE INDEX idx_map_civ ON map_state(civ);
CREATE INDEX idx_map_name ON map_state(name);
CREATE INDEX idx_mat_cat  ON materials(category);
CREATE INDEX idx_mat_code ON materials(code);
CREATE INDEX idx_power_name ON power_state(name);
CREATE INDEX idx_rela_ab ON char_relation(a_id, b_id);
CREATE INDEX idx_rm_new ON renumber_map(new_code);
CREATE INDEX idx_rm_old ON renumber_map(old_code);
CREATE INDEX idx_rule_type ON rule_state(type);
CREATE INDEX idx_tech_civ ON tech_state(civ);
CREATE INDEX idx_tech_name ON tech_state(name);
CREATE INDEX idx_technique_series ON technique_items(series);
CREATE INDEX idx_timeline_phase ON timeline_state(phase);
CREATE INDEX idx_treasure_name ON treasure_state(name);
CREATE INDEX idx_world_state_civ ON world_state(civilization);
CREATE INDEX idx_world_state_vol ON world_state(volume, chapter);
CREATE TRIGGER mat_ad AFTER DELETE ON materials BEGIN
    INSERT INTO materials_fts(materials_fts, rowid, code, name, summary, personality)
    VALUES ('delete', old.id, old.code, old.name, old.summary, old.personality);
END;
CREATE TRIGGER mat_ai AFTER INSERT ON materials BEGIN
    INSERT INTO materials_fts(rowid, code, name, summary, personality)
    VALUES (new.id, new.code, new.name, new.summary, new.personality);
END;
CREATE TRIGGER mat_au AFTER UPDATE ON materials BEGIN
    INSERT INTO materials_fts(materials_fts, rowid, code, name, summary, personality)
    VALUES ('delete', old.id, old.code, old.name, old.summary, old.personality);
    INSERT INTO materials_fts(rowid, code, name, summary, personality)
    VALUES (new.id, new.code, new.name, new.summary, new.personality);
END;
