#!/usr/bin/env bash
set -uo pipefail
cd /root/novel-system
LIB='/sdcard/Download/小说相关备份/projects/projects/系统/资料库'
B=./target/release/ne-agent
{
  echo "===== 真库 顺序生成 2-30 开始 ====="; date '+%F %T'
  FAIL=0
  for ch in $(seq 2 30); do
    echo "--- 第${ch}章 $(date +%T) ---"
    $B --work-root "$LIB" novel --lib "$LIB" --chap-id $ch --volume 卷01 > ".ci/real_ch${ch}.out" 2>&1
    rc=$?
    head -1 ".ci/real_ch${ch}.out"
    grep -E '记忆回写|已存档|冲突|错误' ".ci/real_ch${ch}.out" | tail -3
    if [ $rc -ne 0 ]; then echo "第${ch}章失败 rc=$rc"; FAIL=$rc; break; fi
  done
  echo "===== 完成 fail=$FAIL ====="; date '+%F %T'
  echo "RESULT=REAL230_DONE fail=$FAIL"
} > .ci/real_2_30.log 2>&1
