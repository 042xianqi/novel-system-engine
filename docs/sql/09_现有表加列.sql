-- ============================================================
-- 现有表加列（决策 3：概念重叠的表走 ALTER，不新建）
-- ⚠️ 执行前必须备份 WAL 三文件
-- ⚠️ SQLite 的 ADD COLUMN 不支持 IF NOT EXISTS
--    → 本脚本**不能重复执行**；重复跑会报 "duplicate column name"
--    → 建议先跑下面的「体检」查询，已存在的列跳过
-- ============================================================

-- 【执行前体检】查看现有列，自行删去已存在的 ADD
-- PRAGMA table_info(map_state);
-- PRAGMA table_info(treasure_state);
-- PRAGMA table_info(events);
-- PRAGMA table_info(power_state);
-- PRAGMA table_info(anomaly_state);
-- PRAGMA table_info(foreshadow);

-- ── map_state（现 38 行）：补 模板J 的地点要素 ────────────────
ALTER TABLE map_state ADD COLUMN surface_legend  TEXT;  -- 表层传说（可以是谎言）
ALTER TABLE map_state ADD COLUMN deep_secret     TEXT;  -- 深层秘密（谁知道？）
ALTER TABLE map_state ADD COLUMN enter_cost      TEXT;  -- 进入代价 & 离开限制
ALTER TABLE map_state ADD COLUMN plot_anchor     TEXT;  -- 伏笔锚点（可发生什么）
ALTER TABLE map_state ADD COLUMN local_rule      TEXT;  -- 本地固有规则（须服从 L0）

-- ── treasure_state（现 11 行）：补 模板K 的奇物要素 ──────────
ALTER TABLE treasure_state ADD COLUMN cost_use       TEXT;  -- 使用一次代价
ALTER TABLE treasure_state ADD COLUMN cost_hold      TEXT;  -- 长期持有侵蚀
ALTER TABLE treasure_state ADD COLUMN cost_overlimit TEXT;  -- 超限催动代价
ALTER TABLE treasure_state ADD COLUMN history_chain  TEXT;  -- 流转历史 H0→H1→…
ALTER TABLE treasure_state ADD COLUMN public_view    TEXT;  -- 大众认知
ALTER TABLE treasure_state ADD COLUMN insider_view   TEXT;  -- 少数知情人认知
ALTER TABLE treasure_state ADD COLUMN truth_view     TEXT;  -- 完全真相
ALTER TABLE treasure_state ADD COLUMN forbidden_use  TEXT;  -- 禁则（越界触发什么）

-- ── events（现 46 行）：补 模板L 的历史事件要素 ──────────────
ALTER TABLE events ADD COLUMN version_popular TEXT;  -- 民间流传版本
ALTER TABLE events ADD COLUMN version_alt     TEXT;  -- 另一版本（各带偏见）
ALTER TABLE events ADD COLUMN real_truth      TEXT;  -- 事实真相
ALTER TABLE events ADD COLUMN deep_cause      TEXT;  -- 深层根源（长期矛盾）
ALTER TABLE events ADD COLUMN knowers         TEXT;  -- 知道真相的人集合
ALTER TABLE events ADD COLUMN link_to_main    TEXT;  -- 和主线可联动点

-- ── power_state（现 24 行）：补 模板V 的体系要素 ─────────────
ALTER TABLE power_state ADD COLUMN alienation     TEXT;  -- 必然异化倾向
ALTER TABLE power_state ADD COLUMN ceiling        TEXT;  -- 天然天花板
ALTER TABLE power_state ADD COLUMN summit_records TEXT;  -- 已知登顶者历史
ALTER TABLE power_state ADD COLUMN system_blind   TEXT;  -- 体系盲区
ALTER TABLE power_state ADD COLUMN forbidden_use  TEXT;  -- 禁则

-- ── anomaly_state（现 14 行）：补 模板W 的异常要素 ───────────
ALTER TABLE anomaly_state ADD COLUMN hypothesis_folk TEXT;  -- 民间假说
ALTER TABLE anomaly_state ADD COLUMN hypothesis_acad TEXT;  -- 学界/秘派假说
ALTER TABLE anomaly_state ADD COLUMN safety_threshold TEXT; -- 安全阈值
ALTER TABLE anomaly_state ADD COLUMN utilization     TEXT;  -- 可用价值

-- ── foreshadow（现 12 行）：补 放出强度（模板G/U 需要）───────
ALTER TABLE foreshadow ADD COLUMN release_strength INTEGER;  -- 1弱 2中 3强
ALTER TABLE foreshadow ADD COLUMN hint_count       INTEGER DEFAULT 0;  -- 弱提示次数
ALTER TABLE foreshadow ADD COLUMN payoff_mode      TEXT;     -- direct|corroborate|misdirect
ALTER TABLE foreshadow ADD COLUMN abandon_reason   TEXT;     -- 废弃原因（若 status=废弃）
