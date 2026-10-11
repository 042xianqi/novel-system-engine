#!/bin/bash
# Android aarch64 交叉编译 —— 把 ne-agent 编译成可在 Android/Termux 系环境运行的产物。
#
# 与 linux aarch64 产物的区别：目标是 `aarch64-linux-android`（bionic libc），
# 不是 glibc。链接器与 C 工具链来自 NDK 的 clang。
#
# 关键点（踩坑记录）：
#   * rusqlite 开的是 `bundled` → 会用 `cc` crate 编译 SQLite 的 C 源码。
#     因此**必须**给 cc 传 NDK clang（CC_/AR_/CXX_ 目标变量），否则它会找
#     系统 cc，编出 glibc 目标物，链 bionic 时炸在 "cannot find -lc"。
#   * 目标变量名两种写法都要给：`aarch64-linux-android`（连字符）与
#     `aarch64_linux_android`（下划线）——不同版本的 cc crate 读法不同。
#   * API level 取 24（Android 7.0，覆盖绝大多数在用设备；过低会缺部分符号）。
#
# 用法：
#   bash scripts/cross-build-android.sh            # 默认 API 24，release
#   API=28 bash scripts/cross-build-android.sh
#   PROFILE=debug bash scripts/cross-build-android.sh
set -euo pipefail

API="${API:-24}"
PROFILE="${PROFILE:-release}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
NDK="${ANDROID_NDK_HOME:-/opt/taixu/toolchains/android/ndk}"

if [ ! -d "$NDK" ]; then
  echo "✗ 找不到 NDK：$NDK（可设 ANDROID_NDK_HOME 覆盖）" >&2
  exit 1
fi

HOST_TAG="linux-aarch64"
[ -d "$NDK/toolchains/llvm/prebuilt/$HOST_TAG" ] || HOST_TAG="linux-x86_64"
BIN="$NDK/toolchains/llvm/prebuilt/$HOST_TAG/bin"

CLANG="$BIN/aarch64-linux-android${API}-clang"
[ -x "$CLANG" ] || { echo "✗ 找不到 NDK clang：$CLANG" >&2; exit 1; }

export PATH="/opt/taixu/toolchains/rust/bin:$PATH"

# 链接器
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$CLANG"
# C 工具链（rusqlite bundled 用得到）。下划线名可用 export；连字符名不能
# `export`（bash 报 not a valid identifier）→ 交给 `env` 传。
export CC_aarch64_linux_android="$CLANG"
export CXX_aarch64_linux_android="$BIN/aarch64-linux-android${API}-clang++"
export AR_aarch64_linux_android="$BIN/llvm-ar"
CLANGXX="$BIN/aarch64-linux-android${API}-clang++"
LLVM_AR="$BIN/llvm-ar"

echo "=== Android 交叉编译 ==="
echo "NDK      : $NDK ($HOST_TAG)"
echo "clang    : $($CLANG --version | head -1)"
echo "target   : aarch64-linux-android (API $API)"
echo "profile  : $PROFILE"
echo

cd "$ROOT"
FLAG=""
[ "$PROFILE" = release ] && FLAG="--release"

env \
  "CC_aarch64-linux-android=$CLANG" \
  "CXX_aarch64-linux-android=$CLANGXX" \
  "AR_aarch64-linux-android=$LLVM_AR" \
  cargo build $FLAG --target aarch64-linux-android -p ne-agent 2>&1 | tail -40
rc=${PIPESTATUS[0]}
echo
echo "cargo 退出码 = $rc"

BIN_OUT="$ROOT/target/aarch64-linux-android/$PROFILE/ne-agent"
if [ -f "$BIN_OUT" ]; then
  echo "=== 产物 ==="
  ls -lh "$BIN_OUT"
  file "$BIN_OUT" 2>/dev/null || true
  echo "--- 需要动态库（readelf）---"
  "$BIN/llvm-readelf" -d "$BIN_OUT" 2>/dev/null | grep NEEDED || true
  echo "--- 单二进制检查（应仅 bionic 标准库）---"
  if "$BIN/llvm-readelf" -d "$BIN_OUT" 2>/dev/null | grep NEEDED | grep -qvE 'libc\.so|libm\.so|libdl\.so|liblog\.so|libgcc'; then
    echo "⚠ 存在额外依赖"
  else
    echo "✓ 仅 Android 标准库（libc/libm/libdl/liblog）"
  fi
else
  echo "✗ 未产出二进制"
  exit 1
fi
exit $rc
