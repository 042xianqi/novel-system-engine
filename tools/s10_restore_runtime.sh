#!/usr/bin/env bash
# S10 后运行时目录恢复 —— 把流水线**运行所需的输入**从冷备份放回 live 根。
#
# 背景：S10 已判定 `02_资料库` 的卡片正文 100% 入 DB，故**不再**恢复它。
#       但以下两样是 ne_inject / ne_cardgen / funnel 的**运行时输入**，DB 里没有：
#         · 03_叙事规划/  —— 1800 章细纲（10 卷 × 180 章）。⚠ **不在 DB**（chapters 表仅 2 行）
#         · 05_管线框架/  —— funnel.yaml（funnel 工具读它）
#
# 用法：
#   tools/s10_restore_runtime.sh            # 恢复到 live 根（幂等，已存在则跳过）
#   tools/s10_restore_runtime.sh --force    # 覆盖恢复
#   tools/s10_restore_runtime.sh --dry-run
set -euo pipefail

R="${S10_ROOT:-/sdcard/Download/小说相关备份/projects/projects/系统/资料库}"
BK="${S10_BACKUP:-$R/09_冷备份/pre_renumber_20261010_1505}"

FORCE=0; DRY=0
for a in "$@"; do
  case "$a" in
    --force) FORCE=1 ;;
    --dry-run) DRY=1 ;;
    -h|--help) sed -n '2,15p' "$0"; exit 0 ;;
    *) echo "未知参数: $a" >&2; exit 2 ;;
  esac
done

say() { printf '%s\n' "$*"; }
run() { if [ "$DRY" = 1 ]; then say "  [dry-run] $*"; else eval "$@"; fi; }

[ -d "$BK" ] || { echo "❌ 冷备份不存在: $BK" >&2; exit 1; }

say "== 恢复运行时目录 =="
say "  live 根 : $R"
say "  冷备份  : $BK"
say ""

# 只恢复「运行时输入」；02_资料库 明确不恢复（卡片已在 DB）
for d in 03_叙事规划 05_管线框架; do
  SRC="$BK/$d"
  DST="$R/$d"
  if [ ! -d "$SRC" ]; then say "⚠ 备份缺 $d，跳过"; continue; fi
  if [ -d "$DST" ] && [ "$FORCE" = 0 ]; then
    say "• $d 已存在，跳过（--force 可覆盖）"
    continue
  fi
  [ -d "$DST" ] && run "rm -rf '$DST'"
  run "cp -rp '$SRC' '$DST'"
  say "• $d ✅ 已恢复"
done

say ""
say "== 校验 =="
[ -f "$R/05_管线框架/funnel.yaml" ] && say "  funnel.yaml ✅ 在位" || say "  funnel.yaml ❌ 缺失"
if [ -d "$R/03_叙事规划/细纲" ]; then
  n=$(grep -c '^### 第' "$R"/03_叙事规划/细纲/卷01.md 2>/dev/null || echo 0)
  say "  细纲/卷01.md ✅ 在位（$n 章）"
else
  say "  细纲 ❌ 缺失"
fi
say "  02_资料库 ：**故意不恢复**（卡片正文已在 novel.db 的 materials）"
