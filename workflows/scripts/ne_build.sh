#!/usr/bin/env bash
# ne_build —— 编译 ne-agent（debug/release，可指定包）。非交互、可后台。
# @name  ne_build
# @desc  cargo build（默认 -p ne-agent, debug）
# @tags  build
#
# 可选环境变量：
#   TARGET   包名（默认 ne-agent）
#   PROFILE  debug|release（默认 debug）
source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"
wf_env

TARGET="${TARGET:-ne-agent}"
PROFILE="${PROFILE:-debug}"

wf_step "① 环境自检"
uname -m
rustc -V
cargo -V
echo "仓库: $(pwd)  HEAD: $(git rev-parse --short HEAD 2>/dev/null || echo '(无)')"

wf_step "② cargo build ($TARGET / $PROFILE)"
if [ "$PROFILE" = release ]; then
  wf_run cargo build --release -p "$TARGET"
else
  wf_run cargo build -p "$TARGET"
fi

wf_step "③ 产物信息"
BIN="target/$PROFILE/ne-agent"
wf_have "$BIN"
ls -lh "$BIN"
echo "动态依赖："
ldd "$BIN" 2>/dev/null | grep '=>' | awk '{print "  " $1}' || true
"$BIN" version 2>/dev/null || true
