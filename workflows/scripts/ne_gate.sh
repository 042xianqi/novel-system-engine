#!/usr/bin/env bash
# ne_gate —— 架构门禁（单文件行数上限等）。非交互、可后台。
# @name  ne_gate
# @desc  跑 architecture-policy 门禁
# @tags  gate,arch
source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"
wf_env

wf_step "① 架构门禁"
wf_run python3 scripts/arch_check.py
