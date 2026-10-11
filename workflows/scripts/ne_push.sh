#!/usr/bin/env bash
# ne_push —— 推送 origin/<branch>，含连通探测 + 重试。非交互、可后台。
# @name  ne_push
# @desc  推送到 origin/main（DNS 修复 + 连通探测 + 3 次重试）
# @tags  push,git
#
# 环境变量：
#   BRANCH  目标分支（默认 main）
source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"
wf_env

BRANCH="${BRANCH:-main}"

wf_step "① 本地状态"
git log --oneline -1
echo "待推送："
git log "origin/${BRANCH}..HEAD" --oneline 2>/dev/null | head -20 || echo "  （无法比较远端，继续尝试）"

wf_step "② 远端连通探测 / DNS"
# 网络抖动的已知根因之一是 github.com 解析到坏 IP；这里是幂等的修复尝试。
if ! timeout 12 curl -sI https://github.com >/dev/null 2>&1; then
  wf_warn "github 不可达，尝试 DNS 修复"
  # shellcheck disable=SC1091
  source /attachments/skills/github-upload/scripts/gh-push.sh >/dev/null 2>&1 || true
  command -v gh_fix_dns >/dev/null 2>&1 && gh_fix_dns >/dev/null 2>&1 || true
fi
i=1
while [ $i -le 10 ]; do
  if timeout 15 curl -sI https://github.com >/dev/null 2>&1; then
    echo "✓ github 可达"
    break
  fi
  echo "  探测 $i 不通，3s 后重试"
  sleep 3
  i=$((i + 1))
done

wf_step "③ git push（最多 3 次）"
n=1
ok=0
while [ $n -le 3 ]; do
  echo "--- 推送尝试 $n ---"
  if git push origin "$BRANCH" 2>&1; then ok=1; break; fi
  echo "  （失败，3s 后重试）"
  sleep 3
  n=$((n + 1))
done

if [ "$ok" != 1 ]; then
  wf_fail "推送失败（3 次）——本地提交完好，可稍后重跑 ne_push"
fi

wf_step "④ 校验一致"
local_head="$(git rev-parse --short HEAD)"
remote_head="$(git ls-remote origin "$BRANCH" 2>/dev/null | awk '{print substr($1,1,7)}')"
echo "本地 HEAD : $local_head"
echo "远端 HEAD : ${remote_head:-（读取失败）}"
if [ -n "$remote_head" ] && [ "$local_head" != "$remote_head" ]; then
  wf_fail "推送后本地与远端 HEAD 不一致（$local_head vs $remote_head）"
fi
echo "✓ 同步一致"
