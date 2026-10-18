#!/usr/bin/env bash
# 后台推送脚本 —— 供 process 工具在后台跑，前台继续工作。
#
# 用法（由 process 工具 start）：
#   bash scripts/push-bg.sh
#
# 行为：
#   * 修 DNS + 载入凭据
#   * 最多重试 3 次（libgit2 之外只走 https；GnuTLS -110 是常见瞬时断连）
#   * 把结果写到 /tmp/ne-agent-push.log（含远端 HEAD 与本地 HEAD 对比）
set -u

LOG=/tmp/ne-agent-push.log
exec >"$LOG" 2>&1
echo "=== push 开始 $(date '+%F %T') ==="

source /attachments/skills/github-upload/scripts/gh-push.sh >/dev/null 2>&1 || true
gh_fix_dns >/dev/null 2>&1 || true

cd /workspace/小说系统 || { echo "✗ 找不到仓库"; exit 1; }
echo "本地 HEAD : $(git rev-parse --short HEAD) $(git log -1 --pretty=%s | head -c 60)"

ok=0
for i in 1 2 3; do
  echo "--- 尝试 $i ---"
  if timeout 120 git push origin main; then ok=1; break; fi
  echo "（失败，3 秒后重试）"
  sleep 3
done

if [ "$ok" = 1 ]; then
  remote=$(git ls-remote origin main 2>/dev/null | awk '{print substr($1,1,7)}')
  echo "✓ push 成功"
  echo "远端 HEAD : $remote"
  echo "本地 HEAD : $(git rev-parse --short HEAD)"
else
  echo "✗ push 失败（3 次）——本地提交完好，稍后手动重跑即可"
fi
echo "=== push 结束 $(date '+%F %T') ==="
