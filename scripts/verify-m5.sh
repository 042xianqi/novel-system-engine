#!/usr/bin/env bash
# M5 真机验证：rewind CLI + estimate CLI（用假 LLM 造两轮对话）
set -u
export PATH=/opt/taixu/toolchains/rust/bin:$PATH
cd /workspace/小说系统

echo "=== 构建 ==="
cargo build -q -p ne-agent 2>&1 | tail -3

# 假 LLM（返回纯文本，不调工具）
python3 crates/ne-agent/tests/tool_call_llm.py 8936 &
LLMPID=$!
sleep 1

mkdir -p /tmp/m5-e2e-llm
cat > /tmp/m5-e2e-llm/models.yml <<'YML'
providers:
  demo:
    baseUrl: http://127.0.0.1:8936/v1
    api: openai-completions
    task:
      keyPool:
        - "sk-fake-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    models:
      - id: demo-chat-1
YML
cat > /tmp/m5-e2e-llm/config.yml <<'YML'
modelRoles:
  default: demo/demo-chat-1
YML
export TAIXU_LLM_DIR=/tmp/m5-e2e-llm

BIN=target/debug/ne-agent
V=$(mktemp -d)
$BIN --vault "$V" vault init >/dev/null
SID=$($BIN --vault "$V" --json session list 2>/dev/null | python3 -c "import sys,json;d=json.load(sys.stdin);print(d[0]['id'] if d else '')" 2>/dev/null)

echo "=== 第一轮 chat ==="
$BIN --vault "$V" chat -m "第一轮：介绍残响之河" 2>&1 | tail -3
echo "=== 第二轮 chat ==="
$BIN --vault "$V" chat -m "第二轮：再加点细节" 2>&1 | tail -3

echo
echo "=== session list（两轮应同一会话 = 4 条消息）==="
$BIN --vault "$V" session list 2>&1
echo
echo "=== session show ==="
$BIN --vault "$V" session show 2>&1 | head -10
echo
echo "=== rewind --last-turn（撤掉第二轮 user → 剩第一轮 2 条）==="
$BIN --vault "$V" session rewind --last-turn 2>&1
echo "--- rewind 后 show ---"
$BIN --vault "$V" session show 2>&1 | head -6
echo
echo "=== rewind --back 1 ==="
$BIN --vault "$V" session rewind --back 1 2>&1
$BIN --vault "$V" session show 2>&1 | head -5
echo
echo "=== 库中消息数（应仍是 4，不删）==="
$BIN --vault "$V" --json session list 2>&1 | python3 -c "import sys,json;d=json.load(sys.stdin);print('库中消息数:', d[0]['messages'] if d else 'N/A')"

echo
echo "=== estimate CLI（中文）==="
printf '陈寂睁开眼，冰凉的水灌进领口。锚点已建立。' | $BIN --vault "$V" estimate 2>&1

kill $LLMPID 2>/dev/null
echo "=== DONE ==="
