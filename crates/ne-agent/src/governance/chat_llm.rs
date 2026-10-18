//! 对话中枢的 **L2 意图层（LLM 兜底）** —— 2026-10-17
//!
//! # 为什么需要它
//! L1 规则层（`chat_intent.rs`）用正则识别意图，优点是 <1ms、零成本、可解释。
//! 但用户说「你能建立任务吗」时，正则匹配不到「建卡」二字（"建立任务" ≠ "建卡"），
//! 就掉进了 fallback → 复读一遍能力清单。用户体感就是"这玩意是个复读机"。
//!
//! L2 的职责：**只在 L1 全部落空时兜底**，用真 LLM 理解自然语言。
//!
//! # 三条硬约束（不可违反）
//!
//! 1. **绝不绕过授权锁**：调用前检查 `real_llm_lock::ALLOW_FILE`。
//!    锁不在 → 直接返回 `Skipped`，回落到 L1 的 fallback 文案。
//!    本模块**从不创建**该文件（铁则：只能由人点开关创建）。
//!
//! 2. **只读 + 显式确认**：L2 可以查询数据、可以回答闲聊，
//!    但**绝不直接执行写操作**。若 LLM 判断用户想建卡/写章节，
//!    只返回 `needs_confirmation: true` 让前端弹确认按钮，绝不自动派发。
//!    这是用户定的「一枪不开」纪律。
//!
//! 3. **失败必须优雅降级**：网关 502、超时、JSON 解析失败 —— 任何异常都
//!    不能让聊天框报错或卡住。降级为"我暂时连不上大脑"而不是崩。
//!
//! # 为什么不自带 HTTP 客户端
//! 仓库已有 `ne_engine::http::post_json_value`（零依赖手写 HTTP/1.1，
//! 支持 TLS/chunked），且 `Cargo.toml` 明确"不引入 reqwest/tokio"。
//! 复用它是唯一正确选择。

use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

use super::chat_intent::ChatReply;

/// 本地 ofm 网关默认端点（与 cardgen.py 保持同一口径）
const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:18902/v1/chat/completions";
/// 用户指定唯一模型：deepseek-v4.1-flash-free（不降级、不换模型）
const DEFAULT_MODEL: &str = "deepseek-v4.1-flash-free";
/// 本地 ofm 网关的约定 Key
const DEFAULT_KEY: &str = "sk-local-free";

/// L2 调用结局。
pub enum L2Outcome {
    /// 成功：拿到一条可用的聊天回复
    Replied(Value),
    /// 跳过（授权锁没开 / 网关不可达 / 解析失败）——调用方回落到 L1 fallback
    Skipped { why: String },
}

/// 工具清单（给 LLM 看的，让它知道自己能"点"哪些功能）
///
/// 【为什么把清单写死在这里而不从代码反射】
/// 反射能自动同步，但会把内部函数名（`intent_self_check`）暴露给模型，
/// 模型容易照着函数名瞎编调用。手写一份「人话版能力清单」更可控：
/// 模型知道有「系统自检」这个能力，但它只会用 `intent: "self_check"` 这种
/// 约定好的 token 来指代，不会编造不存在的调用。
fn tool_catalog() -> Value {
    json!([
        {"intent": "status",         "desc": "查询当前任务队列与执行进度（pending/running/done/failed 统计）", "needs_confirm": false},
        {"intent": "failures",       "desc": "列出失败/被拒/阻塞的任务清单，含卡名与失败原因",            "needs_confirm": false},
        {"intent": "self_check",     "desc": "跑一次系统自检（管线健康度、缺卡、门禁状态）",            "needs_confirm": false},
        {"intent": "library",        "desc": "查询资料库：有多少张卡、某文明的设定、某类目有哪些卡",       "needs_confirm": false},
        {"intent": "chapters",       "desc": "查询某卷有哪些章节、某章是否已生成",                       "needs_confirm": false},
        {"intent": "cardgen",        "desc": "为某文明批量建卡（写操作，系统会自动入队，用户随时可取消）", "needs_confirm": true},
        {"intent": "write_chapter",  "desc": "生成某一章的正文（写操作，系统会自动入队，用户随时可取消）",  "needs_confirm": true},
        {"intent": "chat",           "desc": "纯闲聊/答疑，不触发任何系统动作",                          "needs_confirm": false}
    ])
}

/// 系统提示词。
///
/// 设计要点：
/// - 强制 JSON 输出（不靠 response_format，因为网关不一定支持，改用提示词约束 + 容错解析）
/// - 明确「你是秘书不是作家」：不许在回复里编造小说设定
/// - 明确「不知为不知」：查不到就说查不到，不许编数字
fn system_prompt() -> String {
    r#"你是《残响之河》小说创作系统的 AI 秘书「中枢」。

## 你的身份与边界
- 你负责理解用户意图、回答问题、查询系统状态。
- 你**不是**小说作者。绝不编造任何世界观设定、角色名、地点、势力。
  用户问设定时，只依据下面给出的【实时系统状态】回答；状态里没有的，就直说"资料库里没有记录"。
- 你**不直接执行**写操作，但系统会在你判出写意图后**自动把任务入队**。
  所以你的 reply 里**绝不要**说「需要你点确认」「确认后才会执行」「我不会动手」
  这类话——那与事实不符，会让用户以为没提交成功。正确说法是描述你理解到的
  需求（文明、数量、类别），例如「好，为星耀联邦建 5 张卡」。入队结果由系统
  自动追加在后面，不需要你操心。

## 你能做的事（工具清单）
{tools}

## 输出格式（必须是合法 JSON，不要 markdown 代码块围栏）
{{
  "reply": "给用户看的中文回复，自然、简洁、有信息量。2~5 句。",
  "intent": "上面工具清单里的 intent 值之一，纯闲聊用 chat",
  "needs_confirm": false 或 true,
  "params": {{}}  // intent 需要参数时填，如 {{"civ":"太玄帝国","count":5}}
}}

## 【极重要】reply 字段里禁止出现英文双引号 "
你要引用词语时，一律用中文引号「」或书名号《》，绝不要用 " "。
原因：英文双引号在 JSON 字符串里必须转义，而模型经常忘记转义，
导致整个 JSON 解析失败，用户看到一堆乱码。用「」永远不会出问题。

## 判断原则
- 用户在问「你能做什么」→ intent=chat，正常介绍你能做的事，不要复读状态数字。
- 用户在问具体数据 → 用【实时系统状态】里的真实数字回答，不许瞎猜。
- 用户提到「失败 / 哪些失败了 / 跑挂了 / 报错 / 没成功 / 被拒」这类词，
  哪怕句子很短、没带"任务"二字（如「那些失败的」），一律 intent=failures，
  不要回"没听懂"。这是最容易激怒用户的误判：他心里有明确的一件事，
  我们却说不知道他在说什么。
- 用户在下达写指令（"建卡"/"写第N章"）→ intent 填对应值，needs_confirm=true，
  params 里填全参数（cardgen 要 civ + count；write_chapter 要 volume + chapter）。
  reply 只描述需求，**不要**说"需要确认/确认后才执行"（系统会自动入队）。
- 用户只是聊天/吐槽 → intent=chat，像个正常助手一样回话。

## 实时系统状态
{state}
"#
    .replace("{tools}", &serde_json::to_string_pretty(&tool_catalog()).unwrap_or_default())
    .replace("{state}", "")
}

