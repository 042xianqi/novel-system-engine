//! L2 工具调用层（Function Calling）—— 2026-10-18
//!
//! # 为什么要有这个模块
//!
//! 原来的 L2 是「让模型吐 JSON，我们在 Rust 里 if-else 分发」。这条路有两个硬伤：
//!
//! 1. **意图枚举是封闭的**。模型只能在 8 个写死的 intent 里选，用户说
//!    「星耀那边长啥样」时，模型只能硬塞进 library 或 chat，塞错了就答非所问。
//! 2. **参数是我们自己从中文里抠的**。`extract_keyword` 那套噪声词表就是
//!    这个缺陷的症状——为了从「太玄帝国是什么样的」里抠出「太玄帝国」，
//!    不得不穷举"什么样的/是什么样/长什么样…"，永远补不完。
//!
//! 正确做法：把工具签名交给模型，让**模型自己**决定调哪个工具、参数填什么。
//! 它天生就懂「星耀那边长啥样」= `query_cards(keyword="星耀")`，
//! 不需要我们写一条正则，也不需要噪声词表。
//!
//! # 协议（OpenAI 兼容，已实测 ofm 网关 18902 支持）
//!
//! ```text
//! 请求: {model, messages, tools:[...], tool_choice:"auto"}
//! 响应: choices[0].message.tool_calls[{id, function:{name, arguments}}]
//! 然后我们执行工具 → 追加 {"role":"tool","tool_call_id":...,"content":结果}
//! → 再问一次模型 → 它用自然语言总结
//! ```
//!
//! # 安全边界（与铁则一致）
//!
//! - 工具分两类：**只读**（status/library/failures/chapters/self_check）与
//!   **写操作**（cardgen/write_chapter）。
//! - 模型调写操作时，**只入队不执行**，且锁没开时由 task_store 标 blocked。
//! - 最多 3 轮工具调用，防止模型自激循环烧额度。

use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

use super::chat_intent::ChatReply;

/// 单次对话最多允许的工具调用轮数。
///
/// 【为什么从 3 提到 4】实测「给我看看星耀」时模型跑了 4 轮：
///   第1轮 query_cards("星耀") → 只拿到标题列表
///   第2轮 query_cards("星耀城") + library_stats() → 想看主卡正文
///   第3轮 query_cards("星耀", limit=20) + query_cards("CIVI-99-009") → 再确认
///   第4轮 终于给出自然语言总结
/// 卡在 3 轮就会吐兜底文案。提到 4 轮留出余量。
///
/// 同时根治了"反复重试"的诱因：query_cards 现在直接回正文摘要
/// （见 `run_tool` 里的 briefs），模型第一轮就有料可写，正常 1~2 轮结束。
/// 4 轮是给"先查统计再查明细"这类真实多步需求留的空间，不是给自激留的。
const MAX_ROUNDS: usize = 4;

/// 工具定义。**这张表就是模型能看到的能力边界**。
///
/// 写法要点：
/// - description 要写"什么时候用它"，不只是"它做什么"——
///   模型靠这句决定调不调，比参数说明重要得多。
/// - 参数 description 里给示例（如「星耀」「太玄」），
///   模型会照着示例归一化用户口语（"星耀那边" → "星耀"）。
fn tool_specs() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": "query_cards",
                "description": "在小说资料库里查询实体（文明/势力/角色/地点/物品等）的卡片。\
                                用户问某个设定的内容、有哪些卡、长什么样、是什么样、介绍一下，都用这个。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "keyword": {
                            "type": "string",
                            "description": "要查的实体名。用最短的专名即可，如「星耀」「太玄帝国」「混沌」。不要带「的设定」「怎么样」这类词。"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "最多返回多少张卡，默认 8，最大 20。"
                        }
                    },
                    "required": ["keyword"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "library_stats",
                "description": "查资料库总体统计：总卡数、各分类数量、各文明覆盖情况。\
                                用户问「有多少张卡」「库里都有什么」时用这个。",
                "parameters": { "type": "object", "properties": {} }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "task_status",
                "description": "查任务队列状态：在跑几个、排队几个、执行器是否在线、授权锁状态、历史累计。",
                "parameters": { "type": "object", "properties": {} }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "list_failures",
                "description": "列出失败/被拒/阻塞的任务清单，含卡名与具体失败原因。\
                                用户问「哪些失败了」「跑挂了什么」时用这个。",
                "parameters": { "type": "object", "properties": {} }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "list_chapters",
                "description": "查某卷有哪些章节、某章正文是否已生成。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "volume": {
                            "type": "integer",
                            "description": "卷号，如 1 表示卷01。不传则列全部卷的概要。"
                        }
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "run_self_check",
                "description": "登记一次系统自检（管线健康度、缺卡、门禁状态）。",
                "parameters": { "type": "object", "properties": {} }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "generate_cards",
                "description": "为某文明批量生成设定卡（写操作）。用户说「建N张X的卡」「帮星耀补几张卡」时用。\
                                注意：只会把任务排进队列，真正执行由用户在面板点「执行」，不会立刻写库。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "civ": {
                            "type": "string",
                            "description": "文明名。可用简称，如「星耀」「太玄」「混沌」，系统会自动补成正名。"
                        },
                        "count": {
                            "type": "integer",
                            "description": "要生成几张卡，1~40，默认 5。"
                        },
                        "spec": {
                            "type": "string",
                            "description": "卡类分布，格式如「文明:1,势力:2,地点:2」。不确定就别填，系统会按默认比例推断。"
                        }
                    },
                    "required": ["civ"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "write_chapter",
                "description": "生成某一章的正文（写操作）。用户说「写第N章」「跑第N章正文」时用。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "volume": {
                            "type": "integer",
                            "description": "卷号，如 1。默认 1。"
                        },
                        "chapter": {
                            "type": "integer",
                            "description": "章号，如 4。"
                        }
                    },
                    "required": ["chapter"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "inspect_card",
                "description": "按卡号精确查一张卡的完整信息（正文摘要、分类、所属文明）。\
                                用户给出形如 CIVI-99-900、CHAR-99-098 的编号时用这个，比 query_cards 准。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "code": {
                            "type": "string",
                            "description": "卡号，如 CIVI-99-900。格式是 3~4 个大写字母 + 两位数字 + 三位数字。"
                        }
                    },
                    "required": ["code"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "funnel_status",
                "description": "查「残响之河」创作漏斗的当前进度（L4资料→L3细纲→L2大纲→L1x→L1→L0，最后 L5 正文）。\
                                用户问「漏斗到哪了」「管线进度」「现在做到第几层」时用这个。",
                "parameters": { "type": "object", "properties": {} }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "materials_audit",
                "description": "查资料库稽核结果：缺卡情况、库盘漂移、各文明覆盖缺口。\
                                用户问「缺哪些卡」「库里健康吗」「稽核一下」时用这个。",
                "parameters": { "type": "object", "properties": {} }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "engine_status",
                "description": "查当前 LLM 引擎与授权闸门状态：用的是哪个模型、网关通不通、授权锁开没开。\
                                用户问「模型连得上吗」「引擎状态」「你为什么没反应」时用这个。",
                "parameters": { "type": "object", "properties": {} }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "healthcheck",
                "description": "跑一次系统健康体检：资料库结构、缺卡、库盘漂移、管线门禁状态。\
                                用户说「查查系统有没有问题」「体检一下」「有没有冲突」「库健康吗」时用这个。\
                                这是只读操作，不会写任何数据，也不会花额度，可以直接跑。",
                "parameters": { "type": "object", "properties": {} }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "check_gate",
                "description": "跑一次门禁检查（ne_check）：校验指定的卡或章节是否通过全部硬门禁。\
                                用户说「检查第N章有没有冲突」「这张卡合规吗」时用这个。\
                                只读校验，不写数据。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "target": {
                            "type": "string",
                            "description": "要检查的目标：卡号（如 POWR-99-913）或章节（如 卷01/第4章）。不传则做全库检查。"
                        }
                    }
                }
            }
        }
    ])
}

