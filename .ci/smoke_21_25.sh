#!/usr/bin/env bash
set -uo pipefail
cd /root/novel-system
S=/root/stress_lib
B=./target/release/ne-agent
{
  echo "===== 冒烟验证 21-25 开始 ====="; date '+%F %T'
  for ch in 21 22 23 24 25; do
    echo "--- 第${ch}章 生成 ---"
    $B --work-root "$S" novel --lib "$S" --chap-id $ch --volume 卷01 > .ci/smoke_ch${ch}.out 2>&1
    echo "rc=$?"
    grep -E 'genetate|门禁|verdict|记忆回写|已存档|错误|冲突|panic' .ci/smoke_ch${ch}.out | tail -4
    echo "--- 第${ch}章 审计 ---"
    $B --work-root "$S" audit-material-coverage --from $ch --volume 卷01 --lib "$S" 2>&1 \
      | grep -E '^== |prompt 合计|NOQUERY|EMPTY|锚点表注入|L0 整本|L0.5|L1 叙事|L1x 宏大|护栏'
  done
  echo "===== 冒烟完成 ====="; date '+%F %T'
  echo "RESULT=SMOKE_DONE"
} > .ci/smoke_21_25.log 2>&1
