//! 外部接入层 `api.rs` —— 三层系统提示词 + 记忆子系统串接（需求文档 §一/§二）。
//!
//! # 数据流（严格顺序，不可颠倒，§一）
//!
//! ```text
//! 用户输入(章号 + 原始L3细纲)
//!   ↓ memory_system::preprocess()   ← 本轮新增（状态冲突则 Err，不调模型）
//!   ↓ build_knowledge_injection     ← 原有，零改动，只多收一个文本片段
//!   ↓ build_prompt                  ← 原有，零改动
//!   ↓ 总输入 token 校验（超限直接报错）
//!   ↓ LLM
//!   ↓ 原有门禁链路                  ← 零改动
//!   ↓ memory_system::post_update()  ← 本轮新增
//! ```
//!
//! # 系统提示词分层（§二）
//!
//! `SYS_LAYER_0`（底层行为） → `SYS_LAYER_1`（世界观铁律） → 记忆注入片段（靠近尾部）
//! → `SYS_LAYER_2`（本章任务） → `writing_template`（原有模板）。
use anyhow::Result;
use rusqlite::Connection;

/// 第 0 层：底层模型行为系统提示（最高优先级，固定）。
pub const SYS_LAYER_0: &str = "\
你是专用长篇小说写作智能体，运行于 Rust 调度系统，后端模型 GLM-5.3-Flash。
硬性行为规则，必须无条件遵守：
1. 严格服从输入内的资料库、记忆注入片段、本章压缩细纲。禁止自行脑补新增设定、新增人物、新增跨卷长线伏笔。
2. 你收到的记忆注入片段来自程序结构化快照：包含人物当前伤势、修为、持有道具、跨卷锚点进度。
   - 出现人物状态矛盾：优先采信【记忆注入片段】 > 本章细纲；细纲冲突会被前置代码拦截，你几乎不会遇见；若遇见矛盾，不要自行调和，直接在正文末尾输出一行 WARNING 标记矛盾点，不要偷偷改写剧情掩盖。
3. 实体与设定：资料库没出现过的人物、功法、地点，不可以编造；未完成设定标记【待L3补】，原样保留，不要自己填充细节。
4. 幻觉抑制：不要输出系统日志、数据库 ID、溯源标签、内部锚点编号、伪代码。输出只生成纯粹小说正文。
5. 格式保护规则：[[CANON_TEXT_START]] 与 [[CANON_TEXT_END]] 包裹的定版文本，一字不可修改、不可删减。段落长度遵从模板约束，禁止超大块长段落。
6. 不要做前置分析、不要写本章梗概、不要结尾写总结复盘。输出直接进入小说正文。
7. 输出 token 不要逼近上限，如果剧情体量巨大，在正文末尾输出 SPLIT 标记，代表本章剧情体量过大需要拆分下一章继续写。";

/// 第 1 层：小说世界观 & 文风铁律（来自 `long_term_memory`，动态拼接，全局不变）。
///
/// 当 `long_term_memory` 表为空（未导入）时，回落到本内置常量。
pub const SYS_LAYER_1_FALLBACK: &str = "\
# 《残响之河》世界观与写作铁律
## 金手指时序铁则
卷01-03，推演面板/锚点只表现为颅内模糊异样体感；严禁输出面板UI、弹窗、“锚点已建立”文字、人声提示；U0功能随剧情逐步显现；正文永远禁止解释推演面板来源，来源设定仅资料库内部保存，叙事不引用。
## 代价铁则
只要动用推演面板，必须写出对应的代价体感：精神损耗、记忆消耗、存在熵耗，禁止能力白嫖。
## 卷序与红线规则
故事单向向前推进，禁止跳卷、回溯改写已发生剧情；卷04起进入红线区：只兑现旧伏笔，禁止新开跨卷长线剧情。
## 文风基调铁则
全书底色黑暗、压抑、生存苦难。
幽默自嘲、脑洞吐槽只允许存在于主角陈寂的内心想法；客观环境描写、受伤、死亡、悲剧场景禁止戏谑搞笑；吐槽念头出现之后，必须立刻拉回残酷现实，不可以消解苦难悲剧。
心理描写：多用碎片化跳跃念头，允许想法半途搁置，不要通篇大段完整复盘式心理独白；允许短暂镜头切到配角独立小动作视角；允许本章谜团不闭环，留到后续章节揭开。
## 核心角色静态特质
【陈寂】穿越农家少年，16岁。外表冷静务实寡言，说话克制，嘴上不会讲玩笑话；仅内心允许冒出不合时宜自嘲碎念，念头可以碎片化中断，不要写大段顺滑内心复盘；禁止嘴上插科打诨。
【陈老头】年迈贫苦老人，丧亲苦难，疼爱孙子，心思敏感多疑，有老人执拗，不能工具人化。
【林晚秋】外门师姐，战友恋人，灵识共鸣型，敏锐隐忍，拥有独立立场，不做无脑附庸。
【白鸦】影卫，立场复杂，存有私人善意；战死留下遗物、委托、赎罪支线。
【陈默】漂泊少年，亲情长线锚点。
## 身份揭露铁则
卷08才揭露陈寂=穿越者，推演面板是体系外未证变量；正文绝不描写面板来源、不写前世背景故事。
## 跨卷锚点元规则
1.白鸦委托线：查清存在之基真相，卷04暗影王朝卷集中收口；附带赎罪支线家书寄青溪村白芷。
2.林晚秋情感线：卷08身份揭露深化，卷09-10终局收束。
3.陈默亲情线：牵挂，卷09魂契桥重逢。
4.推演面板U阶时序：U0卷01，U1-U3卷02-03，U4-U5卷04-05，U6-U7卷07-08，U8本源推演仅限卷10终局一次性使用。
5.存在之基碎片：卷01埋下，各卷逐步挖掘。
6.跨文明观测异象：卷01炼气四重异象触发，后续各文明观测响应。
> 红线标记：卷04开始，不再新增跨卷长线，只能兑现已列出线索。";