/// 收集当前系统实时状态，喂给 LLM 做上下文。
///
/// 【为什么必须真实查询而不是让模型猜】
/// 用户最常问"现在有多少张卡""跑到哪了"。如果模型不知道真实数字，
/// 它会编一个看起来合理的（"目前约 700 张"），这是最伤信任的行为。
/// 所以这里先查真数据，再交给模型组织语言。
fn collect_state(conn: &Connection) -> String {
    let mut out = Vec::new();
    // 任务队列
    if let Ok(mut st) = conn.prepare("SELECT state, COUNT(*) FROM webui_task GROUP BY state") {
        if let Ok(rows) = st.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        }) {
            let items: Vec<String> = rows
                .filter_map(|x| x.ok())
                .map(|(s, n)| format!("{s}={n}"))
                .collect();
            if !items.is_empty() {
                out.push(format!("任务队列：{}", items.join(" / ")));
            }
        }
    }
    // 资料库总量
    if let Ok(n) = conn.query_row("SELECT COUNT(*) FROM materials", [], |r| r.get::<_, i64>(0)) {
        out.push(format!("资料库卡片总数：{n} 张"));
    }
    // 分类分布（前 8 类，让模型能回答"有多少势力卡"）
    if let Ok(mut st) = conn.prepare(
        "SELECT category, COUNT(*) c FROM materials GROUP BY category ORDER BY c DESC LIMIT 8",
    ) {
        if let Ok(rows) = st.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        }) {
            let items: Vec<String> = rows
                .filter_map(|x| x.ok())
                .map(|(c, n)| format!("{c} {n}"))
                .collect();
            if !items.is_empty() {
                out.push(format!("分类分布：{}", items.join("、")));
            }
        }
    }
    // 授权锁状态
    let unlocked = Path::new(super::real_llm_lock::ALLOW_FILE).exists();
    out.push(format!(
        "真实 LLM 授权锁：{}",
        if unlocked { "已解锁" } else { "已锁定（不能真跑）" }
    ));
    if out.is_empty() {
        "（暂无可用状态数据）".to_string()
    } else {
        out.join("\n")
    }
}

/// 从模型返回的 content 里抠出 JSON。
///
/// 【为什么要容错】实测部分免费模型即使被要求"只输出 JSON"，
/// 仍会包一层 ```json 围栏，或在前后加一句"好的，这是结果："。
/// 直接 serde_json::from_str 会失败，用户的聊天就崩了。
/// 所以先剥围栏、再定位第一个 `{` 到最后一个 `}`。
fn extract_json(content: &str) -> Option<Value> {
    let s = content.trim();
    // ① 直接解析
    if let Ok(v) = serde_json::from_str::<Value>(s) {
        if v.is_object() {
            return Some(v);
        }
    }
    // ② 剥 ```json ... ``` 围栏
    let stripped = if let Some(rest) = s.strip_prefix("```json") {
        rest.trim_start()
    } else if let Some(rest) = s.strip_prefix("```") {
        rest.trim_start()
    } else {
        s
    };
    let stripped = stripped.trim_end_matches("```").trim();
    if let Ok(v) = serde_json::from_str::<Value>(stripped) {
        if v.is_object() {
            return Some(v);
        }
    }
    // ③ 定位第一个 `{` 到最后一个 `}`
    if let (Some(a), Some(b)) = (stripped.find('{'), stripped.rfind('}')) {
        if b > a {
            if let Ok(v) = serde_json::from_str::<Value>(&stripped[a..=b]) {
                if v.is_object() {
                    return Some(v);
                }
            }
        }
    }
    None
}

