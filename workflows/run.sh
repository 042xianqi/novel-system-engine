#!/usr/bin/env bash
# ne-agent 工作流统一入口 —— 太墟工作流 JSON 与沙箱后台都走这一个入口。
#
# 用法：
#   bash workflows/run.sh <名字> [参数...]
#   bash workflows/run.sh --list
#
# 设计（用户 2026-10-11 指示）：
#   * **脚本是唯一真源**：真正逻辑全在 workflows/scripts/*.sh；
#   * **太墟工作流 JSON 只写一行文本**：`bash workflows/run.sh <名字>`，
#     不在 JSON 里内联 bash（内联会导致"UI 一份、脚本一份"两处漂移）；
#   * **可后台**：全程非交互、失败即停、日志落盘，能无人值守跑完。
#
# 后台运行（由 process 工具托管，不加 nohup/&）：
#   bash workflows/run.sh ne_push
# 前台等价命令（人在终端时）：同上。
#
# 环境变量：
#   WF_ROOT      仓库根（默认自动推断）
#   WF_LOG_DIR   日志目录（默认 <repo>/.wf/logs）
#   NE_COMMIT_MESSAGE  ne_commit 的提交说明（不传则用默认）

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export WF_ROOT="${WF_ROOT:-$(cd "$SCRIPT_DIR/.." && pwd)}"
SCRIPTS="$SCRIPT_DIR/scripts"

# 枚举可用工作流（脚本文件名去掉 .sh）
wf_names() {
  find "$SCRIPTS" -maxdepth 1 -name 'ne_*.sh' -printf '%f\n' | sed 's/\.sh$//' | sort
}

case "${1:-}" in
  ""|-h|--help)
    echo "用法: bash workflows/run.sh <名字> [参数...]"
    echo "      bash workflows/run.sh --list"
    echo
    echo "可用工作流："
    for n in $(wf_names); do
      printf '  %s\n' "$n"
    done
    exit 0
    ;;
  --list)
    wf_names
    exit 0
    ;;
esac

NAME="$1"; shift

# 防注入：名字必须是已存在的脚本
if ! echo "$NAME" | grep -Eq '^ne_[a-z0-9_]+$' || [ ! -f "$SCRIPTS/$NAME.sh" ]; then
  echo "✗ 未知工作流：$NAME" >&2
  echo "  可用：$(wf_names | tr '\n' ' ')" >&2
  exit 2
fi

WF_LOG_DIR="${WF_LOG_DIR:-$WF_ROOT/.wf/logs}"
mkdir -p "$WF_LOG_DIR"
export WF_LOG="$WF_LOG_DIR/$NAME.log"
export WF_NAME="$NAME"

# 全程 tee 到日志（前台可见，后台可查；两路一致）
exec > >(tee "$WF_LOG") 2>&1

echo "=== [${WF_NAME}] 开始 $(date '+%F %T') ==="
echo "脚本: workflows/scripts/$NAME.sh"
echo "参数: $*"
echo "---"

start=$(date +%s)
set +e
bash "$SCRIPTS/$NAME.sh" "$@"
rc=$?
set -e
end=$(date +%s)

echo "---"
if [ $rc -eq 0 ]; then
  echo "=== [${WF_NAME}] ✓ 成功 用时 $((end - start))s $(date '+%F %T') ==="
else
  echo "=== [${WF_NAME}] ✗ 失败 rc=$rc 用时 $((end - start))s $(date '+%F %T') ==="
fi
exit $rc
