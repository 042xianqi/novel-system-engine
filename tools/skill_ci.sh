#!/usr/bin/env bash
# 技能：后台 CI —— 编译 + 测试，结果写日志（供前台轮询读取）
# 用法: tools/skill_ci.sh [stage_name]
# 输出: /root/novel-system/.ci/<stage>.log ，末行固定为 RESULT=...
set -uo pipefail
cd /root/novel-system
STAGE="${1:-latest}"
CI=/root/novel-system/.ci
mkdir -p "$CI"
LOG="$CI/$STAGE.log"

{
  echo "=== stage: $STAGE ==="
  date '+%F %T'
  echo "--- build ---"
  BUILD_OUT=$(cargo build --release 2>&1)
  ERR=$(printf '%s\n' "$BUILD_OUT" | grep -cE '^error')
  printf '%s\n' "$BUILD_OUT" | grep -E '^error' -A5 | head -60
  echo "build_errors=$ERR"

  if [ "$ERR" -ne 0 ]; then
    echo "RESULT=BUILD_FAIL errors=$ERR"
    exit 0
  fi

  echo "--- test ---"
  TEST_OUT=$(cargo test --release 2>&1)
  PASSED=$(printf '%s\n' "$TEST_OUT" | grep -oE '[0-9]+ passed' | awk '{s+=$1} END {print s+0}')
  FAILED=$(printf '%s\n' "$TEST_OUT" | grep -oE '[0-9]+ failed' | awk '{s+=$1} END {print s+0}')
  printf '%s\n' "$TEST_OUT" | grep -E 'FAILED|panicked' -A4 | head -40
  echo "passed=$PASSED failed=$FAILED"
  if [ "$FAILED" -ne 0 ] || [ "$PASSED" -eq 0 ]; then
    echo "RESULT=TEST_FAIL passed=$PASSED failed=$FAILED"
  else
    echo "RESULT=OK passed=$PASSED failed=$FAILED"
  fi
} > "$LOG" 2>&1
