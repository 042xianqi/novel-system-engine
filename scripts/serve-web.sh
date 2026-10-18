#!/usr/bin/env bash
# 启动 ne-agent WebUI 面板（对话中枢 / 系统视图 / 设置中心）
#
# 【为什么要有这个脚本】
# 2026-10-18 踩过一次坑：直接跑
#     ./target/release/ne-agent world-gov web serve --port 8270
# 时，`lib_root` 会 fallback 到 `vault.workspace()`，也就是
#     /root/novel-system/.neagent/workspace/
# 那里有个 176KB 的 novel.db **没有 materials 表**。
# 后果是聊天里问「星耀那边长啥样」，系统回「库里 0 张卡」——
# 数据明明在，系统说没有，是最伤人的那种假答案。
#
# 正确做法是显式传 `--work-root <资料库根>`，让它连真库。
set +H

LIB="${NOVEL_LIB_ROOT:-/sdcard/Download/小说相关备份/projects/projects/系统/资料库}"
PORT="${NOVEL_WEB_PORT:-8270}"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$REPO/target/release/ne-agent"
LOG="${NOVEL_WEB_LOG:-/root/work/ne_agent_run.log}"

if [ ! -x "$BIN" ]; then
  echo "❌ 找不到可执行文件：$BIN"
  echo "   先编译：cd $REPO && cargo build -p ne-agent --release"
  exit 1
fi

if [ ! -f "$LIB/novel.db" ]; then
  echo "❌ 资料库不存在：$LIB/novel.db"
  echo "   可用 NOVEL_LIB_ROOT=... 指定其他库根"
  exit 1
fi

echo "库根  : $LIB"
echo "端口  : $PORT"
echo "日志  : $LOG"

# 清掉旧进程（否则端口占用，新进程起不来但也不报错）
pkill -f 'world-gov web serve' 2>/dev/null
sleep 1

mkdir -p "$(dirname "$LOG")"
cd "$REPO" || exit 1
nohup "$BIN" --work-root "$LIB" world-gov web serve --port "$PORT" \
  > "$LOG" 2>&1 &

# 轮询就绪（最多约 10 秒）
for i in $(seq 1 20); do
  sleep 0.5
  code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/" 2>/dev/null)
  if [ "$code" = "200" ]; then
    pid=$(pgrep -f 'world-gov web serve' | head -1)
    echo "✅ 已就绪：http://127.0.0.1:$PORT/  (PID $pid)"
    # 顺手确认连的是真库，别再踩"空库"的坑
    if [ -e "/proc/$pid/fd" ]; then
      db=$(ls -l "/proc/$pid/fd" 2>/dev/null | grep -o '/[^ ]*novel\.db$' | head -1)
      echo "   数据库：${db:-未知}"
    fi
    exit 0
  fi
done

echo "❌ 启动超时，查看日志：$LOG"
tail -20 "$LOG"
exit 1
