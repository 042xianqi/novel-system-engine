// main.rs —— 残响之河桌面壳（Tauri 2 预研入口）
//
// # 职责（最小可用）
// 1. 以 **sidecar** 方式拉起 `ne-agent serve --port 8270`（即现有 web_gateway）；
// 2. 轮询等端口就绪（避免窗口白屏时后端还没起来）；
// 3. 窗口加载 `http://127.0.0.1:8270`（dev 模式）或打包内的 `frontendDist`
//    静态资源 + 同源 API（生产模式，前端 fetch('/api/...') 天然同源）。
//
// # 为什么不用 Tauri 的 start_server + 内置 HTTP
// 我们的 web_gateway 是**手写 std::net 阻塞 HTTP/1.1**（无 tokio/axum），
// 已经把静态 + API 全包了。套进 Tauri 只需"把窗口指过去"，不必重写服务端。
//
// # 阻塞说明（沙箱）
// 本目录是**预研产物**，在当前 PRoot aarch64 沙箱内**无法真编译**：
//   pkg-config --exists webkit2gtk-4.1 → 缺失
//   pkg-config --exists gtk+-3.0       → 缺失
// 二者是 Tauri Linux 端的硬依赖。要在真机/桌面发行版上跑，需先装：
//   Debian/Ubuntu: sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev \
//                                  librsvg2-dev patchelf
//   Android 目标则无需 GTK，改用 tauri android init（需 Android SDK/NDK）。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 后端端口（与 web_gateway 默认一致）
const BACKEND_PORT: u16 = 8270;
/// 等后端就绪的上限
const BOOT_TIMEOUT: Duration = Duration::from_secs(25);

/// 持有 sidecar 子进程句柄：窗口关闭时统一回收，避免僵尸进程。
struct Sidecar(Mutex<Option<Child>>);

/// 轮询 `/api/*` 端口是否可连（TCP 层就绪即可，不必等 HTTP 体）。
fn wait_port(port: u16, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if TcpStream::connect_timeout(
            &format!("127.0.0.1:{port}").parse().unwrap(),
            Duration::from_millis(300),
        )
        .is_ok()
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

/// 拉起 ne-agent sidecar。
///
/// sidecar 有两种来源：
///   · 打包后：Tauri 会把 `binaries/ne-agent-<target-triple>` 释放到资源目录，
///     用 `tauri::process::Command::new_sidecar("ne-agent")` 取路径（此处为
///     减少对 tauri crate 的强依赖，直接用相对路径 + PATH 兜底）。
///   · 开发时：直接调用 workspace 里已构建的 target/release/ne-agent。
fn spawn_backend() -> Option<Child> {
    let candidates = [
        // 打包：Tauri externalBin 释放位置（同目录）
        "ne-agent",
        // 开发：workspace 构建产物（从 src-tauri 往上三级）
        "../../target/release/ne-agent",
        "../../target/debug/ne-agent",
    ];
    for bin in candidates {
        let r = Command::new(bin)
            .args(["serve", "--port", &BACKEND_PORT.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Ok(c) = r {
            return Some(c);
        }
    }
    None
}

fn main() {
    // ── 1. 启动后端 ──
    // 若端口已被占用（用户已手动开了面板），就不再重复 spawn。
    let child = if wait_port(BACKEND_PORT, Duration::from_millis(150)) {
        None
    } else {
        spawn_backend()
    };

    // ── 2. 等就绪 ──
    let ready = wait_port(BACKEND_PORT, BOOT_TIMEOUT);
    if !ready {
        eprintln!("[tauri-shell] 后端 {BACKEND_PORT} 端口 {BOOT_TIMEOUT:?} 内未就绪，窗口可能白屏");
    }

    // ── 3. 建窗口 ──
    // 注：dev 下 devUrl 直连 8270；release 下加载打包的 frontendDist，
    //     其内 fetch('/api/...') 会打到 tauri:// 同源 —— 生产需把 API 也
    //     经 Tauri 的 custom protocol 代理到 127.0.0.1:8270（见 README 第 3 节）。
    let sidecar = Sidecar(Mutex::new(child));

    tauri::Builder::default()
        .manage(sidecar)
        .setup(|_app| Ok(()))
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Destroyed = event {
                // 最后一个窗口关闭 → 回收 sidecar
                if let Some(state) = window.try_state::<Sidecar>() {
                    if let Ok(mut g) = state.0.lock() {
                        if let Some(mut c) = g.take() {
                            let _ = c.kill();
                            let _ = c.wait();
                        }
                    }
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("tauri 运行时错误");
}