/// L2 主入口：用 LLM 理解意图。
///
/// 返回 `Skipped` 时调用方应回落到 L1 的 fallback 文案（不要报错）。
pub fn interpret(conn: &Connection, _lib_root: &Path, message: &str) -> Result<L2Outcome> {
    // ── ① 授权锁：没开就跳过，绝不绕过 ──
    // 聊天框的 L2 是要真外呼的。用户明确要求"解锁只能由我本人点"，
    // 所以这里只检查、不创建。
    if !Path::new(super::real_llm_lock::ALLOW_FILE).exists() {
        return Ok(L2Outcome::Skipped {
            why: "真实 LLM 授权锁未打开".to_string(),
        });
    }

    let endpoint =
        std::env::var("NOVEL_LLM_ENDPOINT").unwrap_or_else(|_| DEFAULT_ENDPOINT.to_string());
    let model = std::env::var("NOVEL_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
    let key = std::env::var("NOVEL_API_KEY").unwrap_or_else(|_| DEFAULT_KEY.to_string());

    let state = collect_state(conn);
    let sys = system_prompt().replace(
        "## 实时系统状态\n",
        &format!("## 实时系统状态\n{state}\n"),
    );

    let body = json!({
        "model": model,
        "messages": [
            {"role": "system", "content": sys},
            {"role": "user", "content": message},
        ],
        "temperature": 0.3,
        "max_tokens": 1000,
        "stream": false,
    });

    let headers = vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Authorization".to_string(), format!("Bearer {key}")),
    ];

    // ── ② 外呼：零依赖 HTTP，15 秒超时 ──
    // 【为什么从 8 秒放宽到 15 秒】实测 deepseek-v4.1-flash-free 在网关
    // 负载高时（批次正在建卡、多张卡并发占满网关）会拖到 12~16 秒，
    // 8 秒会频繁超时 → 全部回落 L1 fallback → 用户看到"没听懂"。
    // 15 秒能覆盖绝大多数情况；真正卡死时也还是会在 15 秒内断开，
    // 不会让聊天框无限转圈。
    let resp = match ne_engine::http::post_json_value(
        &endpoint,
        &headers,
        &body,
        Duration::from_secs(15),
    ) {
        Ok(v) => v,
        Err(e) => {
            return Ok(L2Outcome::Skipped {
                why: format!("网关不可达：{e}"),
            })
        }
    };

    // ── ③ 抽 content ──
    let content = resp
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");

    let parsed = match extract_json(content) {
        Some(v) => v,
        None => {
            // 【2026-10-17 修】模型没按 JSON 回时，不能直接把原文塞给用户。
            //
            // 实测踩坑：deepseek 在 reply 里写了未转义的英文双引号
            // （`我只拿到了"书目"，没拿到"正文"`），JSON 解析失败 →
            // 原来这里 fallback 用原文当回复，结果用户看到的是
            // 一整坨裸 JSON：{"reply":"...","intent":"library",...}
            //
            // 正确做法：先尝试"捞"出 reply 字段的文本内容（哪怕是坏 JSON，
            // 用正则也能抠出 "reply": "..." 之间那段）；实在捞不到，
            // 才把原文当回复，并且要剥掉明显的 JSON 语法残渣。
            if let Some(txt) = salvage_reply_field(content) {
                return Ok(L2Outcome::Replied(
                    ChatReply::chat(txt, "l2_chat").to_json(),
                ));
            }
            let txt = content.trim();
            // 整段以 { 开头就当解析失败处理，不要把它丢给用户看
            if txt.is_empty() || txt.starts_with('{') {
                return Ok(L2Outcome::Skipped {
                    why: "模型输出无法解析为可读回复".to_string(),
                });
            }
            return Ok(L2Outcome::Replied(
                ChatReply::chat(txt, "l2_chat").to_json(),
            ));
        }
    };

    let reply_txt = parsed
        .get("reply")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if reply_txt.is_empty() {
        return Ok(L2Outcome::Skipped {
            why: "模型回复为空".to_string(),
        });
    }

    let intent = parsed
        .get("intent")
        .and_then(|x| x.as_str())
        .unwrap_or("chat");
    let needs_confirm = parsed
        .get("needs_confirm")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);

    // ── ④ 只读意图：允许直接执行（查状态/查资料库不改变任何东西）──
    // 【为什么这层仍走 L1 的 intent_status 而不是自己查】
    // 保证「同一个问题，无论 L1 还是 L2 命中，回答口径完全一致」。
    // L2 只负责"听懂"，具体怎么答仍交给经过验证的 L1 实现。
    match intent {
        "status" => {
            // 【session 传空串】这是 L2 内部借用 L1 实现来答状态，
            // 不是用户发言。传空串 → 不回填历史 → 不污染上下文。
            if let Ok(v) = super::chat_intent::handle(conn, _lib_root, "看当前任务状态", "") {
                return Ok(L2Outcome::Replied(with_prefix(v, &reply_txt)));
            }
        }
        "self_check" => {
            if let Ok(v) = super::chat_intent::handle(conn, _lib_root, "跑一次系统自检", "") {
                return Ok(L2Outcome::Replied(with_prefix(v, &reply_txt)));
            }
        }
        _ => {}
    }

    // ── ④.5 资料库检索：不再只依赖模型判成 library ──
    //
    // 【2026-10-18 重构理由】实测「太玄帝国是什么样的」时，模型判成 chat
    // （它觉得自己"没有正文可读"），于是完全跳过检索，回了一句
    // "我手上没有可依据的记录，不能替你编"——而库里明明有 225 张相关卡。
    // 这是最糟的体验：数据在，系统说没有。
    //
    // 根因是把"要不要查"这个决定权交给了模型，而模型看不到库里的数据，
    // 它只能凭 prompt 里那点统计数字判断，必然倾向于说"没有"。
    // 正确分工：**模型负责组织语言，检索这件事由我们无条件先做**。
    //
    // 触发条件（任一即可）：
    //   · 模型自己判成 library
    //   · 用户句子里出现了已知文明/势力/地点等专名
    //   · 用户用了查询类问法（是什么/什么样/介绍/讲讲/有哪些…）
    let wants_library = intent == "library"
        || is_lookup_question(message)
        || super::chat_intent::pick_civ_pub(message).is_some();

    if wants_library {
        // 【关键词来源优先级】模型 params > 用户原话抽取。
        // 注意与旧版相反：现在优先用**原话**抽取，因为它更接近用户意图；
        // 模型 params 只在原话抽不出东西时兜底。
        let from_msg = extract_keyword(message);
        let from_llm = parsed
            .get("params")
            .and_then(|p| p.get("keyword").or_else(|| p.get("civ")))
            .and_then(|x| x.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.to_string());
        let term = from_msg.or(from_llm).unwrap_or_default();

        if !term.is_empty() {
            let hits = search_cards(conn, &term, 12);
            let mut txt = String::new();
            if !reply_txt.is_empty() {
                txt.push_str(&reply_txt);
            }
            if !hits.is_empty() {
                if !txt.is_empty() {
                    txt.push_str("\n\n");
                }
                txt.push_str(&format!("资料库里查到 {} 张「{term}」相关卡：\n", hits.len()));
                for (code, name, cat, chars) in &hits {
                    txt.push_str(&format!("· {code}｜{name}（{cat}，{chars} 字）\n"));
                }
            } else if is_specific_entity(&term) {
                // 只有"看起来像专名"时才说查不到，避免泛问（"有多少张卡"）
                // 被追加一句自相矛盾的"没有「现在有多少张卡」相关的卡"。
                if !txt.is_empty() {
                    txt.push_str("\n\n");
                }
                txt.push_str(&format!("我把资料库翻了一遍，没有「{term}」相关的卡片。"));
            }
            return Ok(L2Outcome::Replied(
                ChatReply::chat(txt.trim_end(), "l2_library").to_json(),
            ));
        }
    }

    // ── ⑤ 写意图：真入队（不再只回一句"确认"）──
    //
    // 【2026-10-18 重构 · 用户明确指令】原实现只回 needs_confirmation=true，
    // 用户点确认后前端还得再发一次请求，实测体验是"系统什么也没干"。
    // 现在改为：模型识别出写意图 → **立刻 submit 进队列**，回复里带任务号。
    //
    // 【为什么"入队"是安全的，不违反授权铁则】
    //   · 入队只是往 webui_task 写一条 pending 记录，**不发生任何外呼**
    //   · 真跑仍要过两道闸：面板点「执行」+ /sdcard/wogov_real_llm.allow 解锁
    //   · 所以"自动入队"与"真跑必须人工授权"并不冲突，反而是正确的分工：
    //     聊天负责表达意图 → 队列负责排队 → 人工决定何时真跑
    //
    // 【real 的口径】这里一律 real=false（预演）。理由：预演不花额度、
    // 不写卡、不会撞号，跑完能验结构；真跑由用户在面板上决定。
    // 用户若在句子里明说"真跑/real"，才置 true——但真跑仍需锁，锁没开
    // task_store::submit 会把它标成 blocked，不会偷偷外呼。
    if needs_confirm && (intent == "cardgen" || intent == "write_chapter") {
        let params = parsed.get("params").cloned().unwrap_or(json!({}));
        if let Some(v) = submit_write_task(conn, intent, &params, &reply_txt, message) {
            return Ok(L2Outcome::Replied(v));
        }
        // submit 失败才退回"只回确认"，不能让用户以为提交成功
        let mut r = ChatReply::chat(&reply_txt, &format!("l2_{intent}"));
        r.needs_confirmation = true;
        r.confirm = Some(json!({
            "intent": intent,
            "params": params,
            "warning": "这是写操作，确认后才会真正执行。",
        }));
        return Ok(L2Outcome::Replied(r.to_json()));
    }

    // ── ⑥ 其余（闲聊/答疑）：直接用模型的话 ──
    Ok(L2Outcome::Replied(
        ChatReply::chat(&reply_txt, &format!("l2_{intent}")).to_json(),
    ))
}

