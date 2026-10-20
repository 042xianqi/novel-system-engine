#!/usr/bin/env bash
# verify-deploy.sh —— 部署核对（3 秒判定"改动到底生效没有"）
#
# 【为什么要这个脚本】2026-10-20 事故复盘：
#   连续多轮出现"我构建通过、共享目录一致，但真机看不到改动"。
#   根因：**部署端（Operit 的 8270）不从共享目录取 dist**，它服务的是
#   自己那份旧构建。而我此前每轮只验证到"构建通过 + 同步一致"就收工，
#   漏掉了最后也是最关键的一环 —— 核对**部署端服务的产物 hash**。
#   后来用"三方 hash 对照"才查出真相（GhxoyCt4 vs DxcW72bA）。
#
# 本脚本把那次取证固化成一条命令。
set -uo pipefail
FE="/root/novel-system-refactor/frontend"
SHARED="/sdcard/Download/大脑系统最新源码 /frontend"
PORT="${1:-8270}"

local_hash=$(ls "$FE/dist/assets/" 2>/dev/null | grep -oE '^index-[A-Za-z0-9_-]+\.js' | head -1)
shared_hash=$(ls "$SHARED/dist/assets/" 2>/dev/null | grep -oE '^index-[A-Za-z0-9_-]+\.js' | head -1)
served_hash=$(timeout 10 curl -sS "http://127.0.0.1:$PORT/" 2>/dev/null | grep -oE 'index-[A-Za-z0-9_-]+\.js' | head -1)

printf '┌─ 部署核对 ──────────────────────────────────────────────\n'
printf '│ ① 我构建       : %s\n' "${local_hash:-（无）}"
printf '│ ② 共享目录     : %s\n' "${shared_hash:-（无）}"
printf '│ ③ 端口 %-5s  : %s\n' "$PORT" "${served_hash:-（不可达）}"
printf '└────────────────────────────────────────────────────────\n'

rc=0
[ -n "$local_hash" ] && [ "$local_hash" = "$shared_hash" ] \
  && printf '✓ 构建 → 共享目录：一致\n' \
  || { printf '✗ 构建 → 共享目录：不一致（跑 tools/sync-shared.sh）\n'; rc=2; }

if [ -z "$served_hash" ]; then
  printf '? 端口 %s 不可达，无法判定部署\n' "$PORT"; exit 3
fi
if [ "$served_hash" = "$local_hash" ]; then
  printf '✓✓ 部署已生效 —— 真机看到的就是最新构建\n'
else
  printf '✗✗ 部署【未生效】：端口 %s 服务的是 %s，而我构建的是 %s\n' \
    "$PORT" "$served_hash" "$local_hash"
  printf '    → 不是代码问题，是部署链没接上。请让部署方把\n'
  printf '      %s/dist/ 覆盖到它真正的 dist 路径后重启。\n' "$SHARED"
  rc=1
fi
exit $rc
