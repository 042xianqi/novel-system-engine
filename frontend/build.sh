#!/usr/bin/env bash
# build.sh —— 前端一键构建（含依赖自检 + 冒烟）
#
# 为什么要有这个脚本：
#   旧模式下改一行 UI = cargo build + 重启，2 分钟起步。
#   新模式下改 UI 只需 ./build.sh，约 3 秒，且**不用重编 Rust**。
#
# 用法：
#   ./build.sh          构建到 dist/
#   ./build.sh --smoke  构建 + 跑 SSR 冒烟（推荐改完 UI 后跑）
#   ./build.sh --check  只跑冒烟（不重建）
#
# 注意：本脚本**不重启后端**。Rust 侧每次请求都重新读文件，
#      所以 dist 更新后浏览器刷新即可看到新版（无需重启 ne-agent）。

set -u
cd "$(dirname "$0")"

GREEN=$'\033[32m'; RED=$'\033[31m'; YELLOW=$'\033[33m'; DIM=$'\033[2m'; NC=$'\033[0m'
say()  { echo "${GREEN}▸${NC} $*"; }
warn() { echo "${YELLOW}!${NC} $*"; }
die()  { echo "${RED}✗${NC} $*"; exit 1; }

MODE="${1:-build}"

# ── 依赖自检 ──
if [ ! -d node_modules ]; then
  warn "node_modules 不存在，先装依赖（首次约 30 秒）"
  npm install --no-audit --no-fund || die "npm install 失败"
fi

# ── 烟雾测试 ──
if [ "$MODE" = "--check" ] || [ "$MODE" = "--smoke" ]; then
  say "SSR 冒烟（编译所有 .vue + 渲染检查）"
  node ssr-smoke.mjs || die "冒烟失败，先修好再构建"
fi

# ── 构建 ──
if [ "$MODE" != "--check" ]; then
  say "构建 dist/"
  npx vite build || die "vite build 失败"

  echo
  say "产物清单："
  ls -lh dist/assets/ | awk 'NR>1 {printf "     %-34s %s\n", $9, $5}'
  echo
  TOTAL=$(du -sh dist | cut -f1)
  say "dist 总大小 ${TOTAL}"
  echo
  echo "${DIM}提示：Rust 侧每次请求都重读 dist，浏览器刷新即可看到新版。${NC}"
  echo "${DIM}      旧单文件面板仍可通过 /legacy 访问（回滚用）。${NC}"
fi