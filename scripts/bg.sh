#!/usr/bin/env bash
# 通用后台作业 —— 需要等待结果的长任务（编译/测试/推送/联网探测）走这里。
#
# 用法（由 process 工具 start）：
#   bash scripts/bg.sh <name> -- <command...>
#
# 结果落在 /tmp/ne-agent-bg-<name>.log，含耗时与退出码。
# 前台用 `process logs` 或直接读该文件查看进度。
set -u

name="${1:-job}"; shift
[ "${1:-}" = "--" ] && shift

LOG="/tmp/ne-agent-bg-${name}.log"
export PATH=/opt/taixu/toolchains/rust/bin:$PATH

{
  echo "=== [$name] 开始 $(date '+%F %T') ==="
  echo "命令: $*"
  echo "---"
  start=$(date +%s)
  "$@"
  rc=$?
  end=$(date +%s)
  echo "---"
  echo "=== [$name] 结束 rc=$rc 用时 $((end - start))s $(date '+%F %T') ==="
} >"$LOG" 2>&1

exit 0
