# ne-agent 开发进度（跨段续跑用）

> 最后更新：2026-10-11（M3 进行中）
> 仓库：github.com/042xianqi/novel-system　本地：/workspace/小说系统
> 路线图：docs/ROADMAP-ne-agent.md　参考笔记：docs/ref_taixu_harness_notes.md、docs/ref_omp_notes.md
> 太墟源码：/workspace/taixu-ref（Kotlin，47535 行，**主参考**）

## 铁律（不要违反）

1. **现有 9 个 `ne-*` crate 一行不改**，只被 `ne-agent` 调用。
2. **遇到问题/bug 先查太墟**怎么实现（用户 2026-10-11 明确要求），不要自己造启发式。
   太墟路径 `/workspace/taixu-ref`，关键文件：
   - `harness/src/main/java/top/wkbin/taixu/harness/HarnessLoop.kt`（主循环、RoundBudget）
   - `.../ProviderClient.kt:711 sanitizeApiTranscript`（出口配对修复）
   - `.../prompt/MemoryRecallSelector.kt`（BM25 召回）
   - `.../prompt/SystemPromptBuilder.kt`（分层 + prefix cache 契约）
   - `core/database/.../AgentContextDao.kt`（pinned 排序必须内容确定）
3. **每完成一轮改动**：更新记忆 + 更新计划 + 提交 + 推送，连续作业不停下问。
4. 会话沙箱：rust 工具链 `export PATH=/opt/taixu/toolchains/rust/bin:$PATH`

## 已完成（已提交推送）

| 阶段 | commit | 内容 |
|---|---|---|
| M0 地基 | `d071b8b` | crate 骨架、vault（init/show/import-llm）、config（runtime.yaml + 委托 ne-engine）、models list/check |
| M1 它自己 | `cbe7acd` | session.rs（会话**树**）、prompt.rs（分层 + 双层资源 + `{{VAR}}` 严格）、tool.rs（enum Tool + 沙箱）、harness.rs（多轮循环 + 熔断 + sanitize）、task.rs（chat） |
| M2 记忆 | `cbff26c` | memory.rs（pinned 常驻 system + BM25/CJK-bigram recall 挂 user 轮） |

测试规模：ne-agent 51 条；全 workspace 243 通过 0 失败。

### 关键的、不可逆的决定（已定死）

- **会话存树**（`parent_id`）；`history()` 沿 head 回溯，分叉只取 head 路径。
- **system prompt 只放稳定字节**；逐轮内容走 `TurnPrefix` 挂最新 user 轮。
- **pinned 排序必须内容确定**（`scope权威序 → createdAt → id`），**不能用 updatedAt**（会击穿 prefix cache）。

## 进行中：M3 小说任务

### 本段的核心发现

`ne_engine::budget::build_injection` 与 `ne_engine::chapter::build_prompt` 早已写好，
但它们依赖的 `BudgetCtx` / `ChapterCtx` trait **全仓库只有测试里的 mock，没有真实现**
→ **半条生成链是断的**。M3 要补的就是这一半。

### 已完成（未编译未提交）

- `crates/ne-agent/src/novel_ctx.rs`（425 行，7 单测）：
  - `NovelCtx::open(root)` —— **只读**打开 `novel.db`（`SQLITE_OPEN_READ_ONLY`）
  - `impl BudgetCtx for NovelCtx`：`outline_text` / `material`（含 old_code/new_code 兜底）/
    `read_lib_file`（磁盘优先，DB content 回退）/ `role_rows` / `char_card_rows` /
    `foreshadow_rows` / `event_rows` / `prev_tail`
  - `impl ChapterCtx for NovelCtx`：`injection`（委托 `budget::build_injection`，口径一致）/
    `prompt_file`（库根 `05_管线框架/prompts/` 优先）/ `kb_rules`（**真实库无 kb_rules 表，如实返空不杜撰**）/
    `material_content`
  - 只读原则：不写任何库表；查询失败优雅降级返回空，不炸整条链

### 真实库事实（实地确认 2026-10-11）

库根 `/sdcard/Download/小说相关备份/projects/projects/系统/资料库`：
- `novel.db` 65 张表；`materials` 916 行；`chapters` **0 行**；**无 `kb_rules` 表**
- `foreshadow` / `events` 表存在（样例：`EVT-100 残响初鸣 pending`）
- `03_叙事规划/细纲/卷NN.md`、`04_正文/卷NN/第N章.md` 存在
- `05_管线框架/prompts/` 下有模板（`l5_正文.md` 等）
- **`02_资料库` 目录已不存在** → 卡片走 DB content 回退

### 剩余步骤（M3 收尾）

1. `lib.rs` 加 `pub mod novel_ctx;`；`cargo build/test -p ne-agent` 跑到全绿
2. `task.rs` 加 `novel` 任务函数：
   `NovelCtx::open(vault)` → `ChapterCtx::injection` → `chapter::build_prompt` →
   `llm::call` → `chapter::write_chapter` → `gates::run_all` → 写回 `chapters` 表
3. CLI 加 `novel` 子命令（`--chap-id N`、`--volume`、`--dry-run`、`--json`）
4. 真机验收：假 LLM 服务端跑通整章（注入包非空 + prompt 含模板 + 门禁返回）
5. 提交推送 + 更新记忆/计划

### 需要读的接口（M3 剩余步骤要用）

- `ne_engine::chapter::write_chapter(...)` —— 签名见 `crates/ne-engine/src/chapter.rs:291`
- `ne_engine::chapter::build_prompt<C: ChapterCtx>(ctx, chapter, volume) -> Result<String>`
- `ne_engine::budget::build_injection<C: BudgetCtx>(ctx, volume, chapter) -> Result<Value>`
- `ne_engine::gates::run_all(text, chapter, volume, cfg, ctx: &dyn GateCtx) -> GateReport`
- `ne_engine::config::load(root) -> Result<Config>`
- `ne_engine::ctx::RepoCtx::{open, impl GateCtx}`
- `ne_engine::llm::{LlmParams, Message, call}`（**LlmOutput 无 finish_reason**；**Message 无 PartialEq**）
- `ne_engine::providers::{load_from_dir, resolve_llm_dir, build_params}`

## 后续（M4–M7，见 docs/ROADMAP-ne-agent.md）

- **M4 外部世界**：`web_search`/`web_fetch` 工具（复用已装 `mcp_websearch`）+ 收编 ne-mcp 6 工具为内置
- **M5 长大**：`ContextWindowPolicy`（token 估算 + 预算）、compaction、prefix cache 落实（文件字节戳驱动）、会话树 rewind
- **M6 安全协作**：审批三档、持久化任务状态机 + CAS、`plan`/`scratchpad`/`load_rule`/`load_skill`
- **M7 完善**：子智能体 Lane、单二进制打包、架构门禁

## 命令速查

```bash
export PATH=/opt/taixu/toolchains/rust/bin:$PATH
cd /workspace/小说系统
cargo build -p ne-agent && cargo test -p ne-agent
cargo test --workspace                     # 243 基线

# 端到端（假 LLM）：见 crates/ne-agent/tests/{strict_llm_server.py,mem_capture_server.py}
export TAIXU_LLM_DIR=/tmp/fake-llm        # 指向含 models.yml+config.yml 的目录
target/debug/ne-agent --vault $V vault init
target/debug/ne-agent --vault $V chat -m "..."
target/debug/ne-agent --vault $V memory save k v --pinned
target/debug/ne-agent --vault $V session list

# 推送
source /attachments/skills/github-upload/scripts/gh-push.sh && gh_fix_dns
git push origin main
```
