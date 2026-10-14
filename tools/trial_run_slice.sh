#!/bin/bash
# trial_run_slice.sh —— world-gov 分片受控试运行（逐卡 + 软限流 + 可中断）
#
# 设计要点（对齐用户决策）：
#   * **不在进程内一口气轰完 164 条**：shell 层逐卡调用 `inspect-card`，
#     Ctrl-C 随时停，已跑完的卡已写 conflict_records，**持久保留**。
#   * 每卡之间**随机休眠 8~12s** 做软限流，控制 token 速率。
#   * `--limit N`（或位置参数 $1）限制最大跑卡数，适合逐步放量。
#   * 每轮自动打基线快照；结束输出审计汇总（conflict/llm stats/cache）。
#
# 前置（三重锁，缺一不可）：
#   1) cargo build --release --features llm_integration
#   2) export WOGOV_ALLOW_REAL_LLM=yes
#   3) 逐卡 `inspect-card --real-semantic` 充当命令行锁
#
# 用法：
#   tools/trial_run_slice.sh [LIMIT] [SLICE] [KEY文件]
#   例：tools/trial_run_slice.sh 20            # 1/4 片，最多跑 20 张
#       tools/trial_run_slice.sh 9999          # 1/4 片，全片（≈164 张，预估 ~2h）
set -euo pipefail

export WOGOV_ALLOW_REAL_LLM=yes
WORK_ROOT="${WORK_ROOT:-/sdcard/Download/小说相关备份/projects/projects/系统/资料库}"
B="${B:-./target/release/ne-agent}"
LIMIT="${1:-9999}"
SLICE="${2:-1/4}"
KEYS_FILE_ARG="${3:-}"
SLEEP_MIN=8
SLEEP_MAX=12

if [ ! -x "$B" ]; then
  echo "[fatal] 二进制不存在：$B（先 cargo build --release --features llm_integration）" >&2
  exit 1
fi

echo "=== 受控分片试运行 | slice=$SLICE | limit=$LIMIT | sleep:${SLEEP_MIN}~${SLEEP_MAX}s ==="
"$B" --work-root "$WORK_ROOT" sys-check full --json

# 取出分片内全部卡片 key（--list-keys：每行一个，纯文本）
KEYS_FILE="${KEYS_FILE_ARG:-$(mktemp)}"
if [ -z "$KEYS_FILE_ARG" ]; then
  "$B" --work-root "$WORK_ROOT" world-gov inspect-batch --slice "$SLICE" --dry-run --list-keys > "$KEYS_FILE"
fi
TOTAL=$(wc -l < "$KEYS_FILE" | tr -d ' ')
echo "分片 $SLICE 共 $TOTAL 张卡，最多跑 $LIMIT 张"

# 基线快照
SNAP_ID=$("$B" --work-root "$WORK_ROOT" snap create --label "slice-trial-$(date +%s)" --json \
          | python3 -c 'import json,sys;print(json.load(sys.stdin)["snap_id"])')
echo "基线快照: $SNAP_ID"

COUNT=0
OK=0
FAIL=0
while IFS= read -r KEY; do
    [ -z "$KEY" ] && continue
    [ "$COUNT" -ge "$LIMIT" ] && break
    COUNT=$((COUNT + 1))     # 注意：用 $(( )) 而非 (( )) —— 后者在 COUNT=0 时返回非零会被 set -e 杀掉
    echo -e "\n[$COUNT] 处理卡片: $KEY"

    # 逐卡真实核验；非 0 退出码不中止整轮（记 FAIL 继续）
    # 注意：必须 `--semantic --real-semantic` 双开关——只给 real-semantic 会停在 PROBE_READY（不推理）
    # DRY_RUN=1 时不真实外呼（用 dry_run_mock 后端跑同一条链路），用于无网预演调度/休眠/审计。
    RC=0
    if [ "${DRY_RUN:-0}" = "1" ]; then
        OUT=$("$B" --work-root "$WORK_ROOT" world-gov inspect-card --key "$KEY" --semantic --json 2>/dev/null) || RC=$?
    else
        OUT=$("$B" --work-root "$WORK_ROOT" world-gov inspect-card --key "$KEY" --semantic --real-semantic --json 2>/dev/null) || RC=$?
    fi
    if [ "$RC" -ne 0 ] || [ -z "$OUT" ]; then
        echo "⚠️  $KEY 返回非 0 退出码（rc=$RC），继续下一张"
        FAIL=$((FAIL + 1))
    else
        ST=$(printf '%s' "$OUT" | python3 -c 'import json,sys;d=json.load(sys.stdin);print(d.get("status","?"),"conflict=",d.get("conflict_id"))' 2>/dev/null || echo "PARSE_ERR")
        echo "    $ST"
        case "$ST" in
            *MALFORMED*|*GUARD_REJECTED*|*BACKEND_ERROR*|PARSE_ERR) FAIL=$((FAIL + 1));;
            *) OK=$((OK + 1));;
        esac
    fi

    # 随机休眠限流（末张后不必等）
    if [ "$COUNT" -lt "$LIMIT" ]; then
        DELAY=$(( SLEEP_MIN + RANDOM % (SLEEP_MAX - SLEEP_MIN + 1) ))
        echo "→ 休眠 ${DELAY}s …"
        sleep "$DELAY"
    fi
done < "$KEYS_FILE"

[ -z "$KEYS_FILE_ARG" ] && rm -f "$KEYS_FILE"

echo -e "\n=== 试运行结束（ok=$OK fail=$FAIL / 共 $COUNT）审计 ==="
"$B" --work-root "$WORK_ROOT" world-gov conflict summary --json
"$B" --work-root "$WORK_ROOT" world-gov llm stats --json
"$B" --work-root "$WORK_ROOT" world-gov llm cache status --json

echo "提示：全片放量去掉 LIMIT（或给大值 9999）；随时 Ctrl-C 中断，已完成结果不丢失。"