/// 在资料库里模糊搜卡。
///
/// 【为什么不在 LLM 提示词里塞全库 700 张卡】
/// 700 张卡的清单约 3 万字，塞进 prompt 会把 token 打爆且拖慢 3~8 秒的响应。
/// 正确做法：模型只负责识别"用户想查什么"，真正检索由 SQL 做，
/// 只把命中的前 N 条回填给用户。
///
/// 【2026-10-18 修 · 逐级回退】
/// 实测踩坑：用户问「太玄帝国是什么样的」，extract_keyword 剥掉"是什么"
/// 却留下"样"，得到「太玄帝国什么样」→ LIKE 命中 **0 张**，
/// 于是模型说"资料库里没有太玄帝国"，而实际有 225 张。
/// 这是最伤人的假答案：数据明明在，系统说没有。
///
/// 现在改为：先试完整词，查不到就依次砍掉尾部字重试，
/// 一旦命中就停。宁可给略宽的匹配，也不能对已有数据说"没有"。
///
/// 返回 (code, name, category, content_chars) 列表。
fn search_cards(
    conn: &Connection,
    kw: &str,
    limit: usize,
) -> Vec<(String, String, String, i64)> {
    let term = extract_keyword(kw).unwrap_or_else(|| kw.to_string());
    if term.trim().is_empty() {
        return Vec::new();
    }

    // 逐级回退逻辑抽到了 `search_cards_fallback`（FC 层共用同一份）。
    // 这里只负责"先把噪声词剥掉"，回退细节不在此处维护。
    search_cards_fallback(conn, &term, limit)
}

/// 单次精确 LIKE 检索（不做关键词提取，调用方负责给干净的词）。
fn search_cards_exact(
    conn: &Connection,
    term: &str,
    limit: usize,
) -> Vec<(String, String, String, i64)> {
    let pat = format!("%{}%", term);
    let mut out = Vec::new();
    // 【2026-10-17 修】排序必须按"相关度"，不能按字数。
    //
    // 实测踩坑：原 SQL 是 `ORDER BY content_chars DESC`，
    // 查「太玄帝国」返回的第一张卡是「星耀城（6623 字）」——
    // 只因为它在 content 正文里偶然提到过一次"太玄帝国"，而字数最多。
    // 用户看到的就是"我问太玄帝国，你给我星耀城"，完全不可接受。
    //
    // 正确相关度优先级：
    //   0. 名字完全等于关键词（CIVI-...｜太玄帝国）
    //   1. 名字包含关键词（太玄帝国诡异志）
    //   2. sub_category 等于关键词（该文明下属的所有卡）
    //   3. 正文里提到（兜底，按字数排）
    let sql = "SELECT code, name, category, COALESCE(content_chars,0) FROM materials \
               WHERE (name LIKE ?1 OR sub_category LIKE ?1 OR content LIKE ?1) \
               AND name NOT LIKE '(占位)%' \
               AND COALESCE(stale,0) = 0 \
               ORDER BY CASE \
                 WHEN name = ?2 THEN 0 \
                 WHEN name LIKE ?1 THEN 1 \
                 WHEN sub_category = ?2 THEN 2 \
                 ELSE 3 END, \
                 COALESCE(content_chars,0) DESC \
               LIMIT ?3";
    if let Ok(mut st) = conn.prepare(sql) {
        if let Ok(rows) = st.query_map(
            rusqlite::params![pat, term, limit as i64],
            |r| {
                Ok((
                    r.get::<_, String>(0).unwrap_or_default(),
                    r.get::<_, String>(1).unwrap_or_default(),
                    r.get::<_, String>(2).unwrap_or_default(),
                    r.get::<_, i64>(3).unwrap_or(0),
                ))
            },
        ) {
            for x in rows.flatten() {
                out.push(x);
            }
        }
    }
    out
}

