#!/usr/bin/env bash
# sync-shared.sh —— 把前端产物同步到共享目录（**冲突感知，绝不静默覆盖**）
#
# ══════════════════════════════════════════════════════════════════════════
# 为什么要有这个脚本（2026-10-20 事故复盘）
# ══════════════════════════════════════════════════════════════════════════
# 旧做法是 `cp -a src dist "$D/frontend/"`，它有两个致命问题：
#   ① 单向盲覆盖 —— 协作者（Operit）在共享目录上手改的文件被无声抹掉，
#      而且**没有备份、无法追回**（已真实发生：FloatingBall/TaskBoard 的修复丢失）
#   ② `cp -a` 只覆盖不删除 —— 我删掉的文件在共享目录继续存在（幽灵文件）
#
# 本脚本改成分三步：
#   1) **备份**：凡是被改过的文件先复制到 .sync-backup/<时间戳>/，可追回
#   2) **冲突检测**：目标文件与我上次同步的清单（.sync-manifest）不一致
#      → 说明别人改过 → 默认**拒绝覆盖**并打印冲突清单（可用 --force 强制）
#   3) **同步**：只覆盖"我自己的文件"（与清单一致），并处理我的删除（幽灵文件）
#
# 用法：
#   tools/sync-shared.sh              # 正常同步（有冲突则停下并列出）
#   tools/sync-shared.sh --force      # 强制覆盖（仍会备份）
#   tools/sync-shared.sh --check      # 只检查冲突，不动文件
set -uo pipefail

SRC_ROOT="$(cd "$(dirname "$0")/.." && pwd)/frontend"
DST_ROOT="/sdcard/Download/大脑系统最新源码 /frontend"   # 注意：目录名带尾随空格
MANIFEST="$SRC_ROOT/.sync-manifest"
BACKUP_DIR="$DST_ROOT/.sync-backup"
STAMP="$(date +%Y%m%d_%H%M%S)"
FORCE=0; CHECK=0
for a in "$@"; do
  case "$a" in
    --force) FORCE=1 ;;
    --check) CHECK=1 ;;
  esac
done

[ -d "$SRC_ROOT" ] || { echo "✗ 源目录不存在：$SRC_ROOT"; exit 1; }
[ -d "$DST_ROOT" ] || { echo "✗ 共享目录不存在：$DST_ROOT（注意末尾空格）"; exit 1; }

# 参与同步的路径（相对 frontend/）
PATHS="src dist index.html package.json"

# ── 1) 列出源侧文件（相对路径）─────────────────────────────────────────────
list_src() {
  for p in $PATHS; do
    [ -e "$SRC_ROOT/$p" ] || continue
    if [ -f "$SRC_ROOT/$p" ]; then echo "$p"; else (cd "$SRC_ROOT" && find "$p" -type f); fi
  done | sed 's#^\./##' | sort
}

# ── 2) 冲突检测：目标与我上次同步的 sha 不同 → 别人改过 ──────────────
CONFLICTS=""
NEW_FILES=""
while IFS= read -r rel; do
  s="$SRC_ROOT/$rel"; d="$DST_ROOT/$rel"
  [ -f "$d" ] || { NEW_FILES="$NEW_FILES$rel\n"; continue; }
  if [ -f "$MANIFEST" ]; then
    want=$(grep -F "  $rel" "$MANIFEST" 2>/dev/null | awk '{print $1}' | head -1)
    if [ -n "$want" ]; then
      got=$(sha256sum "$d" | awk '{print $1}')
      [ "$want" != "$got" ] && CONFLICTS="$CONFLICTS$rel\n"
    fi
  fi
done < <(list_src)

if [ -n "$CONFLICTS" ]; then
  printf "⚠ 检测到协作者改动（自上次同步后被修改）：\n"
  printf "$CONFLICTS" | sed 's/^/    /'
  printf "\n  这些文件**不会**被覆盖。处理方式：\n"
  printf "    a) 请改动者把 patch 发来，合入真源仓库后重跑本脚本（推荐）\n"
  printf "    b) 或确认可覆盖：tools/sync-shared.sh --force（仍会先备份到 .sync-backup/）\n"
  [ "$CHECK" = "1" ] && exit 2
  [ "$FORCE" != "1" ] && { echo "✗ 已停下，未改动任何文件。"; exit 2; }
fi
[ "$CHECK" = "1" ] && { echo "✓ 无冲突。"; exit 0; }

# ── 3) 备份 + 同步 ──────────────────────────────────────────────────────
mkdir -p "$BACKUP_DIR/$STAMP"
[ -n "$CONFLICTS" ] && printf "$CONFLICTS" | while IFS= read -r rel; do
  [ -z "$rel" ] && continue
  mkdir -p "$BACKUP_DIR/$STAMP/$(dirname "$rel")"
  cp -a "$DST_ROOT/$rel" "$BACKUP_DIR/$STAMP/$rel"
done

LIST="$(list_src)"
echo "$LIST" | while IFS= read -r rel; do
  [ -z "$rel" ] && continue
  mkdir -p "$DST_ROOT/$(dirname "$rel")"
  cp -a "$SRC_ROOT/$rel" "$DST_ROOT/$rel"
done

# 幽灵清理：上次同步过、这次源侧已删除 → 从目标移除（进备份）
if [ -f "$MANIFEST" ]; then
  awk '{print $2}' "$MANIFEST" | while IFS= read -r rel; do
    [ -z "$rel" ] && continue
    if [ ! -e "$SRC_ROOT/$rel" ] && [ -e "$DST_ROOT/$rel" ]; then
      mkdir -p "$BACKUP_DIR/$STAMP/$(dirname "$rel")"
      cp -a "$DST_ROOT/$rel" "$BACKUP_DIR/$STAMP/$rel"
      rm -f "$DST_ROOT/$rel"
      echo "  ↳ 清理幽灵文件：$rel"
    fi
  done
fi

# 写新清单（源侧 sha256）
: > "$MANIFEST"
while IFS= read -r rel; do
  [ -z "$rel" ] && continue
  sha256sum "$SRC_ROOT/$rel" | awk -v r="$rel" '{print $1"  "r}' >> "$MANIFEST"
done <<< "$LIST"

echo "✓ 同步完成（$(echo "$LIST" | wc -l) 个文件）；清单已更新：$MANIFEST"
[ -d "$BACKUP_DIR/$STAMP" ] && echo "  备份：$BACKUP_DIR/$STAMP（冲突文件与幽灵文件）"