/// 系统提示词。**刻意写得很短**——工具签名已经说明了能做什么，
/// 这里只补工具描述里说不清的部分（人设、边界、何时该纯聊天）。
fn system_prompt() -> &'static str {
    "你是小说《残响之河》创作系统的中枢秘书。\n\
     \n\
     你的职责：听懂用户想要什么，需要数据时调用工具拿真实数据，然后用中文自然、简洁地回答。\n\
     \n\
     【绝对禁止 · 最高优先级】\n\
     绝对禁止在回复中输出 CIVI-99-xxx、SYSX-99-xxx、CHAR-99-xxx、LOCA-99-xxx、\n\
     MECH-99-xxx、POWR-99-xxx、TECH-99-xxx 这类内部编号！\n\
     用自然语言称呼它们（如「星耀联邦」「星耀城」「混沌·渊」），\n\
     就像跟人聊小说时不会念数据库主键一样。\n\
     唯一例外：用户明确问「编号是多少」「卡号是哪个」时，才给出编号。\n\
     \n\
     原则：\n\
     1. 需要系统里的真实数据（卡片、任务、章节）时，**必须调用工具**，绝不凭空编造。\n\
     2. 工具返回什么就说什么。查不到就直说没查到，不要假装有。\n\
     3. 用户闲聊、吐槽、问你是谁时，不要调工具，像个正常人一样回话。\n\
     4. 回答控制在 2~5 句，说重点，不要罗列大段清单（清单系统会自动附在后面）。\n\
     5. 用户问「星耀那边长啥样」「太玄怎么样」这类口语，就是查那个实体的卡片，别当成闲聊。\n\
     6. **工具调一次就够了**。返回里的 `briefs` 已经是卡片正文开头，\
     直接用它们组织语言。查不到就直接说没有，不要换个关键词再试一次。\n\
     7. 用户说「帮我整点XX的卡」这类写操作，调 generate_cards 入队即可，\
     不要自己解释执行细节（系统会附上提示）。\n\
     8. 不要在回复里写「（数据来源：xxx）」「命中 N 张卡」这类系统日志，\
     系统会自动用徽章展示。你只管说人话。\n\
     \n\
     【资源安全规则 · 必读】\n\
     以下两类操作会消耗真实资源（LLM token / 磁盘写入），**入队前必须提醒用户**：\n\
       · write_chapter（写正文，单章动辄数万 token）\n\
       · generate_cards 且 count >= 10（大批量建卡）\n\
     提醒方式：在回复里明确说一句「这会真跑并消耗额度，确认已解锁吗？」\n\
     然后仍然照常调用工具入队（入队不等于执行，真跑由用户在面板点「执行」触发）。\n\
     不要因为担心就拒绝调用——用户明确要求时就入队，只是要提醒。\n\
     \n\
     以下操作**不会**消耗额度，直接调、不用提醒：\n\
       query_cards / library_stats / task_status / list_failures / list_chapters /\n\
       inspect_card / funnel_status / materials_audit / engine_status /\n\
       healthcheck / check_gate / run_self_check"
}

/// 回填对话历史：把最近若干轮 user/assistant 消息取出来，转成 LLM messages。
///
/// # 为什么必须有这个函数
///
/// 没有历史时，`messages` 每次都是 `[system, 当前这一句]`。
/// 用户说「那混沌那边呢」——模型只收到这七个字，
/// 根本不知道"那"指代什么，只能反问"您说哪边"。这不是模型笨，是没给它上下文。
///
/// # 取多少轮
///
/// `HISTORY_TURNS = 10` 轮 = 最多 20 条消息。理由：
/// - 太少（3 轮）接不住跨轮的指代和话题延续；
/// - 太多会挤占上下文预算，而工具返回本身就占了不少（briefs 约 1.4K 字）；
/// - 10 轮对应约 2~3K 字，配合 system + 工具结果，总输入仍在舒适区。
///
/// # 只取 user / assistant，跳过其余
///
/// 库里可能存着 tool 消息（若将来把工具中间态落库）。**不能把它们塞回 messages**：
/// OpenAI 协议要求 `role:"tool"` 必须紧跟对应的 `assistant.tool_calls` 且
/// `tool_call_id` 对得上，断链会直接 400。而历史里我们只存最终答复，
/// 中间的 tool_calls 没有留存，硬塞必然断链。所以这里过滤掉。
///
/// # 老数据里可能带 JSON 残渣
///
/// 早期 L2 是"让模型吐 JSON"的模式，若它没吐干净，落库的 assistant
/// content 里会混着 `{"reply": "..."} ` 这样的原文。这里做一次清洗，
/// 否则模型会学着历史里的样子也开始吐 JSON（few-shot 污染）。
fn load_history(lib_root: &Path, session: &str) -> Vec<Value> {
    /// 回填轮数上限（1 轮 = user + assistant 两条）
    const HISTORY_TURNS: usize = 10;

    let s = if session.trim().is_empty() {
        super::DEFAULT_SESSION
    } else {
        session.trim()
    };

    let store = match super::open_session(lib_root) {
        Ok(s) => s,
        Err(e) => {
            // 会话库打不开不该让聊天整个失败——没历史也能答，只是接不住指代
            eprintln!("[chat tools] 读历史失败（忽略）：{e}");
            return Vec::new();
        }
    };
    super::ensure_session(&store, s);

    let hist = match store.history(s) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("[chat tools] history() 失败（忽略）：{e}");
            return Vec::new();
        }
    };

    // history() 已是「旧→新」顺序（内部沿 parent 回溯后 reverse），无需再翻。
    // 只取尾部 N 条，避免老对话把上下文吃满。
    let keep = HISTORY_TURNS * 2;
    let start = hist.len().saturating_sub(keep);

    hist[start..]
        .iter()
        .filter(|m| m.role == "user" || m.role == "assistant")
        .filter_map(|m| {
            let content = scrub_history_content(&m.content);
            if content.trim().is_empty() {
                return None;
            }
            Some(json!({"role": m.role, "content": content}))
        })
        .collect()
}