/// 从自然语言里抠出检索关键词。
///
/// 例：「帮我看看星耀联邦的设定」→ `星耀联邦`
///     「太玄帝国有哪些势力」    → `太玄帝国`
///
/// 做法：先按噪声词表剥掉口语前缀/后缀，再取剩下的最长连续中文片段。
/// 之所以不直接整句 LIKE：整句带上"帮我看看""的设定"永远匹配不到。
fn extract_keyword(msg: &str) -> Option<String> {
    // 【2026-10-17 修】原词表缺"势力/角色/地图/卡"这类通用类目词，
    // 导致「太玄帝国有哪些势力」被拆成 ["太玄帝国", "势力"] 两段，
    // 而"势力"长度 2 < "太玄帝国" 长度 4，虽然这次侥幸选对了，
    // 但「混沌帝国有哪些势力」这类也可能反过来。更稳的做法是
    // 把类目词一起当噪声剥掉，只留专名。
    const NOISE: &[&str] = &[
        // 口语前缀
        "帮我看看", "帮我查查", "帮我", "看看", "查查", "查一下", "看一下", "我想知道",
        "我想看看", "能不能", "可不可以", "麻烦",
        // 口语后缀 / 疑问
        "的设定", "设定", "的资料", "资料", "有哪些", "是什么", "什么", "吗", "呢", "？", "?",
        // 【2026-10-18 补】疑问/描述类短语。实测「太玄帝国是什么样的」
        // 剥掉"是什么"后剩"样"，拼成「太玄帝国什么样」→ 命中 0 张，
        // 于是模型回"库里没有太玄帝国"，而实际有 225 张。
        //
        // 【顺序极其关键】replace 是按数组顺序逐个执行的，
        // 长词必须排在短词之前，否则短词先命中会把长词切碎：
        //   「星耀联邦长什么样」若先替换"什么样"→ 剩「星耀联邦长」，
        //   多出一个孤立的"长"，反而查不到（实测就是这么挂的）。
        // 所以：长什么样 → 是什么样的 → 是什么样 → 怎么样的 → 什么样 …
        "长什么样", "是什么样的", "是什么样", "怎么样的", "是啥样", "啥样",
        "怎么样", "什么样", "怎样的",
        "简介", "介绍", "讲讲", "说说", "详细说说", "大概", "情况", "样子",
        // 【2026-10-18 补】动词后的残余虚词。实测「介绍一下混沌帝国」
        // 先剥"介绍"再剥"一下"若缺后者，会剩「一下混沌帝国」查不到。
        "一下", "一下子", "一段", "一句", "呢啊", "啊", "呀", "吧",
        // 【2026-10-18 补】"讲了什么"族。实测「残响之河讲了什么故事」
        // 剥掉"什么""故事"后剩「残响之河讲了」——多一个孤立的"讲了"，
        // 于是 LIKE 命中一堆无关卡。整组一并剥掉。
        "讲了什么", "讲了啥", "说什么", "说的是什么", "讲的什么",
        "讲了", "讲的是", "说的", "说的是",
        // 通用类目词（这些不是专名，剥掉才能留下真正的文明/组织名）
        "势力", "角色", "人物", "地点", "地图", "功法", "科技", "文明", "文化",
        "因果线", "时间线", "历史",
    ];
    let mut t = msg.trim().to_string();
    // 【2026-10-18 修 · 结构性修复】噪声词必须**按长度降序**替换。
    //
    // 为什么不能靠"手写数组顺序"：实测「星耀联邦长什么样」被提取成
    // 「星耀联邦长」——因为数组里较早的短词"是什么"先命中了
    // "什么样"里的"什么"，把长词组切碎，剩下一个孤立的"长"。
    // 这种人肉维护的顺序约束极易出错：加一个新词就可能破坏旧词。
    //
    // 改成运行时按长度排序后，长词天然优先，永远不需要关心书写顺序。
    let mut words: Vec<&str> = NOISE.to_vec();
    words.sort_by_key(|w| std::cmp::Reverse(w.chars().count()));
    for n in words {
        t = t.replace(n, " ");
    }
    // 取最长的连续中文片段（≥2 字），这是最可能是专名的部分
    let mut best = String::new();
    let mut cur = String::new();
    for ch in t.chars() {
        if ch.is_whitespace() || ch.is_ascii_punctuation() || (!ch.is_alphanumeric() && ch.len_utf8() > 0 && !is_cjk(ch)) {
            if cur.chars().count() > best.chars().count() {
                best = cur.clone();
            }
            cur.clear();
        } else {
            cur.push(ch);
        }
    }
    if cur.chars().count() > best.chars().count() {
        best = cur;
    }
    let b = best.trim().to_string();
    if b.chars().count() >= 2 {
        Some(b)
    } else {
        None
    }
}

fn is_cjk(c: char) -> bool {
    let u = c as u32;
    (0x4E00..=0x9FFF).contains(&u)      // 基本汉字
        || (0x3400..=0x4DBF).contains(&u)  // 扩展 A
        || (0x3000..=0x303F).contains(&u)  // 中文标点
}

/// 从"坏 JSON"里把 reply 字段的文本捞出来。
///
/// 场景：模型输出 `{"reply":"我只拿到了"书目"，没拿到"正文"","intent":"library"}`
/// —— reply 内部的双引号没转义，整段不是合法 JSON，serde 直接报错。
/// 但用户真正想看的只是那段中文，所以用正则把 `"reply"` 后面第一个引号
/// 到下一个 `","intent"` 之间的内容抠出来。
///
/// 只处理 reply 一个字段：其它字段（intent/needs_confirm/params）坏掉时
/// 拿不到也不影响——默认值 chat/false 本来就是安全兜底。
fn salvage_reply_field(content: &str) -> Option<String> {
    let s = content;
    // 定位 "reply" 之后的冒号
    let start = s.find("\"reply\"")?;
    let after = &s[start + 7..];
    let colon = after.find(':')?;
    let after_colon = after[colon + 1..].trim_start();
    // 必须以引号开头（否则 reply 不是字符串，无从捞取）
    let body = after_colon.strip_prefix('"')?;
    // 找到结束位置：后面紧跟 `","` 或 `"\n` 或 `"}` 的那个引号
    // （不用 rfind，避免把后面字段的内容也吞进来）
    let mut end = None;
    let bytes: Vec<char> = body.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == '"' {
            // 看后面几个非空字符
            let rest: String = bytes[i + 1..].iter().take(12).collect();
            let r = rest.trim_start();
            if r.starts_with(",}") || r.starts_with(",\"") || r.starts_with('}') || r.is_empty() {
                end = Some(i);
                break;
            }
        }
        i += 1;
    }
    let e = end?;
    let txt: String = bytes[..e].iter().collect();
    let txt = txt.trim().to_string();
    if txt.is_empty() || txt.starts_with('{') {
        None
    } else {
        Some(txt)
    }
}

/// 判断用户这句话是不是"在问某个具体设定"（需要去库里捞真数据）。
///
/// 【为什么不能只靠模型的 intent=library】
/// 实测：「太玄帝国是什么样的」模型判成 chat，理由冠冕堂皇——
/// "我手上没有正文可读，不能替你编"。听起来负责，实际是假答案：
/// 库里有 225 张相关卡，它是**没去查**就说没有。
/// 所以"要不要查"必须由代码根据用户的问法决定，不能交给看不见数据的模型。
///
/// 只认明确的查询/介绍类问法，避免把"今天好累啊"这种闲聊也拖去查库。
fn is_lookup_question(msg: &str) -> bool {
    const CUES: [&str; 20] = [
        "是什么", "什么样", "什么样的", "怎么样", "介绍", "讲讲", "说说", "简介",
        "详细", "有哪些", "哪个", "谁", "设定", "背景", "故事", "讲了什么",
        "是什么样的人", "长什么样", "世界观", "梗概",
    ];
    CUES.iter().any(|c| msg.contains(c))
}

/// 判断抽出来的词像不像"具体实体名"（文明/组织/人名），而不是泛问短语。
///
/// 【为什么需要】「现在有多少张卡」这种泛问，抽词会抽出整句，
/// 拿它去 SQL LIKE 必然查不到。此时不能告诉用户"库里没有这个词"——
/// 用户问的是总数，模型已经答对了，加一句"查不到"反而自相矛盾。
///
/// 判据：
///   · 长度 3~8 字（真正的专名：「太玄帝国」4 字、「星耀联邦」4 字、「青苔社」3 字）
///   · 不含疑问词/数字/口语词（含了就说明是问句碎片，不是专名）
fn is_specific_entity(term: &str) -> bool {
    let n = term.chars().count();
    if n < 3 || n > 8 {
        return false;
    }
    const BAD: [&str; 21] = [
        "多少", "什么", "怎么", "哪些", "哪个", "现在", "目前", "一共", "总共", "共有",
        "张卡", "卡片", "资料", "库里", "系统", "任务", "状态", "进度", "情况", "告诉",
        "帮我看",
    ];
    if BAD.iter().any(|b| term.contains(b)) {
        return false;
    }
    // 含阿拉伯数字的多半是"第3卷"这类，不是实体名
    if term.chars().any(|c| c.is_ascii_digit()) {
        return false;
    }
    true
}

