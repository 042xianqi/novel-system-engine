#!/usr/bin/env python3
# mcp_call.py —— world-gov 独立 stdio MCP 客户端（预演/验证 Operit 客户端会走的路径）
#
# 用法：
#   python3 tools/mcp_call.py <tool> [--arg key=value ...] [--root DIR]
#   python3 tools/mcp_call.py --list          # 只列工具名
#
# 协议：newline-delimited JSON-RPC 2.0；initialize → notifications/initialized(无 id) → tools/call
# 默认 DRY（real_semantic 缺省 false），绝不真实外呼。
import subprocess, json, sys, os

ROOT = os.environ.get('NOVEL_LIB_ROOT', '/sdcard/Download/小说相关备份/projects/projects/系统/资料库')
BIN = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'target', 'release', 'ne-mcp')


def coerce(v):
    if v in ('true', 'True'): return True
    if v in ('false', 'False'): return False
    if v.isdigit(): return int(v)
    if v.startswith('[') or v.startswith('{'):
        return json.loads(v)
    return v


def main():
    args = sys.argv[1:]
    root = ROOT
    if '--root' in args:
        i = args.index('--root'); root = args[i + 1]; del args[i:i + 2]
    if not args:
        print(__doc__); sys.exit(2)
    tool = args[0]
    tool_args = {}
    if tool != '--list':
        for a in args[1:]:
            if a == '--arg': continue
            if '=' in a:
                k, v = a.split('=', 1); tool_args[k] = coerce(v)

    p = subprocess.Popen([BIN, '--root', root],
                         stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                         stderr=subprocess.DEVNULL, text=True, bufsize=1)
    _id = [0]

    def send(method, params, expect=True):
        if expect:
            _id[0] += 1
            mid = _id[0]
            p.stdin.write(json.dumps({'jsonrpc': '2.0', 'id': mid, 'method': method, 'params': params}) + '\n')
            p.stdin.flush()
            while True:
                line = p.stdout.readline()
                if not line: raise RuntimeError('EOF from server')
                d = json.loads(line)
                if d.get('id') == mid: return d
        else:  # notification：无 id，不等待
            p.stdin.write(json.dumps({'jsonrpc': '2.0', 'method': method, 'params': params}) + '\n')
            p.stdin.flush()

    try:
        r = send('initialize', {'protocolVersion': '2025-06-18', 'capabilities': {},
                                'clientInfo': {'name': 'mcp_call', 'version': '0'}})
        info = r['result']['serverInfo']
        send('notifications/initialized', {}, expect=False)
        if tool == '--list':
            rl = send('tools/list', {})
            names = [t['name'] for t in rl['result']['tools']]
            print(f"server={info['name']} v{info['version']} | tools={len(names)}")
            for n in names: print(' ', n)
            sys.exit(0)
        rc = send('tools/call', {'name': tool, 'arguments': tool_args})
        if 'error' in rc:
            print(f"[协议错误] {rc['error']}"); sys.exit(3)
        res = rc['result']
        txt = res['content'][0]['text'] if res.get('content') else '(空)'
        err = res.get('isError', False)
        print(f"server={info['name']} v{info['version']} | tool={tool} | isError={err}")
        print(txt)
        sys.exit(1 if err else 0)
    finally:
        try:
            p.stdin.close(); p.wait(timeout=5)
        except Exception:
            p.kill()


if __name__ == '__main__':
    main()