/// 清洗历史消息里的 JSON 残渣。
///
/// 早期版本（"让模型吐 JSON"时代）落库的 assistant 内容可能是
///   `{"reply":"星耀那边有 8 张卡","intent":"library"}`
/// 这种原文。直接回填给模型会形成 few-shot 污染：
/// 它会认为"我也应该输出 JSON"，于是新一轮又开始吐 JSON。
///
/// 清洗策略：能解析出 `reply` 字段就只取 reply；否则原样返回。
fn scrub_history_content(raw: &str) -> String {
    let t = raw.trim();
    if !t.starts_with('{') {
        return t.to_string();
    }
    if let Ok(v) = serde_json::from_str::<Value>(t) {
        if let Some(r) = v.get("reply").and_then(|x| x.as_str()) {
            if !r.trim().is_empty() {
                return r.trim().to_string();
            }
        }
    }
    t.to_string()
}

/// 一次工具调用的执行结果。
struct ToolOutcome {
    /// 回填给模型的 JSON（tool 消息的 content）
    payload: Value,
    /// 顺带给前端的结构化信息（task_ids / plan 等）
    task_ids: Vec<String>,
    /// 写操作是否真的成功入队
    queued: bool,
}

/// L2 主入口：带工具调用循环的自然语言处理。
///
/// 返回 None 表示"L2 不可用"（没解锁/网关挂/超时），调用方应回落 L1 文案。
pub fn interpret_with_tools(
    conn: &Connection,
    lib_root: &Path,
    message: &str,
    session: &str,
) -> Option<Value> {
    // ── ① 授权锁 ──
    // 【注意】这把锁保护的是"真实外呼"。聊天本身也是外呼，
    // 所以没解锁就不聊——这是既有约定，不改。
    if !Path::new(super::real_llm_lock::ALLOW_FILE).exists() {
        eprintln!("[chat tools] skipped: 授权锁未打开");
        return None;
    }

    let endpoint =
        std::env::var("NOVEL_LLM_ENDPOINT").unwrap_or_else(|_| DEFAULT_ENDPOINT.to_string());
    let model = std::env::var("NOVEL_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
    let key = std::env::var("NOVEL_API_KEY").unwrap_or_else(|_| DEFAULT_API_KEY.to_string());
    let headers = vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Authorization".to_string(), format!("Bearer {key}")),
    ];

    // ── ② 组装 messages：system + 历史 + 当前句 ──
    let mut messages = vec![json!({"role": "system", "content": system_prompt()})];
    messages.extend(load_history(lib_root, session));
    messages.push(json!({"role": "user", "content": message}));

    let mut all_task_ids: Vec<String> = Vec::new();
    let mut trace: Vec<String> = Vec::new();
    let mut queued_any = false;

    for round in 0..MAX_ROUNDS {
        let body = json!({
            "model": model,
            "messages": messages,
            "tools": tool_specs(),
            "tool_choice": "auto",
            "temperature": 0.3,
            "max_tokens": 1200,
            "stream": false,
        });

        let resp = match ne_engine::http::post_json_value(
            &endpoint,
            &headers,
            &body,
            Duration::from_secs(30),
        ) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[chat tools] 网关错误（第{}轮）：{e}", round + 1);
                // 已经拿到过工具结果？那就用已有信息兜底，别整个丢掉
                if !trace.is_empty() {
                    break;
                }
                return None;
            }
        };

        let choice = resp
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first());
        let msg = match choice.and_then(|c| c.get("message")) {
            Some(m) => m,
            None => {
                eprintln!("[chat tools] 响应缺 message 字段");
                return None;
            }
        };

        let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
        let calls = msg.get("tool_calls").and_then(|t| t.as_array());

        // ── 没有工具调用 → 模型直接给了自然语言回答，收工 ──
        let calls = match calls {
            Some(c) if !c.is_empty() => c,
            _ => {
                let text = content.trim();
                if text.is_empty() {
                    // 空回复且无工具调用：异常，回落 L1
                    return None;
                }
                return Some(build_reply(
                    text,
                    &trace,
                    all_task_ids,
                    queued_any,
                ));
            }
        };

        // ── 有工具调用：先把 assistant 的 tool_calls 消息原样回填 ──
        // 【必须回填】OpenAI 协议要求对话里保留这条 assistant 消息，
        // 否则下一轮模型会认为"我没有调用过工具"，导致重复调用或报错。
        messages.push(json!({
            "role": "assistant",
            "content": content,
            "tool_calls": calls,
        }));

        for call in calls {
            let name = call
                .pointer("/function/name")
                .and_then(|x| x.as_str())
                .unwrap_or("");
            let args_raw = call
                .pointer("/function/arguments")
                .and_then(|x| x.as_str())
                .unwrap_or("{}");
            let call_id = call.get("id").and_then(|x| x.as_str()).unwrap_or("call_0");
            // 模型偶尔返回非 JSON 的 arguments（如空字符串），退化为空对象
            let args: Value = serde_json::from_str(args_raw).unwrap_or_else(|_| json!({}));

            let outcome = run_tool(conn, lib_root, name, &args);
            match outcome {
                Some(o) => {
                    all_task_ids.extend(o.task_ids.iter().cloned());
                    queued_any |= o.queued;
                    trace.push(describe_tool(name, &args, &o.payload));
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": call_id,
                        "content": o.payload.to_string(),
                    }));
                }
                None => {
                    // 不认识工具名：明确告诉模型，让它换个方式
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": call_id,
                        "content": json!({"error": format!("没有名为 {name} 的工具")}).to_string(),
                    }));
                }
            }
        }
    }

    // ── 轮数用尽：再要一次"不带工具的纯总结" ──
    //
    // 【为什么不能直接用最后一条 assistant 文本】实测那样会得到
    // 「我用工具查了一下，结果附在下面。」——这是模型在工具循环中途说的话，
    // 不是结论。用户读到的是半句话，等于没回答。
    //
    // 正确做法：把 `tools` 去掉再问一次，明确要求"基于已有工具结果直接回答"。
    // 这一次没有工具可选，模型只能组织语言，拿到的才是真结论。
    if trace.is_empty() {
        return None;
    }
    let text = final_summarize(&endpoint, &headers, &model, &messages)
        .unwrap_or_else(|| "工具查完了，但模型这次没能把结果组织成回复。具体数据见下方步骤。".to_string());
    Some(build_reply(&text, &trace, all_task_ids, queued_any))
}

