#!/usr/bin/env python3
# mcp_trial_seq.py —— world-gov MCP 调用序列（受控试运行 · 独立 stdio 客户端）
#
# 定位：待办③「接入 Operit MCP 客户端自主闭环」的**离线预演版**。
#   用一个持久 ne-mcp stdio 会话，逐卡 tools/call wg_inspect_card，
#   软限流 + 可中断 + 每轮自动快照 + 审计汇总——与将来 Operit 客户端走的
#   路径同构（MCP 不 fork shell、单会话复用、JSON-RPC 往返）。
#
# 三种模式（--mode）：
#   dry     semantic=true dry_run=true   → 只渲染 prompt，不收口、不外呼、不写库（无网可跑）
#   locked  semantic=true real_semantic=false → 走统一收口，三重锁拦截 → REAL_SEMANTIC_LOCKED（无网可跑）
#   real    semantic=true real_semantic=true  → 真实外呼 + 落 conflict_records（需三重锁全开 + 端点在线）
#
# 用法：
#   python3 tools/mcp_trial_seq.py --mode dry    --limit 3 --slice 1/4
#   python3 tools/mcp_trial_seq.py --mode locked --limit 3 --slice 1/4
#   export WOGOV_ALLOW_REAL_LLM=yes
#   python3 tools/mcp_trial_seq.py --mode real   --limit 20 --slice 1/4
import subprocess, json, sys, os, time, random, argparse

ROOT = '/sdcard/Download/小说相关备份/projects/projects/系统/资料库'
REPO = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..')
BIN = os.path.join(REPO, 'target', 'release', 'ne-mcp')