/// 把 LLM 的话放在前面、L1 的结构化回答放在后面。
///
/// 【为什么这么拼】用户想要"像人一样回话"，但状态数字必须准。
/// 让模型说人话开场，再把机器查的准确数据附上，两者兼得。
fn with_prefix(mut v: Value, prefix: &str) -> Value {
    if prefix.is_empty() {
        return v;
    }
    if let Some(o) = v.as_object_mut() {
        let old = o
            .get("reply")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        o.insert(
            "reply".to_string(),
            json!(format!("{prefix}\n\n{old}")),
        );
        o.insert("matched".to_string(), json!("l2_plus_l1"));
    }
    v
}

/// 把 L2 识别出的写意图真正 submit 进队列，返回可直接回给用户的 ChatReply。
///
/// 返回 None 表示"没能入队"（参数不足/ submit 报错），调用方应退回"只回确认"，
/// 绝不能让用户以为任务已经提交成功。
fn submit_write_task(
    conn: &Connection,
    intent: &str,
    params: &Value,
    llm_reply: &str,
    raw_msg: &str,
) -> Option<Value> {
    let mut r = ChatReply::chat("", &format!("l2_{intent}"));

    match intent {
        "cardgen" => {
            // ── ① 定文明 ──
            // 【实测坑】模型常把「星耀」原样返回，而库里正名是「星耀联邦」。
            // 直接用会导致 civ_context 查不到上下文、建出来的卡缺依据。
            // 所以：先用模型值，再用用户原话，最后在 CIVS 里做前缀/包含匹配。
            let from_llm = params
                .get("civ")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let civ = resolve_civ(&from_llm)
                .or_else(|| super::chat_intent::pick_civ_pub(raw_msg).map(|s| s.to_string()))
                .or_else(|| {
                    if from_llm.is_empty() {
                        None
                    } else {
                        Some(from_llm.clone())
                    }
                })?;

            // ── ② 定数量 ──
            let n = params
                .get("count")
                .and_then(|x| {
                    x.as_u64()
                        .map(|v| v as u32)
                        .or_else(|| x.as_str().and_then(|s| s.parse().ok()))
                })
                .unwrap_or(10)
                .clamp(1, 40);

            // ── ③ 定规格 ──
            // 模型给了 spec 字符串就透传，没给就按 L1 同一套推断，
            // 保证"同一句话无论走 L1 还是 L2，建出来的卡分布一致"。
            let spec = params
                .get("spec")
                .and_then(|x| x.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| super::chat_intent::infer_spec_pub(raw_msg, n));

            // ── ④ real 口径 ──
            // 只有用户明说"真跑/实际生成/real"才为 true；且真跑仍需锁，
            // 锁没开 task_store 会把它标 blocked，绝不会偷偷外呼。
            let wants_real = raw_msg.contains("真跑")
                || raw_msg.contains("真生成")
                || raw_msg.contains("实际生成")
                || params.get("real").and_then(|x| x.as_bool()).unwrap_or(false);

            let payload = json!({
                "civ": civ,
                "spec": spec,
                "real": wants_real,
                "count": n,
            });

            match super::task_store::submit(
                conn,
                &json!({"kind": "cardgen", "payload": payload}).to_string(),
            ) {
                Ok(v) => {
                    let id = v["id"].as_str().unwrap_or("").to_string();
                    let ok = v["ok"].as_bool().unwrap_or(false);
                    if ok {
                        let block = format!(
                            "✅ 任务已入队：{id}\n\
                             · 文明：{civ}\n\
                             · 数量：{n} 张（{spec}）\n\
                             · 模式：{}\n\n\
                             点右下角悬浮球或看板「▶ 执行待处理任务」即可开跑。",
                            if wants_real {
                                "真跑（需授权锁）"
                            } else {
                                "预演（real=false，不写卡、不花额度）"
                            }
                        );
                        r.reply = merge_reply_with_result(llm_reply, &block);
                        r.task_ids.push(id);
                    } else {
                        let block = format!(
                            "⚠ 任务没能入队：{}",
                            v["error"].as_str().unwrap_or("未知原因")
                        );
                        r.reply = merge_reply_with_result(llm_reply, &block);
                    }
                    r.hint = Some(
                        "真跑请先在锁条解锁；跑完会自动上锁。".into(),
                    );
                    Some(r.to_json())
                }
                Err(e) => {
                    r.reply = format!("{} \n\n⚠ 登记失败：{e}", llm_reply.trim());
                    Some(r.to_json())
                }
            }
        }
        "write_chapter" => {
            // 章节写作用 file_gated 路径：真跑由 l5 自身的锁判定。
            let volume = params
                .get("volume")
                .and_then(|x| x.as_str())
                .unwrap_or("卷01")
                .to_string();
            let chapter = params
                .get("chapter")
                .and_then(|x| {
                    x.as_u64()
                        .map(|v| v as i64)
                        .or_else(|| x.as_str().and_then(|s| s.parse().ok()))
                })
                .unwrap_or(1);
            let real = params.get("real").and_then(|x| x.as_bool()).unwrap_or(false);

            let payload = json!({
                "volume": volume,
                "chapter": chapter,
                "real": real,
            });
            match super::task_store::submit(
                conn,
                &json!({"kind": "chapter_gen", "payload": payload}).to_string(),
            ) {
                Ok(v) => {
                    let id = v["id"].as_str().unwrap_or("").to_string();
                    if v["ok"].as_bool().unwrap_or(false) {
                        let block = format!(
                            "✅ 已登记写正文任务：{id}（{volume} 第 {chapter} 章，real={real}）"
                        );
                        r.reply = merge_reply_with_result(llm_reply, &block);
                        r.task_ids.push(id);
                    } else {
                        let block = format!(
                            "⚠ 未能登记：{}",
                            v["error"].as_str().unwrap_or("未知原因")
                        );
                        r.reply = merge_reply_with_result(llm_reply, &block);
                    }
                    Some(r.to_json())
                }
                Err(e) => {
                    r.reply = format!("{} \n\n⚠ 登记失败：{e}", llm_reply.trim());
                    Some(r.to_json())
                }
            }
        }
        _ => None,
    }
}