/// 工具循环用尽后，再请求一次**不带工具**的纯文本总结。
///
/// 返回 None 表示这次补救也失败了（网关挂/超时/仍然空回复），
/// 调用方用一句诚实的话兜底，绝不把中途残句当结论。
fn final_summarize(
    endpoint: &str,
    headers: &[(String, String)],
    model: &str,
    history: &[Value],
) -> Option<String> {
    let mut msgs: Vec<Value> = history.to_vec();
    // 追加一条明确的收尾要求。放 user 而不是 system，是因为
    // 多数网关对"结论轮"的 system 消息处理不一致，user 更稳。
    msgs.push(json!({
        "role": "user",
        "content": "别再调用工具了。基于上面已经拿到的工具结果，用中文直接回答我最初的问题，2~5 句，说重点。",
    }));

    let body = json!({
        "model": model,
        "messages": msgs,
        // 刻意不传 tools / tool_choice：没有工具可选，模型只能给文字
        "temperature": 0.3,
        "max_tokens": 1200,
        "stream": false,
    });

    let resp = match ne_engine::http::post_json_value(
        endpoint,
        headers,
        &body,
        Duration::from_secs(30),
    ) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[chat tools] 收尾总结失败：{e}");
            return None;
        }
    };
    let text = resp
        .pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if text.is_empty() {
        return None;
    }
    Some(text)
}

/// 组装最终给前端的 JSON。
fn build_reply(
    text: &str,
    trace: &[String],
    task_ids: Vec<String>,
    queued: bool,
) -> Value {
    let mut r = ChatReply::chat(strip_internal_codes(text), "l2_tools");
    // plan 渲染成前端的步骤卡：让用户看到"系统真的去查了"，而不是凭空说话
    r.plan = trace.iter().map(|t| json!({ "action": t })).collect();
    r.task_ids = task_ids;
    if queued {
        r.hint = Some("任务已入队；真跑请先在锁条解锁，再点悬浮窗的「执行」。".into());
    }
    r.to_json()
}

/// 剔除回复里裸露的内部编号。
///
/// # 为什么提示词之外还要一道代码兜底
///
/// 提示词里已经写了「绝对禁止输出 CIVI-99-xxx」。但那是**建议**，
/// 不是强制——模型有概率不遵守，尤其在工具返回里带了编号时
/// （比如 `briefs` 的 code 字段），它会顺手抄进句子里。
///
/// 实测就是这种情况：`briefs` 里每张卡都带 `code`，
/// 模型看到后觉得"这个信息挺好"，就写成「星耀城（CIVI-99-009）」。
/// 光改提示词压不住，因为它有充分的机会看到编号。
///
/// 所以这里做确定性的后处理：把形如 `XXX-99-999` 的编号从正文里删掉，
/// 顺便清理因此产生的中文标点冗余。**这是最后一道闸，不依赖模型自觉**。
///
/// # 例外
///
/// 用户明确问编号时，模型会自己加「编号是」这类表述。为了不误伤，
/// 只在**括号包裹**或**紧跟实体名**的常见形态下清理；如果整句都是编号
/// （比如用户问"CIVI-99-900 是什么"），就不动——那正是用户要的。
fn strip_internal_codes(text: &str) -> String {
    // 快路径：没有编号形态就直接返回，零成本
    if !text.contains("-99-") && !text.contains("-90-") {
        return text.to_string();
    }

    // 若整段几乎都是编号（用户明确在问编号），原样返回
    let code_like = text.matches("-99-").count() + text.matches("-90-").count();
    if code_like > 0 && text.chars().count() < 60 && code_like >= 2 {
        return text.to_string();
    }

    // ① 先去掉「（CIVI-99-009）」这种括号内的编号（含中文全角括号）
    //    这类是最常见的形态：「星耀城（CIVI-99-009）」
    let bracket_re = regex_lite_bracket();
    let mut out = bracket_re.replace_all(text, "").to_string();

    // ② 再去掉句子里裸露的编号本身（可能带前后空格）
    out = code_re().replace_all(&out, "$1$2").to_string();

    // ③ 清理因此产生的冗余：「星耀城 」→「星耀城」，连续空格压成一个
    out = out.replace("  ", " ");
    // 中文里编号删掉后可能留下「， ，」或「、 」这类，顺手收一下
    out = out.replace("， ，", "，").replace("、 、", "、");
    out = out.replace("（）", "").replace("()", "");
    out = out.trim().to_string();

    // ④ 极端情况：全被清空了 → 回退原文（宁可留着编号，也别给用户空白）
    if out.is_empty() {
        return text.to_string();
    }
    out
}

