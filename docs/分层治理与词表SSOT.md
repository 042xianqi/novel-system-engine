# 分层治理与词表 SSOT（设计层 / 叙事层分离）

> 生成：2026-10-16 · 对应版本 `0.0.24`（拟）
> 适用：`ne-core::lexicon` 及所有引用它的门禁/注入/清洗模块
> 关联：`docs/系统功能总览.md`、`docs/全项目目录.md`、`05_管线框架/rules.md`

---

## 1. 为什么做这件事（根因）

### 1.1 问题现象

写正文前，细纲/资料卡里带着工程代号（「推演面板」「面板」「系统」）。这些词经注入链路进入 L5 生成 prompt，
模型照抄成正文里的「系统行」——例如 `面板弹出一行：「预兆真。」` 这类面板提示语。

### 1.2 根因：系统缺「层」的概念

不是「某个词漏了」，而是**设计层语言与叙事层语言混在一起**，靠注入前一份黑名单临时擦除。四条证据：

1. **同一套词身兼两职**：`推演/面板/系统` 既是设计层真值（`02_资料库/物品/ITEM-99-005`＝推演进化面板），
   又被写进叙事层语料（细纲、资料卡）。正确范式只写在 `02_资料库/角色/CHAR-99-001` 一处：
   > 「异样之物（设计层，正文不可见）…来源设定只存 ITEM-99-005，**叙事层永不引用**」
   ——未贯彻全库。
2. **L3 细纲是混层产物**：既要给设计者看懂，又**直接当 L5 注入语料**（`chapter.rs::build_prompt` 的
   `【细纲】` 段）。两种用途的语言要求互斥。
3. **词表 8 处副本，无 SSOT**：各模块各写一份，已实测漂移（`BOOT_DESIGN_WORDS` 用「分支演算」，
   `DESIGN_WORDS` 却是「分支」）。改一个词要动 5~6 个文件，必然漂移。
4. **注入通道逐条审计发现泄漏点**（`chapter.rs::build_prompt`）：
   - 章末钩子 `hook_line` 在净化**前**抽取（原 `:562`，注释明示「不中性化」）→ **主泄漏通道**
   - `foreshadow.due[].content`、`kb_rules.rule_text`、`char.speech_style/catchphrase` 均未净化
   - 全库曾 **150 条钩子行含设计层词**（卷02:14 / 05:6 / 06:54 / 07:16 / 08:45 / 09:9 / 10:6）

---

## 2. 架构

### 2.1 分层模型

```
┌─ 设计层（Design）─── 只给设计者/引擎看，永不出现在正文 ──────────┐
│  神话词   存在之基/存在之核/高维观测者文明/阿莱夫·零/金色立方体    │
│  设计层词 推演/面板/漏斗/因果流/算账/算路径/影子层/分支演算        │
│  来源：ITEM-99-005（金手指真值）、L0-L5 影子演算工程代号          │
└──────────────────────────────────────────────────────────────┘
        │  注入期 sanitize_injection（边界封闭）中性化
        ▼
┌─ 叙事层（Narrative）── 最终进入 prompt 与正文 ───────────────────┐
│  金手指   「他脑子里那股说不清的东西」「后脑那阵说不清的体感」     │
│  神话替代 「某样东西」「未知来源」                                │
└──────────────────────────────────────────────────────────────┘
        │
        ▼
┌─ 正典层（Canon）── 世界观内部合法用词，与金手指无关 ─────────────┐
│  「推演院/推演师/大推演师/因果推演」= 世界观能力/机构/职业       │
│  （POWR-99-004 推演殿、RELA-99-008 星枢推演院、CHAR-50-012）     │
│  ⚠ 与设计层词的「推演」**同形不同义**，见 §5 待决问题            │
└──────────────────────────────────────────────────────────────┘
```

### 2.2 词表 SSOT：`crates/ne-core/src/lexicon.rs`

**唯一真值源**。全部词表在此定义，其余模块 `use ne_core::lexicon::…`，**禁止本地副本**。

| 常量 | 含义 | 用途 |
|---|---|---|
| `MYTH_NEUTRAL` | 神话词 → 中性词（6 对） | 注入期替换 |
| `MYTH_BANNED` | 神话词硬闸（6 词） | g14 正文判据 |
| `DESIGN_LAYER_NEUTRAL` | 设计层词 → 中性词（10 对） | 注入期替换 |
| `DESIGN_LEAK_WORDS` | 设计层词**探测集**（9 词） | 门禁/自检扫描 |
| `BANNED_WORDS` | 正典禁词（归墟/熵兽/热寂前夜） | G13 |
| `AI_TRACE_WORDS` | AI 痕词（8 词） | G13 AI词维度 |
| `LEAK_WORDS` | 指令泄漏词（8 词） | g16 |
| `NEG_MARKS` + `is_prohibition_line()` | 显式禁令行标记（10 词） | sanitize/扫描共用口径 |

