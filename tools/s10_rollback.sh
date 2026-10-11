#!/usr/bin/env bash
# S10 回滚脚本 —— 撤销「删 27 行空正文占位 + 归档删除候选目录」。
#
# 用途：S10 前置清理若发现误删，用本脚本一键还原到 pre_s10 备份状态。
# 幂等：可重复执行；已还原状态下再跑只复制文件、不改动。
#
# 用法：
#   tools/s10_rollback.sh                  # 回滚（DB + 目录）
#   tools/s10_rollback.sh --db-only        # 只回滚 DB
#   tools/s10_rollback.sh --dirs-only      # 只回滚目录
#   tools/s10_rollback.sh --dry-run        # 只打印将做的事
set -euo pipefail

R="${S10_ROOT:-/sdcard/Download/小说相关备份/projects/projects/系统/资料库}"
BK="$R/09_冷备份/pre_s10_20261011_0148"

DO_DB=1; DO_DIRS=1; DRY=0
for a in "$@"; do
  case "$a" in
    --db-only)   DO_DIRS=0 ;;
    --dirs-only) DO_DB=0 ;;
    --dry-run)   DRY=1 ;;
    -h|--help)   sed -n '2,14p' "$0"; exit 0 ;;
    *) echo "未知参数: $a" >&2; exit 2 ;;
  esac
done

say() { printf '%s\n' "$*"; }
run() { if [ "$DRY" = 1 ]; then say "  [dry-run] $*"; else eval "$@"; fi; }

[ -d "$BK" ] || { echo "❌ 备份目录不存在: $BK" >&2; exit 1; }

say "== S10 回滚 =="
say "  真值库根: $R"
say "  备份目录: $BK"
say ""

# ── 1. 回滚 DB ────────────────────────────────────────────────
if [ "$DO_DB" = 1 ]; then
  SRC="$BK/novel.db"
  [ -f "$SRC" ] || { echo "❌ 备份 DB 缺失: $SRC" >&2; exit 1; }
  say "1) 回滚 novel.db"
  # 先给"当前库"留个退路，避免回滚本身不可逆
  if [ -f "$R/novel.db" ]; then
    cp -p "$R/novel.db" "$R/novel.db.pre-rollback-$(date +%Y%m%d_%H%M%S)"
    say "   已另存当前库为 novel.db.pre-rollback-*"
  fi
  run "cp -f '$SRC' '$R/novel.db'"
  # WAL/SHM 是旧格式残留，回滚后必须清掉，否则 SQLite 可能重放旧日志
  for f in "$R/novel.db-wal" "$R/novel.db-shm"; do
    [ -e "$f" ] && run "rm -f '$f'"
  done
  say "   ✅ 已还原（该版本应为 materials=959，空正文=27）"
  say ""
fi

# ── 2. 回滚删除候选目录 ────────────────────────────────────────
if [ "$DO_DIRS" = 1 ]; then
  SRC="$BK/deleted_candidates_20261002"
  DST="$R/09_冷备份/删除候选_20261002"
  if [ -d "$SRC" ]; then
    say "2) 归档目录还原"
    run "mv -f '$SRC' '$DST'"
    say "   ✅ $SRC  →  $DST"
  else
    say "2) 归档目录：$SRC 不存在（可能已还原过），跳过"
  fi
  say ""
  # 冗余副本仍在 trash，作为第二保险
  T="/sdcard/Download/_trash_小说备份/from_小说相关备份根/extract/projects/系统/资料库/09_冷备份/删除候选_20261002"
  [ -d "$T" ] && say "   （第二副本仍在: $T）"
fi

say "== 回滚完成 =="
say "校验： python3 -c \"import sqlite3;c=sqlite3.connect('$R/novel.db');"
say "        print(c.execute('select count(*) from materials').fetchone())\"   # 期望 959"