/// 匹配「（CIVI-99-009）」这类整块括号（含中文括号）。
///
/// 只匹配括号内**只有编号**或「编号 + 少量说明」的情况，
/// 避免误伤「（这一张 6623 字）」这种正常括号。
fn regex_lite_bracket() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"[（(]\s*[A-Z]{3,4}-\d{2}-\d{3}\s*[）)]").unwrap()
    })
}

/// 匹配裸露的编号本体，如 `CIVI-99-009`。
///
/// # 为什么不能用 `\b`
///
/// `\b` 是「单词边界」，而 Rust 的 `regex` 在 Unicode 模式下把**中文字符
/// 也算作 `\w`**。所以「星耀城CIVI-99-009」这种紧贴中文的写法不算边界，
/// 会漏掉——而模型恰好经常这么写。
///
/// 改用显式的「前面不是字母数字」断言，只排除真正的英文/数字紧贴：
/// 中文、标点、括号都允许，正好覆盖需要清理的场景。
///
/// # 为什么用捕获组 `$1` 而不是直接删
///
/// 断言部分 `(?:^|[^A-Za-z0-9-])` 会**吃掉前一个字符**。若直接替换成空串，
/// 「星耀城（CIVI-99-009）」会变成「星耀城」但左括号被吞、右括号留下，
/// 得到「星耀城）」——比不改还难看。
///
/// 所以前置断言放进**捕获组**，替换时用 `$1` 把它吐回来，
/// 只删编号本身。后置断言用非捕获组（不需要保留右括号，
/// 但保留也无妨——`[^A-Za-z0-9-]` 匹配到的本来就是原文的字符）。
fn code_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"(^|[^A-Za-z0-9-])[A-Z]{3,4}-\d{2}-\d{3}([^A-Za-z0-9-]|$)").unwrap()
    })
}

/// 把一次工具调用压成人话，给前端步骤卡用。
fn describe_tool(name: &str, args: &Value, payload: &Value) -> String {
    match name {
        "query_cards" => {
            let kw = args.get("keyword").and_then(|x| x.as_str()).unwrap_or("");
            let n = payload
                .get("count")
                .and_then(|x| x.as_i64())
                .unwrap_or(0);
            format!("查资料库「{kw}」→ 命中 {n} 张卡")
        }
        "library_stats" => {
            let n = payload.get("total").and_then(|x| x.as_i64()).unwrap_or(0);
            format!("查资料库统计 → 共 {n} 张卡")
        }
        "task_status" => "查任务队列状态".to_string(),
        "list_failures" => {
            let n = payload.get("count").and_then(|x| x.as_i64()).unwrap_or(0);
            format!("查失败任务 → {n} 条")
        }
        "list_chapters" => "查章节列表".to_string(),
        "run_self_check" => "登记系统自检".to_string(),
        "generate_cards" => {
            let civ = args.get("civ").and_then(|x| x.as_str()).unwrap_or("");
            let n = args.get("count").and_then(|x| x.as_i64()).unwrap_or(5);
            format!("登记建卡任务：{civ} {n} 张")
        }
        "write_chapter" => {
            let ch = args.get("chapter").and_then(|x| x.as_i64()).unwrap_or(1);
            format!("登记写正文任务：第 {ch} 章")
        }
        "inspect_card" => {
            let code = args.get("code").and_then(|x| x.as_str()).unwrap_or("");
            if payload.get("error").is_some() {
                format!("查卡号 {code} → 失败")
            } else {
                format!("按卡号查 {code}")
            }
        }
        "funnel_status" => "查创作漏斗进度".to_string(),
        "materials_audit" => "查资料库稽核".to_string(),
        "engine_status" => "查引擎与授权状态".to_string(),
        "healthcheck" => "跑系统体检".to_string(),
        "check_gate" => {
            let t = args.get("target").and_then(|x| x.as_str()).unwrap_or("");
            if t.is_empty() {
                "跑门禁检查".to_string()
            } else {
                format!("门禁检查 {t}")
            }
        }
        _ => format!("调用工具 {name}"),
    }
}

