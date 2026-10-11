import http.server, json, sys

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 8933
LOG = "/tmp/fake_llm_mem.log"

class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get('Content-Length', 0))
        body = json.loads(self.rfile.read(n) or b'{}')
        msgs = body.get('messages', [])
        with open(LOG, 'a') as f:
            f.write(json.dumps({"roles": [m.get('role') for m in msgs],
                                "system": next((m['content'] for m in msgs if m['role']=='system'), ''),
                                "lastUser": next((m['content'] for m in reversed(msgs) if m['role']=='user'), '')},
                               ensure_ascii=False) + "\n")
        resp = {"choices": [{"message": {"role": "assistant", "content": "好的，我记住了。"}, "finish_reason": "stop"}]}
        data = json.dumps(resp).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *a):
        pass

http.server.HTTPServer(('127.0.0.1', PORT), H).serve_forever()
