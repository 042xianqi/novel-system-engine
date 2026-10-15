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

  # --- 第 19 批（P9.1 守卫）：无 feature 构建会覆盖 workspace（含 ne-mcp）的 release 产物，
  # 导致三重锁第一锁 compile_feature=false。CI 结束前把产物恢复为带 feature 版，避免
  # 「CI 跑完 → 真实外呼静默失败」。恢复失败不改 RESULT（只告警），但日志留痕。
  echo "--- restore-feature ---"
  RF_OUT=$(cargo build --release --features llm_integration 2>&1)
  RF_ERR=$(printf '%s\n' "$RF_OUT" | grep -cE '^error')
  if [ "$RF_ERR" -ne 0 ]; then
    echo "feature_restore=FAIL errors=$RF_ERR（请注意：release 产物当前为无 feature 版本）"
    printf '%s\n' "$RF_OUT" | grep -E '^error' -A5 | head -30
  else
    echo "feature_restore=OK（release 产物已为 llm_integration 版，三重锁第一锁可用）"
  fi
} > "$LOG" 2>&1