/// 执行一个工具。
///
/// 返回 None 表示工具名不认识；返回 Some 时 `payload` 会原样回填给模型，
/// 所以 **payload 必须是模型能读懂的紧凑 JSON**，不要塞几万字正文。
fn run_tool(
    conn: &Connection,
    lib_root: &Path,
    name: &str,
    args: &Value,
) -> Option<ToolOutcome> {
    let no_ids = || ToolOutcome {
        payload: Value::Null,
        task_ids: Vec::new(),
        queued: false,
    };

    match name {
        "query_cards" => {
            let kw = args
                .get("keyword")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .trim();
            let limit = args
                .get("limit")
                .and_then(|x| x.as_u64())
                .unwrap_or(8)
                .min(20) as usize;
            if kw.is_empty() {
                return Some(ToolOutcome {
                    payload: json!({"error": "keyword 不能为空"}),
                    ..no_ids()
                });
            }
            // 直接复用 L2 已有的检索（带逐级回退）：
            // 模型给的关键词通常已经干净，但偶尔会给「星耀联邦的文明设定」
            // 这种长料，回退能把它救回来。
            // **不再需要 keyword 提取**——模型给的就是专名。
            let hits = super::chat_llm::search_cards_pub(conn, kw, limit);
            let items: Vec<Value> = hits
                .iter()
                .map(|(code, name, cat, chars)| {
                    json!({"code": code, "name": name, "category": cat, "chars": chars})
                })
                .collect();

            // ── 关键：把正文摘要一并给模型 ──
            //
            // 【为什么必须有】实测踩坑：只返回 code/name/category/chars 时，
            // 模型看到的是"8 张卡的标题列表"，没有任何实质内容。
            // 它的反应是**反复重试**——第 1 轮查「星耀」，第 2 轮改查「星耀城」，
            // 第 3 轮再加 limit=20 再查一遍，以为是自己参数没给对。
            // 结果 3 轮 MAX_ROUNDS 用完还没拿到正文，只能吐"结果附在下面"。
            //
            // 给了摘要之后，模型第一轮就能回答"星耀联邦是科技双轨制文明，
            // 主卡是星耀城…"，不需要第二轮。这同时省了额度和 3~5 秒延迟。
            //
            // 【截断策略】每张卡取前 180 字，最多 8 张 → 上限约 1440 字。
            // 够模型判断"这张卡讲什么"，又不至于把上下文撑爆。
            let mut briefs: Vec<Value> = Vec::new();
            for (code, name, _cat, _chars) in hits.iter().take(8) {
                let body: String = conn
                    .query_row(
                        "SELECT COALESCE(content,'') FROM materials WHERE code=?1",
                        rusqlite::params![code],
                        |r| r.get::<_, String>(0),
                    )
                    .unwrap_or_default();
                let brief: String = body
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(180)
                    .collect();
                briefs.push(json!({
                    "code": code, "name": name, "brief": brief,
                }));
            }

            Some(ToolOutcome {
                payload: json!({
                    "keyword": kw,
                    "count": items.len(),
                    "cards": items,
                    // 主体内容单独给一份，模型据此组织语言，不必再查第二轮
                    "briefs": briefs,
                    "note": if items.is_empty() {
                        "库里没有匹配的卡。可以直接告诉用户没有，不要换关键词重试。"
                    } else {
                        "以上 briefs 就是卡片正文开头，够回答「长什么样」了。\
                         不要为了拿全文而重复调用本工具。"
                    },
                }),
                ..no_ids()
            })
        }
        "library_stats" => {
            let stats = super::chat_intent::library_stats_pub(conn);
            Some(ToolOutcome {
                payload: stats,
                ..no_ids()
            })
        }
        "task_status" => {
            let v = super::chat_intent::status_pub(conn, lib_root);
            Some(ToolOutcome {
                payload: v,
                ..no_ids()
            })
        }
        "list_failures" => {
            let v = super::chat_intent::failures_pub(conn);
            Some(ToolOutcome {
                payload: v,
                ..no_ids()
            })
        }
        "list_chapters" => {
            let vol = args.get("volume").and_then(|x| x.as_i64());
            let v = super::chat_intent::chapters_pub(conn, vol);
            Some(ToolOutcome {
                payload: v,
                ..no_ids()
            })
        }
        "run_self_check" => {
            let v = super::chat_intent::self_check_pub(conn);
            Some(ToolOutcome {
                payload: v,
                ..no_ids()
            })
        }
        "generate_cards" => {
            let civ_raw = args
                .get("civ")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let count = args
                .get("count")
                .and_then(|x| x.as_u64())
                .unwrap_or(5)
                .clamp(1, 40) as u32;
            let civ = super::chat_llm::resolve_civ_pub(&civ_raw).or_else(|| {
                if civ_raw.is_empty() {
                    None
                } else {
                    Some(civ_raw.clone())
                }
            });
            let civ = match civ {
                Some(c) => c,
                None => {
                    return Some(ToolOutcome {
                        payload: json!({"error": "没能识别出文明名，请让用户明确是哪个文明"}),
                        ..no_ids()
                    })
                }
            };
            let spec = args
                .get("spec")
                .and_then(|x| x.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| super::chat_intent::infer_spec_pub("", count));
            let payload = json!({
                "civ": civ, "spec": spec, "real": false, "count": count,
            });
            match super::task_store::submit(
                conn,
                &json!({"kind": "cardgen", "payload": payload}).to_string(),
            ) {
                Ok(v) => {
                    let id = v["id"].as_str().unwrap_or("").to_string();
                    let ok = v["ok"].as_bool().unwrap_or(false);
                    Some(ToolOutcome {
                        payload: json!({
                            "queued": ok,
                            "task_id": id,
                            "civ": civ,
                            "count": count,
                            "spec": spec,
                            "mode": "预演（不写卡、不花额度）",
                            "error": v["error"].as_str().unwrap_or(""),
                        }),
                        task_ids: if ok && !id.is_empty() { vec![id] } else { Vec::new() },
                        queued: ok,
                    })
                }
                Err(e) => Some(ToolOutcome {
                    payload: json!({"queued": false, "error": format!("登记失败：{e}")}),
                    ..no_ids()
                }),
            }
        }
        "write_chapter" => {
            let volume = args
                .get("volume")
                .and_then(|x| x.as_i64())
                .unwrap_or(1);
            let chapter = args
                .get("chapter")
                .and_then(|x| x.as_i64())
                .unwrap_or(1);
            // ── 安全边界：real 一律强制 false ──
            //
            // 【为什么不信模型给的 real】`write_chapter` 的 spec 里虽然有
            // real 参数，但真跑（消耗额度 + 写文件）必须由人显式决定。
            // 模型看到用户说「真跑」就可能填 true，而它没有判断授权的权力。
            // 所以这里**无条件覆盖为 false**：入队的一律是预演，
            // 真跑只能由人在面板上点，并且还要过 `/sdcard/wogov_real_llm.allow`。
            // 这是"模型只能登记意图，不能决定执行"的具体落法。
            let real = false;
            let vol_str = format!("卷{:02}", volume);
            let payload = json!({
                "volume": vol_str, "chapter": chapter, "real": real,
            });
            match super::task_store::submit(
                conn,
                &json!({"kind": "chapter_gen", "payload": payload}).to_string(),
            ) {
                Ok(v) => {
                    let id = v["id"].as_str().unwrap_or("").to_string();
                    let ok = v["ok"].as_bool().unwrap_or(false);
                    Some(ToolOutcome {
                        payload: json!({
                            "queued": ok,
                            "task_id": id,
                            "volume": vol_str,
                            "chapter": chapter,
                            "real": real,
                        }),
                        task_ids: if ok && !id.is_empty() { vec![id] } else { Vec::new() },
                        queued: ok,
                    })
                }
                Err(e) => Some(ToolOutcome {
                    payload: json!({"queued": false, "error": format!("登记失败：{e}")}),
                    ..no_ids()
                }),
            }
        }
        // ── 以下 4 个走 ne-mcp 桥（真实引擎数据，不落库）──
        //
        // 【为什么必须补这 4 个】如果把路由切成"L2 优先"，而工具表里没有
        // 这几项，那么原先 L1 的 `try_data_query` 能回答的「漏斗到哪了」
        // 「缺哪些卡」「模型连得上吗」就会全部失效——模型无工具可调，
        // 只能编或说"我查不了"，比改造前更差。这是路由改造的前置条件。
        //
        // 【坑】ne-mcp 可能没编译/资料库路径不对，失败时**必须如实回报**，
        // 不能让模型以为查到了。所以 error 字段写清原因，由模型转述。
        name if matches!(
            name,
            "inspect_card" | "funnel_status" | "materials_audit" | "engine_status"
        ) =>
        {
            let (tool, targs) = match name {
                "inspect_card" => {
                    let code = args
                        .get("code")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_uppercase();
                    if code.is_empty() {
                        return Some(ToolOutcome {
                            payload: json!({"error": "code 不能为空，需要形如 CIVI-99-900 的卡号"}),
                            ..no_ids()
                        });
                    }
                    ("wg_inspect_card", json!({ "key": code }))
                }
                "funnel_status" => ("funnel", json!({})),
                "materials_audit" => ("ne_materials", json!({})),
                "healthcheck" => ("wg_self_check", json!({})),
                "check_gate" => {
                    let target = args
                        .get("target")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    if target.is_empty() {
                        ("ne_check", json!({}))
                    } else if code_re().is_match(&target) {
                        // 看着像卡号 → 走单卡检视（ne_check 的入参是路径，不是卡号）
                        ("wg_inspect_card", json!({ "key": target.to_uppercase() }))
                    } else {
                        ("ne_check", json!({ "target": target }))
                    }
                }
                _ => ("wg_llm_status", json!({ "action": "gate" })),
            };
            match super::chat_tools::ChatTools::connect(lib_root) {
                Ok(mut t) => match t.call(tool, targs) {
                    Ok(raw) => {
                        // summarize 给人话摘要；同时把原文截断后一并给模型，
                        // 让它既能引用具体数字，又不至于被几万字符撑爆上下文。
                        let summary = super::chat_tools::summarize(tool, &raw);
                        let raw_short: String = raw.chars().take(3000).collect();
                        Some(ToolOutcome {
                            payload: json!({
                                "tool": tool,
                                "summary": summary,
                                "raw_truncated": raw_short,
                            }),
                            ..no_ids()
                        })
                    }
                    Err(e) => Some(ToolOutcome {
                        payload: json!({
                            "error": format!("工具 {tool} 调用失败：{e}"),
                            "hint": "ne-mcp 可能未编译或资料库路径不可读",
                        }),
                        ..no_ids()
                    }),
                },
                Err(e) => Some(ToolOutcome {
                    payload: json!({
                        "error": format!("连不上引擎：{e}"),
                        "hint": "确认 target/release/ne-mcp 存在，且资料库路径可读",
                    }),
                    ..no_ids()
                }),
            }
        }
        _ => None,
    }
}

