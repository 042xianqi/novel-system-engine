#!/usr/bin/env python3
"""最小 MCP 服务（stdio + newline-delimited JSON-RPC 2.0）—— 供 ne-agent 单测使用。

只实现握手 + tools/list + tools/call(echo)，验证客户端行为。
"""
import json
import sys


def handle(req):
    method = req.get("method")
    rid = req.get("id")
    if method == "initialize":
        return {"jsonrpc": "2.0", "id": rid, "result": {
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "fake", "version": "0.0.1"},
        }}
    if method == "tools/list":
        return {"jsonrpc": "2.0", "id": rid, "result": {"tools": [
            {"name": "echo", "description": "回显输入",
             "inputSchema": {"type": "object",
                             "properties": {"msg": {"type": "string"}},
                             "required": ["msg"]}},
        ]}}
    if method == "tools/call":
        p = req.get("params", {})
        if p.get("name") == "echo":
            msg = (p.get("arguments") or {}).get("msg", "")
            return {"jsonrpc": "2.0", "id": rid, "result": {
                "content": [{"type": "text", "text": f"echo: {msg}"}]}}
        return {"jsonrpc": "2.0", "id": rid,
                "error": {"code": -32601, "message": f"未知工具 {p.get('name')}"}}
    if method and method.startswith("notifications/"):
        return None  # 通知：不响应
    return {"jsonrpc": "2.0", "id": rid,
            "error": {"code": -32601, "message": f"未知方法 {method}"}}


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except Exception:
            continue
        resp = handle(req)
        if resp is not None:
            sys.stdout.write(json.dumps(resp, ensure_ascii=False) + "\n")
            sys.stdout.flush()


if __name__ == "__main__":
    main()