/// 分层系统提示词（拼接顺序固定，§二 尾）。
#[derive(Debug, Clone)]
pub struct LayeredSystemPrompt {
    pub layer0: String,
    pub layer1: String,
    pub memory_inject: String,
    pub layer2: String,
}

impl LayeredSystemPrompt {
    /// 按固定顺序拼成一整段 system prompt。
    ///
    /// `SYS_LAYER_0 → SYS_LAYER_1 → 记忆注入片段 → SYS_LAYER_2`
    /// （`writing_template` 仍在 user 侧 prompt 内，由原有 `build_prompt` 负责，零改动。）
    pub fn assemble(&self) -> String {
        let mut s = String::new();
        s.push_str(&self.layer0);
        s.push_str("\n\n");
        s.push_str(&self.layer1);
        if !self.memory_inject.trim().is_empty() {
            s.push_str("\n\n");
            s.push_str(&self.memory_inject);
        }
        if !self.layer2.trim().is_empty() {
            s.push_str("\n\n");
            s.push_str(&self.layer2);
        }
        s
    }
}

/// 从 `long_term_memory` 读世界观铁则 + 角色档案 + 顶层设定(L0/L0.5) + 分卷大纲(L1/L1x) + 锚点元，拼成 LAYER_1。
/// 按 priority 升序注入。
/// 表空 → 回落内置常量。
pub fn build_layer1(conn: &Connection) -> String {
    let mut stmt = match conn.prepare(
        "SELECT category, key, content FROM long_term_memory
         WHERE category IN ('世界观铁则','角色静态档案','顶层设定','分卷大纲','锚点元')
         ORDER BY priority, category, key",
    ) {
        Ok(s) => s,
        Err(_) => return SYS_LAYER_1_FALLBACK.to_string(),
    };
    let rows: Vec<(String, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map(|it| it.filter_map(|x| x.ok()).collect())
        .unwrap_or_default();
    if rows.is_empty() {
        return SYS_LAYER_1_FALLBACK.to_string();
    }
    let mut s = String::from("# 《残响之河》世界观与写作铁律\n");
    let mut last_cat = String::new();
    for (cat, key, content) in rows {
        if cat != last_cat {
            s.push_str(&format!("## {cat}\n"));
            last_cat = cat;
        }
        s.push_str(&format!("### {key}\n{content}\n"));
    }
    s
}

/// 构建第 2 层：本章执行任务约束（来自压缩版 L3 细纲）。
pub fn build_layer2(volume: &str, chapter: u32, compressed_outline: &str) -> String {
    let body = if compressed_outline.trim().is_empty() {
        "（本章细纲缺失）".to_string()
    } else {
        compressed_outline.to_string()
    };
    format!(
        "# 本章专属写作约束\n本章卷号：{volume}，章号：第{chapter}章\n{body}\n\
         输出要求：严格遵从记忆注入片段的人物状态，不要新增剧情、不要新增人物；只输出纯粹小说正文。"
    )
}

/// 组装完整分层系统提示词。
pub fn build_layered_system(
    conn: &Connection,
    volume: &str,
    chapter: u32,
    memory_inject: &str,
    compressed_outline: &str,
) -> LayeredSystemPrompt {
    LayeredSystemPrompt {
        layer0: SYS_LAYER_0.to_string(),
        layer1: build_layer1(conn),
        memory_inject: memory_inject.to_string(),
        layer2: build_layer2(volume, chapter, compressed_outline),
    }
}

/// 状态冲突校验（§一 / §四.1）：细纲人物/道具 vs 快照状态，冲突则拒绝生成。
///
/// 口径保守：只校验**明确矛盾**（如细纲称「已死」而快照「存活」）。
/// 返回 `Err` 时**不调用模型**。
pub fn check_state_conflict(
    conn: &Connection,
    volume: &str,
    chapter: u32,
    compressed_outline: &str,
) -> Result<()> {
    // 从快照读已死角色
    let snapshot: Option<String> = conn
        .query_row(
            "SELECT payload FROM mid_term_snapshot WHERE upto_chapter < ?1
             ORDER BY upto_chapter DESC LIMIT 1",
            rusqlite::params![chapter],
            |r| r.get(0),
        )
        .ok();
    let Some(p) = snapshot else { return Ok(()) };
    let v: serde_json::Value = serde_json::from_str(&p).unwrap_or(serde_json::Value::Null);
    if let Some(arr) = v.get("char_states").and_then(|x| x.as_array()) {
        for e in arr {
            let name = e.get("name").and_then(|x| x.as_str()).unwrap_or("");
            let status = e.get("status").and_then(|x| x.as_str()).unwrap_or("");
            if name.is_empty() {
                continue;
            }
            // 快照已死，但细纲仍让其行动 → 冲突
            let dead = status.contains('死') || status.contains("殁");
            if dead && compressed_outline.contains(name) {
                anyhow::bail!(
                    "状态冲突：{name} 在快照中已「{status}」，但本章细纲仍安排其出场；\
                     请人工核对（不调用模型）。volume={volume} chapter={chapter}"
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_system;

    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        memory_system::init_schema(&c).unwrap();
        c.execute_batch(
            "CREATE TABLE IF NOT EXISTS char_state (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                char_code TEXT, char_name TEXT, volume TEXT, chapter INTEGER,
                realm TEXT, location TEXT, status TEXT, notes TEXT
            );",
        )
        .unwrap();
        c
    }

    #[test]
    fn layer_order_is_fixed() {
        let c = fresh();
        let sp = build_layered_system(&c, "卷01", 1, "【块C】状态", "### 第1章 涧底醒");
        let s = sp.assemble();
        let i0 = s.find("专用长篇小说写作智能体").unwrap();
        let i1 = s.find("金手指时序铁则").unwrap();
        let im = s.find("【块C】状态").unwrap();
        let i2 = s.find("本章专属写作约束").unwrap();
        assert!(i0 < i1 && i1 < im && im < i2, "分层顺序必须固定");
    }

    #[test]
    fn layer1_falls_back_when_table_empty() {
        let c = fresh();
        assert!(build_layer1(&c).contains("金手指时序铁则"));
    }

    #[test]
    fn layer1_uses_db_when_present() {
        let c = fresh();
        c.execute(
            "INSERT INTO long_term_memory(category,key,content,priority)
             VALUES('世界观铁则','自定义铁则','正文不得出现XYZ',1)",
            [],
        )
        .unwrap();
        let s = build_layer1(&c);
        assert!(s.contains("正文不得出现XYZ"));
        assert!(!s.contains("金手指时序铁则"), "有 DB 数据时不应用 fallback");
    }

    #[test]
    fn conflict_detected_blocks_generation() {
        let c = fresh();
        c.execute(
            "INSERT INTO char_state(char_code,char_name,volume,chapter,status)
             VALUES('CHAR-Y','某人','卷01',0,'已死')",
            [],
        )
        .unwrap();
        memory_system::post_update(&c, "卷01", 30, "正文").unwrap();
        let r = check_state_conflict(&c, "卷01", 31, "本章「某人」突然出现");
        assert!(r.is_err(), "已死角色出场应报冲突");
    }

    #[test]
    fn no_conflict_when_alive() {
        let c = fresh();
        c.execute(
            "INSERT INTO char_state(char_code,char_name,volume,chapter,status)
             VALUES('CHAR-Z','好人','卷01',0,'存活')",
            [],
        )
        .unwrap();
        memory_system::post_update(&c, "卷01", 30, "正文").unwrap();
        assert!(check_state_conflict(&c, "卷01", 31, "「好人」出现").is_ok());
    }
}