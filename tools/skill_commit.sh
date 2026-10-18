#!/usr/bin/env bash
# 技能：后台提交 —— 依赖 CI 通过，自动 git add/commit
# 用法: tools/skill_commit.sh "commit message" [required_stage]
# 行为: 若 CI 日志末行不是 RESULT=OK* 则拒绝提交
set -uo pipefail
cd /root/novel-system
MSG="${1:-chore: auto commit}"
STAGE="${2:-latest}"
CI=/root/novel-system/.ci
LOG="$CI/$STAGE.log"
OUT="$CI/commit_$STAGE.log"

{
  echo "=== commit stage: $STAGE ==="
  date '+%F %T'
  if [ ! -f "$LOG" ]; then
    echo "RESULT=COMMIT_SKIP reason=no_ci_log"
    exit 0
  fi
  LAST=$(tail -1 "$LOG")
  if ! printf '%s' "$LAST" | grep -q '^RESULT=OK'; then
    echo "CI 未通过，拒绝提交。末行: $LAST"
    echo "RESULT=COMMIT_SKIP reason=ci_not_ok"
    exit 0
  fi
  echo "CI: $LAST"
  git add -A
  git -c user.name=ne-agent -c user.email=ne-agent@local commit -m "$MSG" >/dev/null 2>&1
  RC=$?
  HASH=$(git rev-parse --short HEAD 2>/dev/null)
  echo "commit_rc=$RC hash=$HASH"
  echo "RESULT=COMMITTED hash=$HASH rc=$RC"
} > "$OUT" 2>&1