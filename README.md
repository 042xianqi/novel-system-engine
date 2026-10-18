# 小说系统 · 残响之河 Rust 引擎

《残响之河》小说生成管线的 **纯 Rust 实现**。原为 Python 原型（21 个脚本），
现全量移植为 Rust，用于跨设备（Android aarch64 / 桌面）离线运行。

## 结构

| crate | 职责 |
|---|---|
| `ne-core` | 核心数据结构（`InjectionPacket` 等）、维度常量 |
| `ne-funnel` | L0-L5 六层漏斗框架（`funnel.yaml` 单一事实源 + 门禁） |
| `ne-db` | SQLite 访问层（`novel.db`） |
| `ne-injection` | 注入包构建（ranked / 预算裁剪） |
| `ne-engine` | 引擎主体：门禁 / LLM / 预算 / 灌库 / 世界物理 / 角色标定 |
| `ne-qc` | 质检门禁（`gate14` G01–G16 + 六维框架） |
| `ne-cardgen` | 角色卡抽取生成 |
| `ne-mcp` | MCP stdio 服务（对外工具接口） |

## 构建

```sh
cargo build              # 宿主机
cargo test               # 139 tests
cargo build --target aarch64-linux-android   # Android（需 NDK）
```

## 引擎模块对应关系（移植自 Python）

| Rust | Python 源 |
|---|---|
| `ne-engine/dynamics.rs` | `dynamics.py`（8 章级公式） |
| `ne-engine/world_physics.rs` | `world_physics.py`（civ.tide / ent.entropy / res.decay） |
| `ne-engine/gate14.rs` | `gate14.py`（**G01–G16 共 16 道门禁**） |
| `ne-engine/ooc_gate.rs` | `ooc_gate.py`（5 判据） |
| `ne-engine/budget.rs` | `context_budget.py` |
| `ne-engine/emotion.rs` + `pyrandom.rs` | `emotion_engine.py`（MT19937 逐位对齐） |
| `ne-engine/chapter.rs` | `chapter_engine.py` |
| `ne-engine/calib.rs` | `char_calib_etl.py` |
| `ne-engine/loaders.rs` | `load_config.py` + `load_structure.py` |
| `ne-engine/role_samples.rs` | `load_role.py` + `load_samples.py` + `load_state_b.py` |
| `ne-engine/state_load.rs` | `load_state_a.py` + `emuload.py` + `tick.py` |
| `ne-engine/ops.rs` | `cold_backup.py` + `pipeline_backend.py` + `fill.py` |
| `ne-engine/http.rs` + `keypool.rs` | 替代旧的 `omp-rs-7k`（tokio + reqwest） |

## 移植中修复的上游缺陷

1. **`gate14.py` G08 从未生效** —— `bayes.posterior` 返回字符串 `'0.5000'`，
   代码直接与 float 比较 → `TypeError` → 被 `run_14` 的 `except` 吞成 skip。
   「伏笔回收率」这道门禁在原实现里一次都没跑过。Rust 版正确解析。
2. **`emotion_calib_map.json` 语法损坏** —— rule id=3 的 `effect.base` 缺右花括号、
   rule id=12 整行被截断。`json.load()` 必抛 `JSONDecodeError`
   ⇒ `char_calib_etl.py` 从未成功运行。Rust 侧已修复并在加载时做结构校验。
3. **装载脚本硬编码路径** —— `load_state_a.py` / `load_state_b.py` / `emuload.py` /
   `tick.py` / `fill.py` / `cold_backup.py` 均写死
   `ROOT = "/sdcard/projects/系统/资料库"`（真库在别处）。
   Rust 版路径全部由调用方注入。

## 测试

`crates/ne-engine/tests/fixtures/` 含两个测试夹具（角色标定映射表、情绪 schema）。

关键对拍（与 Python 原型，已实测）：

- `gate14` 16 门禁 pass 判定 **0 处不一致**
- MT19937 随机数 **逐位一致**
- SHA-256 **逐位一致**
- `dynamics` 8 公式 **零差异**
- `world_physics` selftest **12/12**