/// 把模型的人话与"真实执行结果"合成最终回复。
///
/// 【为什么不能直接拼接模型的 reply】
/// 实测踩坑：模型按 prompt 里"写操作要用户确认"的旧规则，会自己写
/// 「这属于写操作，我这边不会直接动手——需要你点一下确认按钮」。
/// 但现在架构已经改成**自动入队**了，如果原样保留这句，用户会读到
/// "需要你确认" + "✅ 任务已入队" 两条互相矛盾的信息，完全不知道该信谁。
///
/// 正确做法：真实结果（入队成功/失败）是唯一事实，模型的话只保留
/// 不带"确认/不会动手"这种动作承诺的部分。宁可少说，不能自相矛盾。
fn merge_reply_with_result(llm_reply: &str, result_block: &str) -> String {
    let cleaned = strip_action_promises(llm_reply);
    let mut out = String::new();
    if !cleaned.is_empty() {
        out.push_str(&cleaned);
        out.push_str("\n\n");
    }
    out.push_str(result_block);
    out
}

/// 删掉模型回复里与"自动入队"现实冲突的动作承诺句。
///
/// 按句子切分（中英文句号/感叹号/问号/换行），逐句判定，命中则丢弃。
/// 只丢那几句，保留其余有价值的信息（数量、文明、规格说明等）。
fn strip_action_promises(text: &str) -> String {
    // 这些短语说明模型在说"我不会动手/等你确认"，与已入队的事实冲突
    const CONFLICT: [&str; 12] = [
        "点一下确认",
        "点击确认",
        "点确认",
        "确认按钮",
        "需要你确认",
        "等你确认",
        "确认后才会",
        "确认后就会",
        "不会直接动手",
        "不会立即执行",
        "需要你同意",
        "先确认",
    ];
    let mut kept: Vec<String> = Vec::new();
    for line in text.split('\n') {
        // 再按中文句末标点切句
        for seg in line.split_inclusive(['。', '！', '？', '!', '?', '；', ';']) {
            let s = seg.trim();
            if s.is_empty() {
                continue;
            }
            if CONFLICT.iter().any(|k| s.contains(k)) {
                continue; // 丢掉这句
            }
            kept.push(s.to_string());
        }
    }
    kept.join("")
        .trim()
        .to_string()
}

/// 把模型返回的文明简称补齐成正名（「星耀」→「星耀联邦」）。
/// 用的是精确 sub_category 匹配。模型吐简称会直接导致「查不到该文明设定」，
/// 建出来的卡没有依据 → 变成孤立卡。
fn resolve_civ(raw: &str) -> Option<String> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    const CIVS: [&str; 12] = [
        "太玄帝国",
        "天启帝国",
        "星耀联邦",
        "星际商会",
        "暗影王朝",
        "混沌帝国",
        "蛮族",
        "逻辑族",
        "万界通途",
        "圣辉教廷",
        "归墟古族",
        "界碑交汇所",
    ];
    // ① 精确命中
    if let Some(c) = CIVS.iter().find(|c| **c == t) {
        return Some((*c).to_string());
    }
    // ② 全名包含用户给的简称（"星耀" ⊂ "星耀联邦"）
    if let Some(c) = CIVS.iter().find(|c| c.contains(t) && !t.is_empty()) {
        return Some((*c).to_string());
    }
    // ③ 用户给的全名包含正名（"星耀联邦帝国" 这种赘述）
    if let Some(c) = CIVS.iter().find(|c| t.contains(*c)) {
        return Some((*c).to_string());
    }
    None
}

/// 供 `chat_tools_fc.rs`（L2 工具层）复用的文明名归一化。
///
/// 【为什么必须共用】模型经常把「星耀」原样当 civ 返回，而库里正名是
/// 「星耀联邦」。`civ_context()` 用精确 `sub_category` 匹配，简称直接
/// 查不到上下文 → 建出来的卡没有依据 → 变成孤立卡。
pub fn resolve_civ_pub(raw: &str) -> Option<String> {
    resolve_civ(raw)
}

/// 供 `chat_tools_fc.rs`（L2 工具层）复用的卡片检索（带逐级回退）。
///
/// 【与 `search_cards` 的区别】这里**不调用 `extract_keyword`**。
/// 走 Function Calling 时 keyword 是模型给的干净专名（如「星耀」），
/// 再剥一遍噪声词反而可能把有效字剥掉。但仍然保留逐级回退：
/// 模型偶尔会给「星耀联邦的文明设定」这种长料，回退能救回来。
pub fn search_cards_pub(
    conn: &Connection,
    term: &str,
    limit: usize,
) -> Vec<(String, String, String, i64)> {
    search_cards_fallback(conn, term, limit)
}

/// 纯精确检索（不做任何回退），供需要严格匹配的场合。
pub fn search_cards_exact_pub(
    conn: &Connection,
    term: &str,
    limit: usize,
) -> Vec<(String, String, String, i64)> {
    search_cards_exact(conn, term, limit)
}

/// 逐级回退检索的核心：完整词 → 砍尾字 → … → 最少 2 字。
///
/// 【为什么抽出来】`search_cards`（L2 旧路径，需先剥噪声词）和
/// `search_cards_pub`（FC 路径，词已干净）都要这套回退。
/// 各写一份迟早漂移：一边改了回退下限，另一边忘改，就会出现
/// "L1 查得到、L2 查不到"这种最难查的错。
fn search_cards_fallback(
    conn: &Connection,
    term: &str,
    limit: usize,
) -> Vec<(String, String, String, i64)> {
    let t = term.trim();
    if t.is_empty() {
        return Vec::new();
    }
    let chars: Vec<char> = t.chars().collect();
    let mut tried: Vec<String> = Vec::new();
    for keep in (2..=chars.len()).rev() {
        let candidate: String = chars[..keep].iter().collect();
        if tried.contains(&candidate) {
            continue;
        }
        tried.push(candidate.clone());
        let hits = search_cards_exact(conn, &candidate, limit);
        if !hits.is_empty() {
            return hits;
        }
    }
    Vec::new()
}

