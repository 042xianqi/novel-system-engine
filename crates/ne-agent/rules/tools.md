# 工具规则（tools）

按需加载的规则块：**工具总线的使用约定**。默认不常驻，模型判断相关时用
`load_rule(rule="tools")` 拉取。

## 调用格式

工具调用走文本协议，模型输出：

```text
<tool_call>{"name":"read","args":{"path":"crates/ne-agent/src/lib.rs"}}</tool_call>
```

一条消息里可放多个 `<tool_call>` 块；`args` 是字符串到任意值的对象。

## 工具清单

| 工具 | 用途 | 备注 |
| --- | --- | --- |
| `read` | 读文件 | 相对工作区路径 |
| `write` | 写/覆盖文件 | 最小改动，勿整文件重写 |
| `edit` | 精确文本替换 | `old` 必须唯一匹配 |
| `ls` | 列目录 | — |
| `base` | 执行 shell | 危险命令会触发审批 |
| `plan` | 多步骤计划 | 会话局部，持久化 |
| `scratchpad` | 工作草稿 | 会话局部，持久化 |
| `ask_user` | 向用户提结构化问题 | 关键决策点用，别滥用 |
| `load_rule` | 按需加载规则块 | 见下 |
| `load_skill` | 按需加载技能 | 见下 |
| `web_fetch` / `web_search` | 联网 | 默认关闭，需显式启用 |
| `mcp__<服务>__<工具>` | 外部 MCP | 首个调用惰性连接 |

## 按需加载

规则块与技能的**目录常驻在 system prompt**（只有名字+一句话），完整正文用
`load_rule(rule="…")` / `load_skill(name="…")` 主动拉取——避免正文常驻撑爆上下文。
`load_skill` 还可带 `path` 读技能目录内的子资源（如 `references/spec.md`）。

## 失败处理

工具失败会以 `[ERROR] …` 回注给你，附原因。**据此调整参数或换工具，不要重复完全
相同的调用**——连续多轮全失败会触发熔断停止。
