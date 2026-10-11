#!/usr/bin/env bash
# ne_commit —— 门禁 + 全量测试 + 本地提交。非交互、可后台。
# @name  ne_commit
# @desc  门禁→测试→git commit（提交说明由调用方给，无交互）
# @tags  commit,git
#
# 环境变量：
#   COMMIT_MESSAGE  提交说明（必填；没有则拒绝提交）
#
# 说明：原太墟工作流里有个 HUMAN_APPROVAL 节点，但那需要人在 UI 点。
# 要把"后台可跑"做成一等公民，审批就落在这里：**没有填提交说明 = 不提交**，
# 由人（或上层工作流）在触发前给出 COMMIT_MESSAGE 表达意愿。
source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"
wf_env

if [ -z "${COMMIT_MESSAGE:-}" ]; then
  wf_fail "缺少 COMMIT_MESSAGE（后台提交必须显式给出提交说明，用于替代人工审批）"
fi

wf_step "① 架构门禁"
wf_run python3 scripts/arch_check.py

wf_step "② 全量测试"
LOG="${WF_LOG_DIR:-$WF_ROOT/.wf/logs}/cargo-test.log"
set +e
cargo test --workspace >"$LOG" 2>&1
rc=$?
set -e
tail -8 "$LOG"
wf_cargo_summary "$LOG" || wf_fail "测试未全通过，拒绝提交"
[ $rc -eq 0 ] || wf_fail "cargo test 退出码 $rc"

wf_step "③ 待提交变更"
git status --short
echo "--- 统计 ---"
git diff --stat
git diff --cached --stat

if [ -z "$(git status --short)" ]; then
  wf_info "工作区干净，无变更可提交"
  exit 0
fi

wf_step "④ 创建提交"
git add -A
# 用文件传入说明，避免多行/特殊字符问题
printf '%s\n' "$COMMIT_MESSAGE" > "$WF_LOG_DIR/commit-message.txt"
git commit -F "$WF_LOG_DIR/commit-message.txt"
echo "--- 提交结果 ---"
git log --oneline -1
