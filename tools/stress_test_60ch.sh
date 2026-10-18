#!/usr/bin/env bash
# 压力测试：卷01 第 1-60 章，--use-memory 开启，跨过 30 章快照触发点。
#
# 与需求文档的差异（已核实，按实况修正）：
#   * 二进制是 `ne-agent`（不是 `novel`），子命令 `novel`
#   * 章号参数是 `--chap-id`（不是 `--chapter`）
#   * 卷号是 `--volume 卷01`（不是 `--volume 1`）
#   * `--use-memory` 是 bool flag，**不能写 `--use-memory=true`**
#   * 库根必须显式 `--lib <库根>`（默认是 vault workspace，不是本库）
#
# 用法: ./tools/stress_test_60ch.sh [库根]
set -uo pipefail
cd /root/novel-system

LIB="${1:-/sdcard/Download/小说相关备份/projects/projects/系统/资料库}"
BIN=./target/release/ne-agent
LOG_FILE=".ci/stress_vol1_1_60.log"
mkdir -p .ci

{
  echo "===== 压力测试开始 卷01 1-60章 开启记忆子系统 ====="
  date '+%F %T'
  echo "库根: $LIB"

  FAIL=0
  for ch in $(seq 1 60); do
    echo "--- 正在生成 第${ch}章 ---"
    $BIN --work-root "$LIB" novel --lib "$LIB" --chap-id "$ch" --volume 卷01 --use-memory \
      > ".ci/ch_${ch}.stdout" 2>&1
    ret=$?
    # 正文落盘即视为执行成功；门禁非 PASS 不算失败（内容问题非链路问题）
    tail -6 ".ci/ch_${ch}.stdout"
    if [ $ret -ne 0 ]; then
      echo "第${ch}章执行失败(rc=$ret)，退出"
      FAIL=$ret
      break
    fi
  done

  echo
  echo "===== 压力测试完成 (fail=$FAIL) ====="
  echo "请核查："
  echo "1. mid_term_snapshot 是否自动生成（30/60 章触发快照）"
  echo "2. chapter_meta 表 60 条记录完整（原始细纲/压缩细纲/token统计/status）"
  echo "3. short_term_memory 窗口正确裁剪，不无限膨胀"
  echo "4. post_update 实体提取，歧义带警告，无 AI 脑补"
  echo "5. 回滚功能：指定快照 ID 做一次人工回滚测试"
  echo "RESULT=STRESS_DONE fail=$FAIL"
} > "$LOG_FILE" 2>&1