pub fn try_fallback(conn: &Connection, lib_root: &Path, message: &str) -> Option<Value> {
    match interpret(conn, lib_root, message) {
        Ok(L2Outcome::Replied(v)) => Some(v),
        Ok(L2Outcome::Skipped { why }) => {
            // 记一条日志便于排查（不打扰用户）
            eprintln!("[chat L2] skipped: {why}");
            None
        }
        Err(e) => {
            eprintln!("[chat L2] error: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_plain_json() {
        let v = extract_json(r#"{"reply":"hi","intent":"chat"}"#).unwrap();
        assert_eq!(v["reply"], "hi");
    }

    #[test]
    fn extract_fenced_json() {
        let v = extract_json("```json\n{\"reply\":\"hi\"}\n```").unwrap();
        assert_eq!(v["reply"], "hi");
    }

    #[test]
    fn extract_json_with_prose() {
        let v = extract_json("好的，结果如下：{\"reply\":\"hi\"} 希望有帮助").unwrap();
        assert_eq!(v["reply"], "hi");
    }

    #[test]
    fn extract_garbage_returns_none() {
        assert!(extract_json("完全不是 JSON").is_none());
    }

    #[test]
    fn catalog_has_eight_intents() {
        // 【2026-10-18】原来断言 7，新增 failures 意图后变 8。
        // 同时断言 failures 真的在里面 —— 否则这条测试只保证"数量对"，
        // 有人删掉一个再补一个无关的也能过，形同虚设。
        let arr = tool_catalog();
        let arr = arr.as_array().unwrap();
        assert_eq!(arr.len(), 8);
        assert!(
            arr.iter().any(|x| x["intent"] == "failures"),
            "failures 必须在能力清单里，否则 L2 不知道有这能力，仍会判成 chat"
        );
    }

    /// 文明简称必须补齐成正名，否则建卡时查不到文明上下文 → 产出孤立卡。
    #[test]
    fn resolve_civ_expands_short_names() {
        assert_eq!(resolve_civ("星耀").as_deref(), Some("星耀联邦"));
        assert_eq!(resolve_civ("太玄").as_deref(), Some("太玄帝国"));
        assert_eq!(resolve_civ("星耀联邦").as_deref(), Some("星耀联邦"));
        assert_eq!(resolve_civ("界碑").as_deref(), Some("界碑交汇所"));
        // 认不出的返回 None，交由调用方回退到原话匹配
        assert_eq!(resolve_civ("莫须有文明"), None);
        assert_eq!(resolve_civ(""), None);
    }

    /// 模型回复里"等你点确认"这类话必须被剔除。
    ///
    /// 锁的是实测踩到的坑：架构改成自动入队后，模型仍按旧 prompt 写
    /// 「需要你点一下确认按钮」，用户就会读到"需要确认"+"✅已入队"
    /// 两条互相矛盾的信息，完全不知道该信谁。
    #[test]
    fn strip_action_promises_removes_conflicting_sentences() {
        let raw = "好的，你要为星耀建 5 张卡。这属于写操作，我这边不会直接动手。\
                   需要你点一下确认按钮，确认后才会真正建卡。数量是 5 张。";
        let out = strip_action_promises(raw);
        assert!(!out.contains("点一下确认"), "不能保留确认承诺：{out}");
        assert!(!out.contains("不会直接动手"), "不能保留'不动手'：{out}");
        assert!(!out.contains("确认后才会"), "不能保留'确认后'：{out}");
        // 有价值的信息必须保住
        assert!(out.contains("5 张"), "应保留数量信息：{out}");
        assert!(out.contains("星耀"), "应保留文明信息：{out}");
    }

    /// merge 后的事实以真实结果为锚，模型的话在前、结果在后。
    #[test]
    fn merge_reply_puts_result_last() {
        let out = merge_reply_with_result("好的，为星耀建 5 张卡。", "✅ 任务已入队：wt-abc");
        assert!(out.ends_with("✅ 任务已入队：wt-abc"));
        // 空回复时只剩结果，不留前导空行
        let out2 = merge_reply_with_result("", "✅ 任务已入队：wt-abc");
        assert_eq!(out2, "✅ 任务已入队：wt-abc");
    }

    /// 疑问短语必须被剥干净，只留专名。
    ///
    /// 锁实测 bug：「太玄帝国是什么样的」曾提取出「太玄帝国什么样」，
    /// LIKE 命中 0 张，导致系统对着 225 张真实卡说"库里没有太玄帝国"。
    #[test]
    fn extract_keyword_strips_question_phrases() {
        assert_eq!(extract_keyword("太玄帝国是什么样的").as_deref(), Some("太玄帝国"));
        assert_eq!(extract_keyword("星耀联邦长什么样").as_deref(), Some("星耀联邦"));
        assert_eq!(extract_keyword("介绍一下混沌帝国").as_deref(), Some("混沌帝国"));
        assert_eq!(extract_keyword("太玄帝国有哪些势力").as_deref(), Some("太玄帝国"));
        // 「讲了什么」族不能留下孤立的"讲了"
        assert_eq!(extract_keyword("残响之河讲了什么故事").as_deref(), Some("残响之河"));
    }

    /// 查询类问法必须被识别为"要查库"，不能靠模型自觉。
    ///
    /// 锁实测 bug：「太玄帝国是什么样的」模型判成 chat，回"我没有可依据的记录"，
    /// 而库里有 225 张卡——它是没去查就说没有。
    #[test]
    fn lookup_questions_are_detected() {
        assert!(is_lookup_question("太玄帝国是什么样的"));
        assert!(is_lookup_question("介绍一下混沌帝国"));
        assert!(is_lookup_question("星耀联邦有哪些势力"));
        assert!(is_lookup_question("残响之河讲了什么故事"));
        // 闲聊不能被当成查库，否则「今天好累啊」也会去翻资料库
        assert!(!is_lookup_question("今天好累啊"));
        assert!(!is_lookup_question("你好"));
        assert!(!is_lookup_question("谢谢"));
    }

    /// 逐级回退：即便关键词带了尾巴，也要能捞到数据，绝不对已有数据说"没有"。
    ///
    /// 用一个内存库验证：库里只有 "太玄帝国"，但搜索词是 "太玄帝国什么样"，
    /// search_cards 应通过砍尾字回退命中它。
    #[test]
    fn search_cards_falls_back_by_truncating_tail() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE materials(
                code TEXT, name TEXT, category TEXT, sub_category TEXT,
                content TEXT, content_chars INTEGER, stale INTEGER DEFAULT 0
             );
             INSERT INTO materials(code,name,category,sub_category,content,content_chars,stale)
             VALUES('CIVI-99-004','太玄帝国','文明','太玄帝国','太玄帝国，太玄。',3450,0);",
        )
        .unwrap();

        // 直接传整句（含噪声）。extract_keyword 会剥掉"是什么样的"，
        // 万一剥不干净，逐级回退仍要命中。
        let hits = search_cards(&conn, "太玄帝国是什么样的", 5);
        assert_eq!(hits.len(), 1, "必须命中已有的太玄帝国卡");
        assert_eq!(hits[0].0, "CIVI-99-004");

        // 隔离卡（stale=1）不能被搜出来
        conn.execute("UPDATE materials SET stale=1", []).unwrap();
        let hits2 = search_cards(&conn, "太玄帝国", 5);
        assert!(hits2.is_empty(), "stale=1 的卡不该出现在检索结果里");
    }
}
