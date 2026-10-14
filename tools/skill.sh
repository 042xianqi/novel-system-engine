#!/usr/bin/env bash
# 全能全局后台技能 —— 所有耗时任务后台执行，前台不阻塞。
#
# 底层完全复用已有脚本：skill_ci.sh / skill_commit.sh / stress_test_60ch.sh / import_ltm.py
# 本文件只加上层调度外壳，不修改底层业务。
#
# 与需求文档的差异（已核实，按实况修正）：
#   * 命令名用 ASCII 连字符 `import-ltm`（文档里的 `import‑ltm` 是 U+2011，shell 不识别）
#   * db-sanity 的库是 `<库根>/novel.db`（无 `novel_agent.db`），需传库根
#   * commit 的 stage 参数语义是「引用哪个 CI 日志」，非 job id
set -uo pipefail

SKILL_ROOT=".skill_run"
mkdir -p "${SKILL_ROOT}/jobs" "${SKILL_ROOT}/logs" "${SKILL_ROOT}/tmp"

DEFAULT_LIB="/sdcard/Download/小说相关备份/projects/projects/系统/资料库"
LIB="${NE_AGENT_LIB:-$DEFAULT_LIB}"

gen_job_id() { echo "job_$(date +%Y%m%d_%H%M%S)_$RANDOM"; }

write_job_meta() {
  local jid=$1 jtype=$2 pid=$3 logpath=$4
  cat > "${SKILL_ROOT}/jobs/${jid}.meta" <<META
JOB_ID=${jid}
JOB_TYPE=${jtype}
PID=${pid}
LOG=${logpath}
LIB=${LIB}
CREATE_TS=$(date -Iseconds)
META
}

show_help() {
cat <<EOF
全能后台全局技能 skill.sh
用法: ./tools/skill.sh <command> [options]

Commands:
  ci                 后台完整CI：编译 + 全部单元测试
  stress [库根]      后台压力测试：卷01 1-60章（--use-memory），跨 30/60 章快照点
  import-ltm [库根]  后台素材导入 dry-run 校验
  commit "<msg>" [stage]  后台安全提交（前置校验 CI 日志）
  db-sanity [库根]   后台库健康检查：schema / 表 / system_config / long_term_memory
  list               列出全部后台任务
  status <jobid>     查询任务状态
  wait <jobid>       阻塞等待并打印最终结果
  kill <jobid>       强制终止
  log <jobid>        实时跟踪日志

说明:
  * 耗时任务全部 nohup 后台，前台可继续开发
  * 日志末行固定为 RESULT=... ，便于程序判断
  * stress 会写 60 章正文；**建议传副本库根**避免污染真库
EOF
}

task_ci() {
  local jid; jid=$(gen_job_id)
  local log="${SKILL_ROOT}/logs/${jid}.log"
  nohup ./tools/skill_ci.sh "$jid" > "$log" 2>&1 &
  write_job_meta "$jid" "ci" "$!" "$log"
  echo "✅ CI 后台启动 JOB_ID=${jid}"; echo "   日志: ./tools/skill.sh log ${jid}"
}

task_stress() {
  local lib="${1:-$LIB}"
  local jid; jid=$(gen_job_id)
  local log="${SKILL_ROOT}/logs/${jid}.log"
  nohup ./tools/stress_test_60ch.sh "$lib" > "$log" 2>&1 &
  write_job_meta "$jid" "stress_vol1_60" "$!" "$log"
  echo "✅ 压力测试后台启动 JOB_ID=${jid}（库根: $lib）"
  echo "   日志: ./tools/skill.sh log ${jid}"
}

task_import_ltm() {
  local lib="${1:-$LIB}"
  local jid; jid=$(gen_job_id)
  local log="${SKILL_ROOT}/logs/${jid}.log"
  nohup bash -c "python3 tools/memory/import_ltm.py '$lib' --dry-run; \
    echo '==== dry-run 完成；确认无误后去掉 --dry-run 执行真实导入 ===='; \
    echo 'RESULT=IMPORT_DRYRUN_DONE'" > "$log" 2>&1 &
  write_job_meta "$jid" "import_ltm_dry" "$!" "$log"
  echo "✅ 素材导入(dry-run) 后台启动 JOB_ID=${jid}（库根: $lib）"
  echo "   日志: ./tools/skill.sh log ${jid}"
}

task_commit() {
  local msg="${1:-}" stage="${2:-latest}"
  if [[ -z "$msg" ]]; then
    echo "❌ 需要提交信息，例: ./tools/skill.sh commit \"fix xxx\""; return 1
  fi
  local jid; jid=$(gen_job_id)
  local log="${SKILL_ROOT}/logs/${jid}.log"
  nohup ./tools/skill_commit.sh "$msg" "$stage" > "$log" 2>&1 &
  write_job_meta "$jid" "safe_commit" "$!" "$log"
  echo "✅ 安全提交后台启动 JOB_ID=${jid}（前置校验 CI stage=${stage}）"
  echo "   日志: ./tools/skill.sh log ${jid}"
}

