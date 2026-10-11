#!/usr/bin/env bash
# 单二进制"拎包走"验证：把 release 二进制拷到干净目录，仅凭 yaml 跑通完整流程。
# 证明：无脚本依赖、无工作区依赖、无源码依赖。
set -u

SRC=/workspace/小说系统/target/release/ne-agent
[ -x "$SRC" ] || { echo "先 cargo build --release -p ne-agent"; exit 1; }

DEST=$(mktemp -d /tmp/ne-agent-portable-XXXX)
echo "=== 干净环境：$DEST ==="
cp "$SRC" "$DEST/ne-agent"
cd "$DEST"

BIN="$DEST/ne-agent"
export TAIXU_LLM_DIR="$DEST/llm"
mkdir -p "$DEST/llm"

# 假 LLM（回显式，不调工具）
python3 - <<'PY' &
import http.server, json
class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        n=int(self.headers.get('Content-Length',0))
        b=json.loads(self.rfile.read(n) or b'{}')
        last=(b.get('messages') or [{}])[-1].get('content','')
        r={"choices":[{"message":{"role":"assistant","content":"回声："+last[:80]},"finish_reason":"stop"}]}
        d=json.dumps(r).encode()
        self.send_response(200); self.send_header('Content-Type','application/json')
        self.send_header('Content-Length',str(len(d))); self.end_headers(); self.wfile.write(d)
    def log_message(self,*a): pass
http.server.HTTPServer(('127.0.0.1',9931),H).serve_forever()
PY
LLM=$!
sleep 1

cat > "$DEST/llm/models.yml" <<'YML'
providers:
  demo:
    baseUrl: http://127.0.0.1:9931/v1
    api: openai-completions
    task:
      keyPool:
        - "sk-fake-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    models:
      - id: demo-chat-1
YML
cat > "$DEST/llm/config.yml" <<'YML'
modelRoles:
  default: demo/demo-chat-1
YML

echo
echo "=== 1. --version（单二进制自报）==="
"$BIN" --version 2>&1 || "$BIN" version 2>&1
VER=$("$BIN" version 2>&1)
echo "$VER" | grep -q "0.0.5" && echo "✓ 版本号正确" || echo "⚠ 版本号未同步"

echo
echo "=== 2. vault init（自建项目库）==="
"$BIN" --vault "$DEST/v" vault init 2>&1 | head -6

echo
echo "=== 3. vault show（自检）==="
"$BIN" --vault "$DEST/v" vault show 2>&1 | head -8

echo
echo "=== 4. chat（真跑一轮，用假 LLM）==="
"$BIN" --vault "$DEST/v" chat -m "你好，介绍一下你自己" 2>&1 | tail -4

echo
echo "=== 5. estimate（M5）==="
printf '陈寂睁开眼，冰凉的水灌进领口。' | "$BIN" --vault "$DEST/v" estimate 2>&1

echo
echo "=== 6. task new + list（M6）==="
"$BIN" --vault "$DEST/v" task new --session demo "写完整卷" 2>&1
"$BIN" --vault "$DEST/v" task list 2>&1 | head -4

echo
echo "=== 7. session list + rewind（M5）==="
"$BIN" --vault "$DEST/v" session list 2>&1 | head -3
"$BIN" --vault "$DEST/v" session rewind --last-turn 2>&1 | head -3

echo
echo "=== 8. 确认无外部脚本依赖（ldd）==="
ldd "$BIN" 2>/dev/null | grep -c "=>" | xargs echo "动态库数量（应 ≤ 4）："
ldd "$BIN" 2>/dev/null | grep -vE "libc|libm|libgcc|ld-linux" | grep "=>" && echo "⚠ 有额外依赖" || echo "✓ 仅标准 C 库"

kill $LLM 2>/dev/null
echo
echo "=== DONE（环境保留在 $DEST，可手工检查）==="