const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:18902/v1/chat/completions";
const DEFAULT_MODEL: &str = "deepseek-v4.1-flash-free";
const DEFAULT_API_KEY: &str = "sk-local-free";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_specs_cover_core_capabilities() {
        let specs = tool_specs();
        let arr = specs.as_array().unwrap();
        let names: Vec<&str> = arr
            .iter()
            .filter_map(|t| t.pointer("/function/name").and_then(|x| x.as_str()))
            .collect();
        for need in [
            "query_cards",
            "library_stats",
            "task_status",
            "list_failures",
            "generate_cards",
            "write_chapter",
        ] {
            assert!(names.contains(&need), "缺少工具 {need}");
        }
    }

    /// 每个工具都必须有 description，否则模型不知道何时该调它。
    #[test]
    fn every_tool_has_description() {
        let specs = tool_specs();
        for t in specs.as_array().unwrap() {
            let d = t.pointer("/function/description").and_then(|x| x.as_str());
            assert!(d.map_or(false, |s| !s.is_empty()), "工具缺描述：{t}");
        }
    }

    /// describe_tool 不能 panic，且要给出可读文案。
    #[test]
    fn describe_tool_is_readable() {
        let d = describe_tool(
            "query_cards",
            &json!({"keyword": "星耀"}),
            &json!({"count": 12}),
        );
        assert!(d.contains("星耀"));
        assert!(d.contains("12"));
    }

    /// 回归：轮数上限必须够模型跑完"查库→组织语言"。
    ///
    /// 【历史事故】原值是 3，实测「给我看看星耀」时模型跑了 4 轮
    /// （查标题列表 → 查主卡 → 再确认 → 才总结），卡在 3 轮就吐兜底文案。
    /// 这条测试锁住"至少 4 轮"，防止有人觉得"3 轮够了"又调回去。
    #[test]
    fn max_rounds_allows_real_lookup_flow() {
        assert!(
            MAX_ROUNDS >= 4,
            "MAX_ROUNDS={MAX_ROUNDS} 太小：实测「给我看看星耀」需要 4 轮才能给出结论"
        );
        // 但也不能无限放大，否则模型自激时会烧掉大量额度
        assert!(MAX_ROUNDS <= 8, "MAX_ROUNDS={MAX_ROUNDS} 过大，自激时会烧额度");
    }

    /// 回归：系统提示词必须明确"工具调一次就够"。
    ///
    /// 【历史事故】模型看不到卡片正文（只拿到标题列表）时会反复重试，
    /// 换关键词、加 limit，一路把轮数用尽。除了在工具返回里补 briefs，
    /// 提示词这层也要明确告知，双保险。
    #[test]
    fn system_prompt_discourages_retry_loop() {
        let p = system_prompt();
        assert!(p.contains("调一次就够"), "缺少「工具调一次就够」的约束");
        assert!(p.contains("briefs"), "缺少对 briefs 字段的说明");
    }

    /// 回归：写操作的工具签名里不能暴露 `real` 参数。
    ///
    /// 【为什么】`real=true` 会真跑（写文件 + 消耗额度）。
    /// 如果工具签名里有这个参数，模型看到用户说「真跑」就可能自己填 true，
    /// 而它没有判断授权的权力。正确做法是把这个能力**从工具面上移除**，
    /// 只留给人从面板触发。
    #[test]
    fn write_chapter_spec_has_no_real_param() {
        let specs = tool_specs();
        for t in specs.as_array().unwrap() {
            if t.pointer("/function/name").and_then(|x| x.as_str()) == Some("write_chapter") {
                let props = t.pointer("/function/parameters/properties");
                assert!(
                    props.and_then(|p| p.get("real")).is_none(),
                    "write_chapter 不应暴露 real 参数（真跑必须由人从面板触发）"
                );
                return;
            }
        }
        panic!("找不到 write_chapter 工具定义");
    }

    /// 回归：ne-mcp 桥接工具必须都在工具表里。
    ///
    /// 【为什么】这些工具承接的是原 L1 `try_data_query` 的能力。
    /// 路由改成"L2 优先"后，如果工具表里漏了某一项，模型就无工具可调，
    /// 只能回复"我查不了"——比改造前更差。
    #[test]
    fn nmcp_bridge_tools_are_registered() {
        let specs = tool_specs();
        let names: Vec<&str> = specs
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t.pointer("/function/name").and_then(|x| x.as_str()))
            .collect();
        for need in [
            "inspect_card",
            "funnel_status",
            "materials_audit",
            "engine_status",
        ] {
            assert!(names.contains(&need), "缺少 ne-mcp 桥接工具 {need}");
        }
    }

    /// 回归：历史消息里的 JSON 残渣必须被清洗掉。
    ///
    /// 【为什么重要】早期 L2 是"让模型吐 JSON"模式，若它没吐干净，
    /// 落库的 assistant 内容就是 `{"reply":"...","intent":"..."}` 原文。
    /// 直接回填会形成 few-shot 污染——模型看到历史里是 JSON，
    /// 新一轮也会跟着吐 JSON，用户就会在聊天框里看到裸 JSON。
    #[test]
    fn scrub_history_extracts_reply_field() {
        let raw = r#"{"reply":"星耀那边有 8 张卡","intent":"library","confidence":0.9}"#;
        assert_eq!(scrub_history_content(raw), "星耀那边有 8 张卡");
    }

    /// 残留：不是 JSON 的普通文本要原样返回，不能被吞。
    #[test]
    fn scrub_history_keeps_plain_text() {
        let plain = "星耀联邦是科技修真融合的文明。";
        assert_eq!(scrub_history_content(plain), plain);
        // 以 { 开头但不是合法 JSON 的，也要原样保留（宁可留着也别丢内容）
        let broken = "{这不是 JSON";
        assert_eq!(scrub_history_content(broken), broken);
    }

    /// 回归：没有 reply 字段的 JSON 不能被清成空字符串。
    ///
    /// 【为什么】宁可把整段 JSON 留着（虽然难看），也不能返回空——
    /// 空内容会被 `load_history` 过滤掉，那条历史等于凭空消失，
    /// 用户会觉得"系统把我说过的话忘了"。
    #[test]
    fn scrub_history_does_not_swallow_json_without_reply() {
        let raw = r#"{"intent":"library","count":8}"#;
        let out = scrub_history_content(raw);
        assert!(!out.trim().is_empty(), "不能清成空字符串");
        assert!(out.contains("library"), "内容应保留");
    }

    /// 回归：默认会话 id 必须是稳定的字面量。
    ///
    /// 【为什么】`DEFAULT_SESSION` 是前端不传 session 时的落点，
    /// 也写进了历史库。若哪天改成随机值，用户一刷新就"历史没了"。
    #[test]
    fn default_session_is_stable() {
        assert_eq!(crate::governance::DEFAULT_SESSION, "chat");
    }

    /// 回归：session.db 路径必须在 work-root 下的 08_运行态，不能散落。
    ///
    /// 【为什么锁】三个候选位置各有坑：
    ///   - repo 内 .neagent/agent.db → 冷备份扫不到，git clean 就没了
    ///   - 搬进 novel.db → 污染资料库 86 张业务表的 schema
    ///   - work-root/session.db → 被稽核/inventory 当脏文件
    /// 08_运行态 是明确区分的运行态目录，这条测试防止有人改回去。
    #[test]
    fn session_db_lives_in_runtime_dir() {
        let p = crate::governance::session_db_path(std::path::Path::new("/tmp/lib"));
        assert!(
            p.ends_with("08_运行态/session.db"),
            "session.db 应在 08_运行态 下，实际：{}",
            p.display()
        );
    }

    /// 回归：回复里不能出现内部编号（最常见形态：括号包裹）。
    ///
    /// 【历史事故】用户看到的是「星耀城（CIVI-99-009）——星耀联邦首都」，
    /// 直接念数据库主键，像个智障机器人。用户明确要求"用自然语言称呼它们"。
    #[test]
    fn strip_internal_codes_removes_bracketed_code() {
        let input = "星耀城（CIVI-99-009）是三层人造行星都市。";
        let out = strip_internal_codes(input);
        assert!(!out.contains("CIVI-99-009"), "编号还在：{out}");
        assert!(out.contains("星耀城"), "实体名被误删：{out}");
        assert!(out.contains("三层人造行星都市"), "正文被误删：{out}");
        // 不能留下孤零零的右括号
        assert!(!out.contains("）"), "括号残留：{out}");
    }

    /// 回归：紧贴中文的裸露编号也要清掉（不能用 `\b`）。
    #[test]
    fn strip_internal_codes_removes_bare_code_next_to_chinese() {
        let input = "核心是SYSX-99-005星耀联邦，科技修真融合。";
        let out = strip_internal_codes(input);
        assert!(!out.contains("SYSX-99-005"), "编号还在：{out}");
        assert!(out.contains("星耀联邦"), "实体名被误删：{out}");
    }

    /// 不能让清洗把整段话吃掉。
    #[test]
    fn strip_internal_codes_keeps_plain_text() {
        let input = "星耀联邦是科技修真融合的星际联邦。";
        assert_eq!(strip_internal_codes(input), input);
    }

    /// 用户明确问编号时，不能把编号删掉（那是他要的答案）。
    ///
    /// 【判据】整段很短且编号密集 → 视为"在问编号"，原样返回。
    #[test]
    fn strip_internal_codes_keeps_code_when_user_asked() {
        let input = "CIVI-99-009 和 SYSX-99-005";
        let out = strip_internal_codes(input);
        assert!(
            out.contains("CIVI-99-009"),
            "用户明确问编号时不该删：{out}"
        );
    }

    /// 回归：系统提示词必须含「禁止输出编号」的死命令。
    ///
    /// 【为什么锁】这条是产品级要求：聊天框是跟人说话，
    /// 不是查数据库。提示词被改动时这条测试会立刻红。
    #[test]
    fn system_prompt_forbids_internal_codes() {
        let p = system_prompt();
        assert!(p.contains("绝对禁止"), "缺少禁止编号的死命令");
        assert!(p.contains("CIVI-99-xxx"), "缺少具体编号示例");
        assert!(
            p.contains("自然语言称呼") || p.contains("用自然语言"),
            "缺少「用自然语言称呼」的要求"
        );
    }
}