task_db_sanity() {
  local lib="${1:-$LIB}"
  local jid; jid=$(gen_job_id)
  local log="${SKILL_ROOT}/logs/${jid}.log"
  local db="${lib}/novel.db"
  # 环境无 `sqlite3` CLI（已核实）→ 必须用 python3 的 sqlite3 模块，否则全部命令失败。
  nohup env DB_PATH="$db" python3 - <<'PYEOF' > "$log" 2>&1 &
import os, sqlite3, sys
db = os.environ["DB_PATH"]
print("==== 数据库路径 ====")
print(db)
if not os.path.exists(db):
    print("RESULT=DB_MISSING")
    sys.exit(0)
c = sqlite3.connect(db)
want = ['long_term_memory','mid_term_snapshot','short_term_memory','chapter_meta','system_config']
print("==== 记忆子系统 5 表 ====")
have = [r[0] for r in c.execute("SELECT name FROM sqlite_master WHERE type='table'")]
for t in want:
    n = c.execute(f"SELECT count(*) FROM {t}").fetchone()[0] if t in have else None
    print(f"  {t}: " + (f"{n} 行" if n is not None else "❌ 不存在"))
print("总表数:", c.execute("SELECT count(*) FROM sqlite_master WHERE type='table'").fetchone()[0])
print("==== system_config 种子 ====")
for k, v in c.execute("SELECT key,value FROM system_config ORDER BY key"):
    print(f"  {k} = {v}")
print("==== long_term_memory 条目 ====")
for cat, key, ln in c.execute("SELECT category,key,length(content) FROM long_term_memory ORDER BY priority"):
    print(f"  {cat} / {key} ({ln} 字符)")
print("long_term_memory 条数:", c.execute("SELECT count(*) FROM long_term_memory").fetchone()[0])
print("char_state 行数(既有表，应不变):", c.execute("SELECT count(*) FROM char_state").fetchone()[0])
print("RESULT=DB_SANITY_DONE")
PYEOF
  write_job_meta "$jid" "db_sanity" "$!" "$log"
  echo "✅ 库健康校验后台启动 JOB_ID=${jid}（库根: $lib）"
  echo "   日志: ./tools/skill.sh log ${jid}"
}

cmd_list() {
  echo "=== 后台任务列表 ==="
  local found=0
  for f in "${SKILL_ROOT}/jobs/"*.meta; do
    [[ -f "$f" ]] || continue
    found=1
    # shellcheck disable=SC1090
    ( source "$f"
      local stat="RUNNING"
      kill -0 "$PID" 2>/dev/null || stat="FINISHED"
      printf '%s | %-16s | PID:%-7s | %-8s | %s\n' "$JOB_ID" "$JOB_TYPE" "$PID" "$stat" "$LOG" )
  done
  [[ $found -eq 0 ]] && echo "(无任务)"
}

cmd_status() {
  local jid="${1:-}" meta="${SKILL_ROOT}/jobs/${1:-}.meta"
  [[ -n "$jid" && -f "$meta" ]] || { echo "❌ job 不存在: $jid"; return 1; }
  # shellcheck disable=SC1090
  source "$meta"
  local stat="RUNNING"; kill -0 "$PID" 2>/dev/null || stat="FINISHED"
  echo "JOB_ID: $JOB_ID"; echo "TYPE: $JOB_TYPE"; echo "PID: $PID"
  echo "STATUS: $stat"; echo "LOG_FILE: $LOG"
  grep "RESULT=" "$LOG" 2>/dev/null || echo "RESULT: 尚未输出结果标记"
}

cmd_wait() {
  local jid="${1:-}" meta="${SKILL_ROOT}/jobs/${1:-}.meta"
  [[ -n "$jid" && -f "$meta" ]] || { echo "❌ job 不存在: $jid"; return 1; }
  # shellcheck disable=SC1090
  source "$meta"
  echo "阻塞等待 ${jid}（PID ${PID}）..."
  # 后台进程非本 shell 子进程，wait 无效 → 轮询
  while kill -0 "$PID" 2>/dev/null; do sleep 5; done
  echo "==== 任务结束，日志尾部 ===="
  tail -30 "$LOG"
}

cmd_kill() {
  local jid="${1:-}" meta="${SKILL_ROOT}/jobs/${1:-}.meta"
  [[ -n "$jid" && -f "$meta" ]] || { echo "❌ job 不存在: $jid"; return 1; }
  # shellcheck disable=SC1090
  source "$meta"
  kill "$PID" 2>/dev/null; sleep 1; kill -9 "$PID" 2>/dev/null
  echo "✅ 已终止 PID=${PID}"
}

cmd_log() {
  local jid="${1:-}" meta="${SKILL_ROOT}/jobs/${1:-}.meta"
  [[ -n "$jid" && -f "$meta" ]] || { echo "❌ job 不存在: $jid"; return 1; }
  # shellcheck disable=SC1090
  source "$meta"
  tail -f "$LOG"
}

CMD="${1:-}"; shift || true
case "$CMD" in
  ci)          task_ci ;;
  stress)      task_stress "$@" ;;
  import-ltm)  task_import_ltm "$@" ;;
  commit)      task_commit "$@" ;;
  db-sanity)   task_db_sanity "$@" ;;
  list)        cmd_list ;;
  status)      cmd_status "$@" ;;
  wait)        cmd_wait "$@" ;;
  kill)        cmd_kill "$@" ;;
  log)         cmd_log "$@" ;;
  *)           show_help; exit 1 ;;
esac