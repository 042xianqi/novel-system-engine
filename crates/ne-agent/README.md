# ne-agent —— 《残响之河》单二进制智能体

把原本松散的 9 个 `ne-*` 脚本收成**一个二进制**：vault 隔离 + 自有模型配置 + 会话 + 记忆 + 工具总线 + 任务。
（用户核心诉求原话：「感觉项目就是松散的脚本，根本不是一个整体」。）

## 是什么

```
ne-agent 一个可执行文件
├── vault         项目隔离库（.neagent/：配置、会话库、记忆库、工作区）
├── 模型配置       自有 models.yml / config.yml / runtime.yaml（复用 ne-engine 解析）
├── 会话           会话**树**（parent_id）+ 分支 rewind
├── 记忆           pinned + BM25 召回
├── 工具总线        17 个：read/write/edit/ls/base + web_fetch/web_search + MCP(`mcp__*`)
│                  + plan/scratchpad + ask_user + load_rule/load_skill
│                  + subagents/dual_agent + compress/history_read/history_search
├── 任务           持久化状态机（8 状态 + CAS）+ 子智能体 Lane
└── 审批           三档模式 + 正交的只读规划门禁
```

底层能力（章节目录、门禁、注入、质检、漏斗）**直接复用既有 `ne-*` crate**，不重写。

## 快速开始

```bash
cargo build --release -p ne-agent        # 产物 target/release/ne-agent（约 6 MB）

# 建项目库
./target/release/ne-agent --vault ~/my-novel vault init

# 配模型（指向任意 OpenAI 兼容端点）
#   ~/my-novel/05_管线框架/data/models.yml
#   ~/my-novel/05_管线框架/data/config.yml

# 对话
./target/release/ne-agent --vault ~/my-novel chat

# 写一章（复用 ne-engine 的注入+预算+门禁全链）
./target/release/ne-agent --vault ~/my-novel novel --chap-id 1

# 起 MCP 服务（供其它 MCP 客户端调用）
./target/release/ne-agent --vault ~/my-novel mcp serve
```

## 设计立场（为什么要这样做）

| 决策 | 理由 |
|---|---|
| **enum 工具 + match 分派**（非 trait 注册表） | 学太墟 `enum HarnessTool`：工具可穷举、可静态检查、无动态注册 |
| **std 阻塞 IO，零 tokio / 零 reqwest** | 单二进制要轻、可拷走；HTTPS 用 rustls + `std::net` 自写（`web.rs`） |
| **抄契约不抄体量** | 主参考太墟 harness（Kotlin，47535 行）只取**契约**（状态机/审批/上下文策略），不搬 PRoot/PTY/Shizuku 等与写小说无关的设施 |
| **CAS 而非乐观更新** | 任务状态转移用 `UPDATE ... WHERE status IN (...)`；防进程恢复后**两个执行者都报"成功"** |
| **plan/scratchpad 持久化** | 长任务跨多轮，历史会被上下文折叠压掉 → 状态外置到库，不受折叠影响 |
| **顺序 Lane 而非并发** | 写小说是长链顺序任务；并发无收益且会让"同一章被两个 Lane 写"。写租约只做**冲突预检** |

## 里程碑（已完成）

| 里程碑 | 内容 |
|---|---|
| M0 | vault / config / models 地基 |
| M1 | 会话树 + prompt 组装 + 工具总线 + 多轮循环 + chat |
| M2 | 记忆（pinned + BM25 召回） |
| M3 | novel 任务：补齐全仓缺失的 `BudgetCtx`/`ChapterCtx` 真实现 |
| M4 | 外部世界：自写 HTTPS + 多引擎搜索 + MCP 客户端 |
| M5 | 长大：上下文预算（CJK token 估算 + 老轮次截断 + 折叠线）+ 会话树 rewind |
| M6 | 安全协作：审批三档 + 任务状态机（CAS）+ plan/scratchpad |
| M7 | 完善：架构门禁 + 子智能体 Lane + 单二进制打包 |
| M8 | 补齐旧待办：ask_user + load_rule/load_skill + 顺序 dual agent + Android 交叉编译 |
| M9 | 上下文可设置 + 压缩（自动/命令/手动）+ 投影（为 UI 铺垫） |

详细路线图见 [`docs/ROADMAP-ne-agent.md`](docs/ROADMAP-ne-agent.md)。

## 架构门禁

```bash
python3 scripts/arch_check.py                # crate 依赖白名单 + import 黑名单 + 行数棘轮
python3 scripts/arch_check.py --sync-baseline
```

- **crate 依赖白名单**：每个 crate 能依赖谁写在 `architecture-policy.json`
- **import 黑名单**：如 `ne-agent` 禁 `tokio`/`reqwest`（架构决策落到门禁，不靠自觉）
- **文件行数棘轮**：上限 700；存量超限登记基线，**只许下调不许上涨**
- CI（`.github/workflows/ci.yml`）在编译前先跑门禁

## 参考来源

- **主参考**：太墟 harness（`wkbin/taixu`）—— 契约对照见 [`docs/ref_taixu_harness_notes.md`](docs/ref_taixu_harness_notes.md)
- **补充**：`oh-my-pi`（主循环停止语义、单二进制打包范式）—— [`docs/ref_omp_notes.md`](docs/ref_omp_notes.md)
