#!/usr/bin/env bash
# trial_run_controlled.sh —— world-gov 受控试运行（带软限流）
#
# 用途：对资料库做**小批量、可中断、可计量**的真实 LLM 语义核验。
#      逐卡调用（不是一次性 inspect-batch），每卡之间 sleep，便于：
#        * 软限流（避免速率峰值）；
#        * 随时 Ctrl-C 中断（已完成的卡已写 conflict_records，保留）；
#        * 逐卡观察（每卡打印状态/verdict/conflict_id）。
#
# 前置（缺一不可，三重锁）：
#   1) cargo build --release --features llm_integration
#   2) export WOGOV_ALLOW_REAL_LLM=yes
#   3) 本脚本用 `inspect-card --real-semantic` 充当命令行锁
#
# 用法：
#   tools/trial_run_controlled.sh [起始序号] [条数] [间隔秒] [key文件]
#   例：tools/trial_run_controlled.sh 0 10 10 /tmp/slice14_keys.txt
#   （key文件每行一个 key；不给则用内置默认 1/4 片 key 清单文件）
set -uo pipefail

WORK_ROOT="${WORK_ROOT:-/sdcard/Download/小说相关备份/projects/projects/系统/资料库}"
B="${B:-./target/release/ne-agent}"
START="${1:-0}"
COUNT="${2:-10}"
SLEEP_SEC="${3:-10}"
KEYS_FILE="${4:-/tmp/slice14_keys.txt}"

if [ ! -x "$B" ]; then
  echo "[fatal] 二进制不存在：$B（先 cargo build --release --features llm_integration）" >&2
  exit 1
fi
if [ ! -f "$KEYS_FILE" ]; then
  echo "[fatal] key 文件不存在：$KEYS_FILE" >&2
  echo "       生成：$B --work-root \"\$WORK_ROOT\" world-gov inspect-batch --slice 1/4 --dry-run --json | python3 -c 'import json,sys;print(chr(10).join(json.load(sys.stdin)[\"keys\"]))' > $KEYS_FILE" >&2
  exit 1
fi

# 三重锁第 2 条：环境变量
if [ "${WOGOV_ALLOW_REAL_LLM:-}" != "yes" ]; then
  echo "[fatal] 需 export WOGOV_ALLOW_REAL_LLM=yes（三重锁未开，绝不外呼）" >&2
  exit 1
fi

echo "=== world-gov 受控试运行 ==="
echo "WORK_ROOT : $WORK_ROOT"
echo "起始/条数  : $START / $COUNT"
echo "间隔(秒)   : $SLEEP_SEC"
echo "key 文件   : $KEYS_FILE"
echo

ok=0; fail=0; n=0
while IFS= read -r key; do
  [ -z "$key" ] && continue
  n=$((n+1))
  if [ "$n" -le "$START" ]; then continue; fi
  if [ "$((n-START))" -gt "$COUNT" ]; then break; fi

  printf '[%2d] %-28s ' "$n" "$key"
  out=$("$B" --work-root "$WORK_ROOT" world-gov inspect-card --key "$key" \
        --semantic --real-semantic --json 2>/dev/null)
  rc=$?
  if [ "$rc" -ne 0 ] || [ -z "$out" ]; then
    echo "ERR(rc=$rc)"; fail=$((fail+1))
  else
    st=$(printf '%s' "$out" | python3 -c 'import json,sys;d=json.load(sys.stdin);print(d.get("status","?"),"| verdict=",(d.get("verdict") or {}).get("verdict") if isinstance(d.get("verdict"),dict) else d.get("verdict"),"| conflict=",d.get("conflict_id"))' 2>/dev/null || echo "PARSE_ERR")
    echo "$st"
    case "$st" in
      *MALFORMED*|*GUARD_REJECTED*|*BACKEND_ERROR*|PARSE_ERR) fail=$((fail+1));;
      *) ok=$((ok+1));;
    esac
  fi
  sleep "$SLEEP_SEC"   # 软限流
done < "$KEYS_FILE"

echo
echo "=== 本轮汇总：ok=$ok fail=$fail ==="
echo "--- LLM 记账（速率刹车参考）---"
"$B" --work-root "$WORK_ROOT" world-gov llm stats --json 2>/dev/null \
  | python3 -c 'import json,sys;d=json.load(sys.stdin);print("calls",d["total_calls"],"fail",d["total_fail"],"cache_hits",d["cache_hits"],"token_in",d["token_in_total"],"token_out",d["token_out_total"])'
echo "--- 冲突库 ---"
"$B" --work-root "$WORK_ROOT" world-gov conflict summary --json 2>/dev/null \
  | python3 -c 'import json,sys;d=json.load(sys.stdin);print("total",d["total"],d["by_review_status"])'
