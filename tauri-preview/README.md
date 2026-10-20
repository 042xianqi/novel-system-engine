# 残响之河 · 桌面壳（Tauri 2 预研）

> 状态：**预研产物**。配置合法（`cargo metadata` exit 0），但**当前沙箱内无法真编译**，
> 阻塞证据与解法见下文第 4 节。不要把本目录当成可用产物。

## 1. 目录结构

```
tauri-preview/
└── src-tauri/
    ├── Cargo.toml        # 独立 workspace（不并入主仓库，避免 tauri 拖进主构建）
    ├── build.rs          # tauri_build::build()
    ├── tauri.conf.json   # 窗口 / 打包 / CSP / externalBin 声明
    └── src/main.rs       # 启动入口：拉起 ne-agent sidecar → 等端口 → 开窗口
```

## 2. 为什么要这个壳

现状：`ne-agent serve --port 8270` 已经用**手写 std::net 阻塞 HTTP/1.1**
（零 tokio / 零 axum）同时提供静态资源 + `/api/*` + SSE。用户要在浏览器里开面板。

桌面壳只需把**同一个后端进程**包装进原生窗口，收益：

- 免开浏览器、独立图标与窗口、系统托盘可扩展
- 打包成 `.deb` / `.AppImage` 后双击即用（含 sidecar 二进制）
- 未来接 Android（Tauri 2 支持 Android target）时复用同一份前端

**不做**：不重写 HTTP 层（无必要）、不引入 tokio（破坏仓库铁律）、不改前端 API 路径。

## 3. sidecar 方案（关键）

Tauri 的 `externalBin` 机制：把 `ne-agent` 二进制放到
`src-tauri/binaries/ne-agent-<target-triple>`（如 `ne-agent-aarch64-unknown-linux-gnu`），
打包时自动随包分发，运行时可用 `Command::new_sidecar("ne-agent")` 拿到路径。

本项目在 `src/main.rs::spawn_backend()` 里做了**三级回落**，兼容开发态与打包态：

| 优先级 | 路径 | 场景 |
|---|---|---|
| 1 | `ne-agent`（PATH） | 打包后 sidecar 已释放到资源目录 |
| 2 | `../../target/release/ne-agent` | 开发：workspace 已 release 构建 |
| 3 | `../../target/debug/ne-agent` | 开发：debug 构建 |

启动流程：`wait_port(8270, 150ms)` 探测端口 → 未占用则 spawn sidecar →
`wait_port(8270, 25s)` 等就绪 → 建窗口。窗口 `Destroyed` 时回收子进程（防僵尸）。

### Python 脚本依赖（`exec_pending.py` 等）

两类方案，按目标平台选：

1. **不打包 Python**（推荐先走这条）：`exec_pending.py` 由用户在资料库目录自行
   放置，后端按相对路径调用 `python3`。优点是零打包复杂度，缺点依赖系统 Python。
2. **打包 Python（PyInstaller）**：把 `exec_pending.py` 用 PyInstaller 打成单文件
   `exec-pending`，同样放进 `binaries/` 走 externalBin，后端改成优先调
   `exec-pending` 再回落 `python3 exec_pending.py`。
   - 体积：一个 Python 运行时约 +12~20MB
   - 交叉编译坑：PyInstaller 不支持跨平台打包，需在目标架构上构建
     （aarch64 需在 aarch64 机器上打）

> 提醒：仓库铁则要求「不引入 tokio/reqwest」，Tauri 自身**内部**用 tokio 不违反
> ——因为壳是独立 crate/独立 workspace，不进入 `ne-agent` 的依赖树。这条边界
> 由 `Cargo.toml` 里显式 `[workspace]`（空表）保证。

## 4. 本沙箱的阻塞证据（重要）

```
$ uname -m
aarch64

$ pkg-config --exists webkit2gtk-4.1 && echo OK || echo MISSING
MISSING

$ pkg-config --exists gtk+-3.0 && echo OK || echo MISSING
MISSING
```

`webkit2gtk-4.1` 与 `gtk+-3.0` 是 **Tauri Linux 端硬依赖**（WebView 载体）。
本沙箱是 PRoot + aarch64，未装这两个 dev 包，因此：

- ✅ `cargo metadata` 通过（配置合法，`META_EXIT=0`）
- ❌ `cargo check` 无法完成（缺系统库，不是代码问题）

### 解除办法

**桌面 Linux 目标**（真机或有 apt 的环境）：

```bash
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev patchelf
cd tauri-preview/src-tauri
cargo build --release          # 首次会拉 tauri 依赖树，约 400+ crates
```

**Android 目标**（无需 GTK）：

```bash
npm i -g @tauri-apps/cli@2
cd tauri-preview
tauri android init             # 需要 Android SDK + NDK
tauri android build --apk
```

> 注意 Tauri 2 的 Android 构建要求 NDK 与 JDK，且首次拉依赖体积较大；
> 建议在联网良好的环境下进行。

## 5. 生产模式下 API 同源问题

- **dev**（`devUrl = http://127.0.0.1:8270`）：窗口直接加载后端，`fetch('/api/...')`
  天然同源，零改动。
- **release**（`frontendDist`）：窗口加载 `tauri://localhost` 下的静态文件，
  `fetch('/api/...')` 会打到 tauri 协议而非后端。三种解法：
  1. 保持 `devUrl` 指向 sidecar（窗口始终加载 `http://127.0.0.1:8270`）—— 最省事，推荐；
  2. 自定义 protocol handler 把 `/api/*` 反代到 `127.0.0.1:8270`；
  3. 前端把 API base 改成绝对地址 `http://127.0.0.1:8270`（需同步放开 CSP 的
     `connect-src`，本项目已在 `tauri.conf.json` 里预留）。

当前 `tauri.conf.json` 采用**方案 1 的部署形态**（`devUrl` 已指向 8270），
`frontendDist` 仅作为打包兜底静态资源。

## 6. 下一步（尚未做）

- [ ] 在装好 GTK/WebKit 的环境里跑通 `cargo build --release`
- [ ] 生成图标（`tauri icon <png>`）
- [ ] 决定 Python 打包路线（不打包 / PyInstaller）
- [ ] 系统托盘与开机自启（可选）