**一致性约束**：`MYTH_NEUTRAL` 键集合 == `MYTH_BANNED`（单测 `myth_neutral_keys_match_myth_banned` 锁定）。

### 2.3 引用关系（改动前 → 改动后）

| 模块 | 原本地副本 | 现状 |
|---|---|---|
| `ne-engine/chapter.rs` | `BANNED_WORDS`/`MYTH_NEUTRAL`/`DESIGN_LAYER_NEUTRAL`/`AI_TRACE_WORDS` | `pub use ne_core::lexicon::{…}` |
| `ne-engine/gates.rs` | `MYTH_BANNED`/`G13_STOP_WORDS`/`G17_DESIGN_LEAK_WORDS`/`LEAK_WORDS`/`G13_AI_WORDS` | 切片视图引用 SSOT |
| `ne-engine/gate14.rs` | `MYTH_BANNED`/`LEAK_WORDS` | `pub use` |
| `ne-agent/health.rs` | `BOOT_DESIGN_WORDS`/`BOOT_NEG_MARKS` | `use … as …` 别名 |
| `ne-agent/outline_rebuild.rs` | `DESIGN_WORDS`/`NEG_MARKS` | `use … as …` 别名 |

> **实测修复的漂移**：`BOOT_DESIGN_WORDS`（「分支演算」）vs `outline_rebuild::DESIGN_WORDS`（「分支」）→ SSOT 统一取精确的「分支演算」。

### 2.4 注入面封闭（`chapter.rs::build_prompt`）

**原则**：注入侧**边界封闭**——每个字段必过 `sanitize_injection`，而非事后黑名单。

| 注入字段 | 位置 | 状态 |
|---|---|---|
| 细纲整体 `outline` | `build_prompt` | ✅ 净化 |
| 章末钩子 `hook_line` | `build_prompt` | ✅ **改为从净化后 outline 抽取**（原主泄漏通道） |
| 资料卡 `materials` | `build_knowledge_injection` | ✅ 净化+剥叙事 |
| 上一章尾巴 `prev_tail` | `build_knowledge_injection` | ✅ 净化 |
| 定版台词 `canon_lines` | `build_knowledge_injection` | ✅ 含设计层词直接丢弃 |
| 到期伏笔 `foreshadow.due[].content` | `build_knowledge_injection` | ✅ 已补 |
| 写法规则 `kb_rules.rule_text` | `build_knowledge_injection` | ✅ 已补 |
| 角色卡 `speech_style`/`catchphrase` | `build_knowledge_injection` | ✅ 已补 |

---

## 3. 原理

### 3.1 `sanitize_injection(md) -> String`

逐行处理：
1. **显式禁令行原样保留**（`is_prohibition_line`）——保规则语义（「严禁出现『面板』」不能被改写）。
2. 其余行：先 `neutralize_myth`（神话词→中性词），再按 `DESIGN_LAYER_NEUTRAL` 替换（长组合优先），
   最后兜底 `推演 → 琢磨`。

### 3.2 门禁三层

| 层 | 命令 | 判据 | 阻塞级别 |
|---|---|---|---|
| **细纲出厂** | `ne-agent outline check --volume 卷NN` | 章文本设计层词 / 章概要雷同 | Fatal（exit 1） |
| **启动自检** | `ne-agent health boot --volume 卷NN` | 注入文件/注入表/资料卡/正文/细纲 | 本卷注入面污染 = Fatal |
| **L5 质检** | `ne-engine check` / `ne-qc` G17 | 正文设计层词 | 卷01-03 = P0，卷04+ = P1 |

> `gates.rs::g17_design_leak`：卷号 1-3 → P0 打回；卷 4+ → P1（认知曲线可逐步显性）。

### 3.3 清洗工具 `outline sanitize`（存量治理）

调本地 LLM，**结构保持**改写：
- 保留：章头/字段名/卡号/锚点/引典行结构
- 改写：设计层词 → 叙事层表达；标题内设计层词；引典行内描述词
- 校验（不通过重试 ≤3 次）：无设计层词 / 引典卡号未丢 / 叙事句内无 `ITEM-99-005` / 无裸英文词
- 幂等：已干净的章自动 skip；备份 `.bak_sanitize`