class McpSession:
    """持久 stdio MCP 会话：initialize 一次，后续 tools/call 复用同一连接。"""

    def __init__(self, root):
        self.p = subprocess.Popen([BIN, '--root', root],
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.DEVNULL, text=True, bufsize=1)
        self._id = 0
        r = self._send('initialize', {'protocolVersion': '2025-06-18', 'capabilities': {},
                                      'clientInfo': {'name': 'mcp_trial_seq', 'version': '0'}})
        self.info = r['result']['serverInfo']
        self._send('notifications/initialized', {}, expect=False)

    def _send(self, method, params, expect=True):
        if expect:
            self._id += 1
            mid = self._id
            self.p.stdin.write(json.dumps({'jsonrpc': '2.0', 'id': mid, 'method': method, 'params': params}) + '\n')
            self.p.stdin.flush()
            while True:
                line = self.p.stdout.readline()
                if not line:
                    raise RuntimeError('EOF from ne-mcp（会话已断）')
                d = json.loads(line)
                if d.get('id') == mid:
                    return d
        else:
            self.p.stdin.write(json.dumps({'jsonrpc': '2.0', 'method': method, 'params': params}) + '\n')
            self.p.stdin.flush()

    def call(self, tool, args):
        """tools/call → 返回 (isError, 解析后的 JSON 或原始文本)。"""
        r = self._send('tools/call', {'name': tool, 'arguments': args})
        if 'error' in r:
            return True, {'protocol_error': r['error']}
        res = r['result']
        err = res.get('isError', False)
        txt = res['content'][0]['text'] if res.get('content') else ''
        try:
            return err, json.loads(txt)
        except (json.JSONDecodeError, ValueError):
            return err, txt

    def close(self):
        try:
            self.p.stdin.close()
            self.p.wait(timeout=5)
        except Exception:
            self.p.kill()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--mode', choices=['dry', 'locked', 'real'], default='dry')
    ap.add_argument('--slice', default='1/4')
    ap.add_argument('--limit', type=int, default=3)
    ap.add_argument('--keys-file', default=None, help='预生成的 key 清单文件；缺省则用 wg_inspect_batch 预览取 keys')
    ap.add_argument('--root', default=ROOT)
    ap.add_argument('--delay-min', type=float, default=None)
    ap.add_argument('--delay-max', type=float, default=None)
    ap.add_argument('--no-snapshot', action='store_true', help='跳过每轮快照（预演时可省）')
    a = ap.parse_args()

    # 软限流：real 默认 8~12s；dry/locked 默认 0.2~0.5s（仍走休眠链路，验证调度）
    if a.delay_min is not None and a.delay_max is not None:
        dmin, dmax = a.delay_min, a.delay_max
    elif a.mode == 'real':
        dmin, dmax = 8.0, 12.0
    else:
        dmin, dmax = 0.2, 0.5

    sess = McpSession(a.root)
    print(f"=== MCP 序列试运行 | mode={a.mode} | slice={a.slice} | limit={a.limit} | delay={dmin}~{dmax}s ===")
    print(f"    server={sess.info['name']} v{sess.info['version']}")

    # 三重锁状态（real 模式必须全开，否则提示）
    _, gate = sess.call('wg_llm_status', {'action': 'gate'})
    print(f"    gate={gate.get('status','?')}")
    if a.mode == 'real' and gate.get('gate_locked', True):
        print("    ⚠️ real 模式但三重锁未全开（需 WOGOV_ALLOW_REAL_LLM=yes + feature + real_semantic）；将收到 REAL_SEMANTIC_LOCKED")

    # 取 key 清单
    if a.keys_file:
        with open(a.keys_file, encoding='utf-8') as f:
            keys = [ln.strip() for ln in f if ln.strip()]
    else:
        _, prev = sess.call('wg_inspect_batch', {'slice': a.slice})
        keys = prev.get('keys', [])
    keys = keys[:a.limit]
    print(f"    分片 {a.slice} 取 {len(keys)} 张卡（源：{'keys-file' if a.keys_file else 'wg_inspect_batch 预览'}）")

    # 基线快照（real 模式；dry/locked 无写入，可选跳过）
    base_snap = None
    if not a.no_snapshot:
        _, sn = sess.call('wg_snapshot', {'action': 'create', 'label': f'mcp-seq-{a.mode}-base'})
        base_snap = sn.get('snap_id')
        print(f"    基线快照: {base_snap} (entry_count={sn.get('entry_count')})")

    ok = fail = 0
    results = []
    t0 = time.time()
    try:
        for i, key in enumerate(keys, 1):
            args = {'key': key, 'semantic': True}
            if a.mode == 'dry':
                args['dry_run'] = True
            elif a.mode == 'locked':
                args['real_semantic'] = False
            else:  # real
                args['real_semantic'] = True
            err, r = sess.call('wg_inspect_card', args)
            status = r.get('status', 'DRY_RENDER' if a.mode == 'dry' else '?') if isinstance(r, dict) else '?'
            if a.mode == 'dry' and isinstance(r, dict) and r.get('dry_run'):
                status = 'DRY_RENDER'
            verdict = r.get('verdict') if isinstance(r, dict) else None
            cid = r.get('conflict_id') if isinstance(r, dict) else None
            good = (not err) and status not in ('NOT_FOUND', '?', 'BACKEND_ERROR', 'GUARD_REJECTED', 'MALFORMED')
            if good:
                ok += 1
            else:
                fail += 1
            results.append({'key': key, 'status': status, 'verdict': verdict, 'conflict_id': cid, 'isError': err})
            vstr = f" verdict={json.dumps(verdict, ensure_ascii=False)}" if verdict else ''
            cstr = f" conflict_id={cid}" if cid else ''
            print(f"[{i}/{len(keys)}] {key} → {status}{vstr}{cstr}{' [isError]' if err else ''}")
            # 软限流（最后一张不休眠）
            if i < len(keys):
                d = random.uniform(dmin, dmax)
                time.sleep(d)
    except KeyboardInterrupt:
        print("\n⚠️ 收到中断（Ctrl-C），停止后续调用，输出已完成汇总…")

    # 审计汇总
    _, stats = sess.call('wg_llm_status', {'action': 'stats'})
    _, conf = sess.call('wg_conflict', {'action': 'summary'})
    end_snap = None
    if base_snap and not a.no_snapshot:
        _, sn = sess.call('wg_snapshot', {'action': 'create', 'label': f'mcp-seq-{a.mode}-end'})
        end_snap = sn.get('snap_id')
    dur = time.time() - t0
    print(f"\n=== 序列结束（ok={ok} fail={fail} / 共 {len(results)}，耗时 {dur:.1f}s）===")
    print(f"    llm_stats: total_calls={stats.get('total_calls')} total_fail={stats.get('total_fail')} "
          f"token_in={stats.get('token_in_total')} token_out={stats.get('token_out_total')}")
    print(f"    conflict summary: {json.dumps(conf, ensure_ascii=False)[:200]}")
    if base_snap:
        print(f"    快照: base={base_snap} end={end_snap}")
    # 落审计文件（供放量后复盘）
    audit_path = f"/tmp/mcp_seq_{a.mode}_{int(t0)}.json"
    with open(audit_path, 'w', encoding='utf-8') as f:
        json.dump({'mode': a.mode, 'slice': a.slice, 'results': results,
                   'ok': ok, 'fail': fail, 'stats': stats, 'base_snap': base_snap,
                   'end_snap': end_snap}, f, ensure_ascii=False, indent=2)
    print(f"    审计文件: {audit_path}")
    sess.close()
    sys.exit(0 if fail == 0 else 1)


if __name__ == '__main__':
    main()