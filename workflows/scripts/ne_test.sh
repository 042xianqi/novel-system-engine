#!/usr/bin/env bash
# ne_test —— 全 workspace 测试 + 通过/失败汇总。非交互、可后台。
# @name  ne_test
# @desc  cargo test（全 workspace）并汇总
# @tags  test
source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"
wf_env

LOG="${WF_LOG_DIR:-$WF_ROOT/.wf/logs}/cargo-test.log"

wf_step "① cargo test（全 workspace）"
set +e
cargo test --workspace >"$LOG" 2>&1
rc=$?
set -e
tail -50 "$LOG"
echo "cargo 退出码 = $rc"

wf_step "② 汇总"
# wf_cargo_summary 失败（有 failed）即整个脚本失败
wf_cargo_summary "$LOG"

exit $rc