---

## 4. 运行

### 4.1 日常命令

```sh
# 细纲出厂门禁（写正文前必跑，只读零外呼）
ne-agent outline check --lib <资料库根> --volume 卷01

# 启动自检（含细纲设计层词 Fatal 判定）
ne-agent health boot --lib <资料库根> --volume 卷01

# 细纲雷同差异重建（调 LLM）
ne-agent outline rebuild --lib <资料库根> --volume 卷01 [--beat N] [--dry-run]

# 细纲设计层词清洗（调 LLM）
ne-agent outline sanitize --lib <资料库根> --volume 卷02 [--chapter N] [--dry-run]
```

### 4.2 前置

- LLM 服务（本地，OpenAI 兼容）：`http://127.0.0.1:8788/v1`（`curl .../v1/models` 验证可达）
- 库根：`/sdcard/Download/小说相关备份/projects/projects/系统/资料库`（经 `--lib` 或 `NE_AGENT_LIB`）
- 构建：`cargo build -p ne-agent`（或 `cargo build --workspace`）

### 4.3 回归

```sh
cargo test --workspace    # 当前基线 829 passed / 0 failed
```

---

## 5. 进度

| 阶段 | 内容 | 状态 |
|---|---|---|
| **Phase 1** | 词表 SSOT（`lexicon.rs` + 8 处副本收敛） | ✅ 完成 |
| **Phase 2** | 注入面封闭化 + `health boot` Fatal + `outline check` | ✅ 完成 |
| **Phase 3** | 存量细纲清洗（卷02/04-10） | ⚠️ **85%**（1723→246 行） |
| **Phase 3.2** | 相关资料卡叙事化 | ⬜ 未做 |

**Phase 3 明细**：卷01/02/03/04/10 **归零**；卷05/06/07/08/09 有残留。
章数完整（各 180 章，无损）；备份在 `09_冷备份/细纲_清洗前_20261015/`。

残留 246 行分布：金手指工程词 103 / 正典专名 72 / 其它 71。主因：LLM 3 次收敛上限。

---

## 6. 问题 / 待决

### 6.1 ⚠ 「推演」双重身份冲突（需拍板）

`推演` 在本系统中同时是：
- **设计层词**（金手指 `ITEM-99-005` 推演面板）→ 该禁
- **世界观正典**：`推演院`(7) / `推演师`(4) / `推演殿`(3) / `大推演`(10) / `因果推演`(11)
  （POWR-99-004、RELA-99-008、CHAR-50-012）

而 `STYLE_CONSTRAINTS` 第 0 条**一刀切禁「推演」**，G17 探测集也用裸「推演」。

**后果**：清洗工具对正典专名有误伤风险（实测多数保住；卷08 一处 `太玄推演使` 被改）。

**选项**：
1. 正典专名**合法**（建议）——世界观能力非金手指；G17 自卷04 起本就是 P1。
2. 词表**拆分**「金手指推演」vs「世界观推演」（建议）。

### 6.2 其它遗留

- **段落门禁不对称**：只查 `> MAX_PARA`，不查 `< MIN_PARA`（第1章 28 段 < 30 被放行）。
- **G07 数据源噪声**：`kb_world_consistency.json` 的 `canon_invariants` 混通用词。
- **旧正文残留**：第1↔2 章有 17 个 8-gram 重合（细纲已差异化，旧正文仍在）。

---

## 7. 变更清单（本次提交）

| 文件 | 变更 |
|---|---|
| `crates/ne-core/src/lexicon.rs` | **新增**：词表 SSOT |
| `crates/ne-core/src/lib.rs` | 注册 `pub mod lexicon` |
| `crates/ne-engine/src/chapter.rs` | 引用 SSOT；注入 4 字段封闭化；hook 净化后抽取 |
| `crates/ne-engine/src/gates.rs` | 引用 SSOT（切片视图） |
| `crates/ne-engine/src/gate14.rs` | 引用 SSOT |
| `crates/ne-agent/src/health.rs` | 引用 SSOT；新增细纲设计层词 Fatal |
| `crates/ne-agent/src/outline_rebuild.rs` | 引用 SSOT；新增 `sanitize` |
| `crates/ne-agent/src/outline_parse.rs` | 细纲解析唯一真值源 |
| `crates/ne-agent/src/cli.rs` / `main.rs` | 新增 `outline check` / `outline sanitize` |
| `crates/ne-qc/tests/dedup_compare.rs` | 修 G01_B 遗留回归（测试语料） |
