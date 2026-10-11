#!/usr/bin/env bash
# 工作流公共库 —— 被 workflows/*.sh 每个步骤 `source`。
#
# 目的：把"编译/测试/门禁/提交/推送/验证"这些**每次都要手敲、容易漏步**的操作
# 固化成一致、可发现、可后台跑、留日志的工作流。
#
# 约定：
#   * 工作流文件开头用 `# @name` / `# @desc` / `# @tags` 声明元信息（runner 解析）
#   * 工作流主体用 `wf_run <cmd...>` 执行每个步骤：会打印步骤头、失败即退出
#   * 一律 `set -euo pipefail`，任何一步失败即整体失败（后台任务不能"假装成功"）
#
# 环境变量（runner 注入）：
#   WF_NAME      当前工作流名
#   WF_ROOT      仓库根
#   WF_RUN_DIR   runner 的运行目录（日志/状态）
#   WF_LOG       本工作流的日志文件路径

set -euo pipefail

: "${WF_ROOT:=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
: "${WF_NAME:=manual}"
: "${WF_RUN_DIR:=$WF_ROOT/.wf}"

# 颜色（非 tty 自动关闭）
if [ -t 1 ]; then
  _C_DIM=$'\033[2m'; _C_B=$'\033[1m'; _C_G=$'\033[32m'
  _C_R=$'\033[31m'; _C_Y=$'\033[33m'; _C_0=$'\033[0m'
else
  _C_DIM=; _C_B=; _C_G=; _C_R=; _C_Y=; _C_0=
fi

wf_info() { printf '%s\n' "${_C_DIM}· $*${_C_0}"; }
wf_warn() { printf '%s\n' "${_C_Y}▲ $*${_C_0}" >&2; }
wf_fail() { printf '%s\n' "${_C_R}✗ $*${_C_0}" >&2; exit 1; }

# 步骤标题
wf_step() {
  printf '\n%s\n' "${_C_B}== $* ==${_C_0}"
}

# 检查命令存在
wf_need() {
  command -v "$1" >/dev/null 2>&1 || wf_fail "缺少命令：$1"
}

# 执行一步（打印后执行；失败即退出，不吞错误）
wf_run() {
  printf '%s\n' "${_C_DIM}\$ $*${_C_0}"
  "$@"
}

# 断言文件存在
wf_have() {
  [ -e "$1" ] || wf_fail "预期存在但找不到：$1"
}

# 进入仓库根（多数工作流都从根跑）
wf_cd_root() { cd "$WF_ROOT"; }

# 确保 rust 在 PATH（TaiXu 工具链）
wf_rust_env() {
  export PATH="/opt/taixu/toolchains/rust/bin:${PATH}"
}

# 汇总 cargo test 输出里的通过/失败数（读 stdin 的文件路径）
wf_cargo_summary() {
  local f="$1"
  python3 - "$f" <<'PY'
import re, sys
t = open(sys.argv[1], encoding="utf-8", errors="replace").read()
tot = sum(int(m) for m in re.findall(r"test result: ok\. (\d+) passed", t))
fail = sum(int(m) for m in re.findall(r"(\d+) failed", t))
ign = sum(int(m) for m in re.findall(r"(\d+) ignored", t))
print(f"总通过 {tot} 失败 {fail} 忽略 {ign}")
sys.exit(1 if fail else 0)
PY
}
