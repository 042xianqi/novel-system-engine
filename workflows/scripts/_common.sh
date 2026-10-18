#!/usr/bin/env bash
# 工作流步骤的公共前置 —— 每个 ne_*.sh 开头 `source` 本文件。
#
# 设计：脚本是**唯一真源**（太墟工作流 JSON 只留一行调用）。因此这里只放
# 所有步骤都需要的东西：环境（Rust 工具链 / 仓库根）、日志、失败即停。
# 不含任何交互（后台/无人值守也能跑）。

# shellcheck shell=bash
set -euo pipefail

: "${WF_ROOT:=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
: "${WF_NAME:=manual}"

# 若由 run.sh 调起，加载它的公共库（wf_step/wf_run/...）；否则就地补齐。
if ! command -v wf_step >/dev/null 2>&1; then
  # shellcheck source=../_lib.sh
  source "$WF_ROOT/workflows/_lib.sh"
fi

# 仓库根 + Rust 工具链（TaiXu 自带 1.99.x）
wf_cd_root() { cd "$WF_ROOT"; }

wf_env() {
  wf_cd_root
  wf_rust_env
}
