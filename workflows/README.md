# workflows/ —— ne-agent 的构建/测试/提交/推送工作流

## 设计（2026-10-11 定）

**脚本是唯一真源；太墟工作流 JSON 只写一行调用；全程可后台跑。**

```
workflows/
├── run.sh                统一入口：bash workflows/run.sh <名字> [参数...]
├── scripts/              唯一的逻辑真源（真正干活的地方）
│   ├── _common.sh        公共前置：wf_env（仓库根 + Rust 工具链）
│   └── ne_{build,test,gate,commit,push,verify}.sh
├── taixu/                太墟工作流薄壳（3 节点，只调 run.sh）
│   └── ne_*.json
├── install_taixu_workflows.py   把 taixu/*.json 装进太墟真库
└── _lib.sh               步骤库（wf_step/wf_run/wf_cargo_summary…），被 _common.sh 兜底加载
```

### 为什么这样拆

改造前：6 个 JSON 各自内联大段 bash。问题——

1. 同一逻辑无法在沙箱侧复用，只能绕过工作流手敲命令 ⇒「松散脚本」的老毛病；
2. JSON 里的脚本无法版本化 / 单测；
3. 要后台跑得另写一份，两处易漂移。

改造后：`/wf ne_X`（太墟 UI）与 `bash workflows/run.sh ne_X`（沙箱/后台）**跑的是同一份脚本**，行为必然一致。

## 两条触发路径

### ① 沙箱 / 后台（本仓库内，无需太墟）
```bash
bash workflows/run.sh --list              # 列出可用工作流
bash workflows/run.sh ne_gate             # 跑门禁
bash workflows/run.sh ne_verify           # 门禁→测试→release→产物体检
```
- 日志：`.wf/logs/<名字>.log`（已 gitignore），前台可见、后台可查。
- **后台托管**（不阻塞前台）用宿主 process 工具：`start → bash workflows/run.sh ne_push`，
  再用 `status` / `logs` 查进度。**不要**用 `nohup` 或 `&`。

### ② 太墟 UI
`/wf ne_push` 等（slashCommand 已在 JSON 里声明）。它内部执行的就是
`bash workflows/run.sh <名字>`。

## 环境变量

| 变量 | 作用 | 默认 |
|---|---|---|
| `BRANCH` | ne_push 目标分支 | `main` |
| `COMMIT_MESSAGE` | ne_commit 提交说明（**必填**） | 无（缺则拒绝提交） |
| `TARGET` / `PROFILE` | ne_build 包名 / debug\|release | `ne-agent` / `debug` |

`ne_commit` 把原来的 `HUMAN_APPROVAL`（需人在 UI 点）换成「**必须传 `COMMIT_MESSAGE`**」——
让"后台可跑"成为一等公民，同时保留"无说明即拒绝"的护栏。

## 装进太墟真库（可选）

太墟工作流只存在于 App 的 Room DB（`workflows.jsonContent`），没有文件级导入通道。

```bash
# 宿主 root 把真库拷到 /sdcard/Download（与沙箱共享）后：
python3 workflows/install_taixu_workflows.py --db <拷出的 taixu.db> --dry-run
python3 workflows/install_taixu_workflows.py --db <拷出的 taixu.db>
# 拷回 + 重启太墟 App 让 Room 重新加载
```

⚠️ **真库被运行中的太墟 App 占用时不可整文件替换**（丢 WAL、App 仍指旧 inode）。
需先停 App，写完再启动——这会中断当前会话，须用户确认。
