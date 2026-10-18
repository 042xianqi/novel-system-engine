-- ============================================================
-- B 批（次优 8 张）：写到 300 万字后才用得上
-- 统一带 code / source_card / source_sha / basis 四列
-- ============================================================

-- 1. 人物微关系链 —— char_state 只存个人状态，无"关系"实体
CREATE TABLE IF NOT EXISTS char_relation (
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
CREATE INDEX IF NOT EXISTS idx_rela_ab ON char_relation(a_id, b_id);

-- 2. 信念/思潮/教义库
CREATE TABLE IF NOT EXISTS ideology (
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

-- 3. 「选择-代价」案例库 —— 校准全书价值观，避免双标
CREATE TABLE IF NOT EXISTS choice_case (
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

-- 4. 谎言&伪装链（补丁表 24）
CREATE TABLE IF NOT EXISTS lie_chain (
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

-- 5. 信息流/情报节点（补丁表 25）—— 消息不是自动全宇宙皆知
CREATE TABLE IF NOT EXISTS info_flow (
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

-- 6. 幻境/意识域空间（补丁表 26）
CREATE TABLE IF NOT EXISTS mental_domain (
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

-- 7. 跨距传讯&远程感知（补丁表 27）—— 防"隔半个大陆瞬间互通"
CREATE TABLE IF NOT EXISTS comm_link (
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

-- 8. 印记/烙印/纠缠标记（补丁表 28）
CREATE TABLE IF NOT EXISTS entity_mark (
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
