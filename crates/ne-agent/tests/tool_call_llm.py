import http.server, json, sys

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 8935
# 第一步让模型调用工具；看到工具结果后收尾
class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get('Content-Length', 0))
        body = json.loads(self.rfile.read(n) or b'{}')
        msgs = body.get('messages', [])
        joined = " ".join(m.get('content', '') for m in msgs)
        if '【工具执行结果】' in joined:
            # 已拿到工具结果 → 摘要收尾
            tail = joined.split('【工具执行结果】')[-1][:300]
            content = "工具返回内容摘要：" + tail
        else:
            last = next((m['content'] for m in reversed(msgs) if m['role'] == 'user'), '')
            import re
            m = re.search(r'CALL:(\w+)\s+(\{.*\})', last)
            if m:
                content = '<tool_call>{"name":"%s","args":%s}</tool_call>' % (m.group(1), m.group(2))
            else:
                content = "没有指令"
        resp = {"choices": [{"message": {"role": "assistant", "content": content}, "finish_reason": "stop"}]}
        data = json.dumps(resp).encode()
        self.send_response(200); self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(data))); self.end_headers(); self.wfile.write(data)
    def log_message(self, *a): pass

http.server.HTTPServer(('127.0.0.1', PORT), H).serve_forever()
