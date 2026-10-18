import http.server, json, sys

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 8932
LOG = "/tmp/fake_llm_strict.log"

class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get('Content-Length', 0))
        raw = self.rfile.read(n) or b'{}'
        body = json.loads(raw)
        msgs = body.get('messages', [])
        roles = [m.get('role') for m in msgs]
        # 严格校验：OpenAI 协议下 role=tool 必须紧跟结构化 tool_calls
        # M1 走文本协议 → 任何 role=tool 都应视为违规（真实服务会 400）
        has_tool = 'tool' in roles
        with open(LOG, 'a') as f:
            f.write(json.dumps({"roles": roles, "hasToolRole": has_tool}, ensure_ascii=False) + "\n")
        if has_tool:
            data = json.dumps({"error": {"message": "Messages with role 'tool' must be a response to a preceding message with 'tool_calls'", "type": "invalid_request_error"}}).encode()
            self.send_response(400)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            return
        # 首轮触发工具；若历史里已有工具结果（经 sanitize 转成 user）→ 收尾
        joined = " ".join(m.get('content', '') for m in msgs)
        if '【工具执行结果】' in joined:
            content = "读完了，文件里是第一章内容。"
        else:
            content = '<tool_call>{"name":"read","args":{"path":"a.txt"}}</tool_call>'
        resp = {"choices": [{"message": {"role": "assistant", "content": content}, "finish_reason": "stop"}]}
        data = json.dumps(resp).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *a):
        pass

http.server.HTTPServer(('127.0.0.1', PORT), H).serve_forever()
