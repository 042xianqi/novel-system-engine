#!/usr/bin/env bash
# ne_verify —— 门禁 + 测试 + release 编译 + 产物体检。非交互、可后台。
# @name  ne_verify
# @desc  门禁→测试→release→产物体检（发布前一条龙）
# @tags  verify,release
source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"
wf_env

wf_step "① 架构门禁"
wf_run python3 scripts/arch_check.py

wf_step "② 全量测试"
LOG="${WF_LOG_DIR:-$WF_ROOT/.wf/logs}/cargo-test.log"
set +e
cargo test --workspace >"$LOG" 2>&1
rc=$?
set -e
tail -20 "$LOG"
wf_cargo_summary "$LOG" || wf_fail "测试未全通过"
[ $rc -eq 0 ] || wf_fail "cargo test 退出码 $rc"

wf_step "③ release 编译"
wf_run cargo build --release -p ne-agent

wf_step "④ 产物体检"
BIN="target/release/ne-agent"
wf_have "$BIN"
ls -lh "$BIN"
"$BIN" version
echo "动态依赖："
ldd "$BIN" 2>/dev/null | grep '=>' | awk '{print "  " $1}' || true

wf_step "⑤ 单二进制检查（应仅标准库）"
if command -v readelf >/dev/null 2>&1; then
  readelf -d "$BIN" | grep NEEDED || true
fi
