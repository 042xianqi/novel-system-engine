#!/usr/bin/env python3
# operit_auto_loop.py —— world-gov 平台自主循环（护栏执行器 · 第 18.6 批）
#
# 用户决策（2026-10-15 08:33）固化的护栏规格：
#   1. 单轮硬上限 ≤40 张卡（服务端 HARD_MAX_PER_RUN 双保险）
#   2. 单日软预算 输入+输出合计 ≤1,200,000 tokens（按 llm stats 当日增量核算）
#   3. 不默认开启：仅在显式触发且 env 已开闸时运行；结束后输出审计（env 回落由开闸方负责）
#   4. 循环内 10±2s 软休眠；每 5 张自动快照一次；每轮结束强制 llm_stats 审计
#   5. pending 观点只增不改：不自动复核/归档，人工终审
#
# 序列（与用户参考序列同构）：
#   0. wg_self_check        → all_ok=false 则拒绝开跑
#   1. wg_inspect_batch 预览 → 取本轮 keys（≤cap，截断显式报告）
#   2. wg_snapshot create   → auto-loop-<epoch> 基线
#   3. 逐卡 wg_inspect_card → sleep 8~12s；每 5 张快照；BACKEND_ERROR 退避重试；预算超限即熔断
#   4. wg_conflict summary  → 观点池增量
#   5. wg_llm_status stats  → 强制审计落盘
#
# 用法（开闸方 = 用户/Operit 调度，负责临时注入 env 并在结束后回落）：
#   WOGOV_ALLOW_REAL_LLM=yes python3 tools/operit_auto_loop.py [--cap 40] [--slice 1/4] [--keys-file F]
#   （env 未开闸时直接拒绝执行；daily 预算超限拒绝执行）
import subprocess, json, sys, os, time, random, argparse, datetime

ROOT = '/sdcard/Download/小说相关备份/projects/projects/系统/资料库'
REPO = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..')
BIN = os.path.join(REPO, 'target', 'release', 'ne-mcp')
DAILY_TOKEN_CAP = 1_200_000
SNAPSHOT_EVERY = 5
DAY_START_TS = int(datetime.datetime.combine(datetime.date.today(), datetime.time.min).timestamp())


class McpSession:
    def __init__(self, root):
        self.p = subprocess.Popen([BIN, '--root', root], stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                  text=True, bufsize=1)
        self._id = 0
        r = self._send('initialize', {'protocolVersion': '2025-06-18', 'capabilities': {},
                                      'clientInfo': {'name': 'operit_auto_loop', 'version': '0.0.23'}})
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
                    raise RuntimeError('EOF from ne-mcp')
                d = json.loads(line)
                if d.get('id') == mid:
                    return d
        self.p.stdin.write(json.dumps({'jsonrpc': '2.0', 'method': method, 'params': params}) + '\n')
        self.p.stdin.flush()

    def call(self, tool, args):
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
            self.p.stdin.close(); self.p.wait(timeout=5)
        except Exception:
            self.p.kill()


def stats_now(sess):
    _, s = sess.call('wg_llm_status', {'action': 'stats'})
    # 今日增量 = 今日已落库行合计（stats 是全量，需按当日行数聚合；退而求其次用全量做跨日基准文件）
    return s


def daily_token_delta(sess):
    """当日 token 消耗：直接查 llm_call_log 当日行合计（只读 SQL 经本地 sqlite）。"""
    import sqlite3
    c = sqlite3.connect(os.path.join(ROOT, 'novel.db'))
    q = c.execute("SELECT COALESCE(SUM(token_in+token_out),0) FROM llm_call_log WHERE ts>=?", (DAY_START_TS,)).fetchone()[0]
    c.close()
    return q


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--cap', type=int, default=40)
    ap.add_argument('--slice', default='1/4')
    ap.add_argument('--keys-file', default=None)
    ap.add_argument('--retries', type=int, default=2)
    a = ap.parse_args()
    cap = min(a.cap, 40)

    env = os.environ.get('WOGOV_ALLOW_REAL_LLM', '')
    if env != 'yes':
        print(f"❌ 护栏拒绝：WOGOV_ALLOW_REAL_LLM={env!r} ≠ 'yes'。本脚本只做真实推理；"
              f"预演请用 tools/mcp_trial_seq.py --mode dry。")
        sys.exit(2)

    sess = McpSession(ROOT)
    print(f"=== 平台自主循环 | cap={cap} slice={a.slice} | 休眠 8~12s | 每日预算 {DAILY_TOKEN_CAP} ===")
    print(f"    server={sess.info['name']} v{sess.info['version']}")

    # 0. self_check 门禁
    err, sc = sess.call('wg_self_check', {'fix': False})
    if err or not sc.get('all_ok'):
        print(f"❌ self_check 未全绿（failed={sc.get('failed')}），拒绝开跑"); sess.close(); sys.exit(3)
    print(f"    self_check: {sc.get('checks')} 项全绿")

    # 预算预检
    spent = daily_token_delta(sess)
    print(f"    今日已耗 token(in+out): {spent} / {DAILY_TOKEN_CAP}")
    if spent >= DAILY_TOKEN_CAP:
        print("❌ 护栏熔断：日预算已满，禁止开跑"); sess.close(); sys.exit(4)

    # 1. 预览取 keys
    if a.keys_file:
        with open(a.keys_file, encoding='utf-8') as f:
            keys_all = [ln.strip() for ln in f if ln.strip()]
    else:
        _, prev = sess.call('wg_inspect_batch', {'slice': a.slice, 'max_batch': cap})
        keys_all = prev.get('keys', [])
    total = len(keys_all)
    keys = keys_all[:cap]
    print(f"    本轮 {len(keys)}/{total} 张{'（已达硬上限，截断）' if total > cap else ''}")

    # 2. 基线快照
    epoch = int(time.time())
    _, sn = sess.call('wg_snapshot', {'action': 'create', 'label': f'auto-loop-{epoch}'})
    base_snap = sn.get('snap_id')
    print(f"    基线快照: {base_snap} (entry_count={sn.get('entry_count')})")

    # 3. 逐卡循环
    ok = fail = skipped = 0
    results, snaps = [], [base_snap]
    t0 = time.time()
    try:
        for i, key in enumerate(keys, 1):
            if daily_token_delta(sess) >= DAILY_TOKEN_CAP:
                print(f"\n🛑 熔断：第 {i} 张前日预算触顶（{DAILY_TOKEN_CAP}），中止本轮；已完成 {i-1} 张")
                break
            err, r = sess.call('wg_inspect_card', {'key': key, 'semantic': True, 'real_semantic': True})
            attempt = 0
            while isinstance(r, dict) and r.get('status') == 'BACKEND_ERROR' and attempt < a.retries:
                attempt += 1
                w = 5.0 * attempt
                print(f"    ↻ {key} transport 失败，退避 {w:.0f}s 重试…"); time.sleep(w)
                err, r = sess.call('wg_inspect_card', {'key': key, 'semantic': True, 'real_semantic': True})
            status = r.get('status', '?') if isinstance(r, dict) else '?'
            verdict = r.get('verdict') if isinstance(r, dict) else None
            cid = r.get('conflict_id') if isinstance(r, dict) else None
            dedup = r.get('cache_dedup', False) if isinstance(r, dict) else False
            if status == 'OK' or status == 'CACHE_HIT':
                ok += 1
            else:
                fail += 1
            results.append({'key': key, 'status': status, 'verdict': verdict,
                            'conflict_id': cid, 'cache_dedup': dedup})
            print(f"[{i}/{len(keys)}] {key} → {status}{' (cache_dedup)' if dedup else ''}"
                  f"{f' verdict={json.dumps(verdict, ensure_ascii=False)}' if verdict else ''}")
            if i < len(keys):
                time.sleep(random.uniform(8, 12))  # 护栏：10±2s 软休眠
            if i % SNAPSHOT_EVERY == 0:
                _, s2 = sess.call('wg_snapshot', {'action': 'create', 'label': f'auto-loop-{epoch}-at{i}'})
                snaps.append(s2.get('snap_id'))
                print(f"    📸 快照 @{i}: {s2.get('snap_id')[:16]}…")
    except KeyboardInterrupt:
        print("\n⚠️ 中断：停止后续调用，输出已完成审计")

    # 4+5. 强制审计
    _, conf = sess.call('wg_conflict', {'action': 'summary'})
    st = stats_now(sess)
    spent_end = daily_token_delta(sess)
    dur = time.time() - t0
    audit = {'cap': cap, 'slice': a.slice, 'epoch': epoch, 'base_snap': base_snap,
             'snapshots': snaps, 'ok': ok, 'fail': fail, 'total_candidates': total,
             'results': results, 'conflict_summary': conf,
             'llm_stats': st, 'daily_token_spent': spent_end, 'daily_cap': DAILY_TOKEN_CAP,
             'elapsed_sec': round(dur, 1), 'env_at_finish': os.environ.get('WOGOV_ALLOW_REAL_LLM', '')}
    path = f'/tmp/operit_loop_{epoch}.json'
    with open(path, 'w', encoding='utf-8') as f:
        json.dump(audit, f, ensure_ascii=False, indent=2)
    print(f"\n=== 循环结束（ok={ok} fail={fail} / {len(results)}，{dur:.0f}s）===")
    print(f"    今日 token: {spent_end} / {DAILY_TOKEN_CAP}")
    print(f"    conflict: {json.dumps(conf, ensure_ascii=False)[:160]}")
    print(f"    快照链: {len(snaps)} 个（基线+每5张）")
    print(f"    审计文件: {path}")
    print(f"    ⚠️ 开闸方请立即回落：unset WOGOV_ALLOW_REAL_LLM（当前={audit['env_at_finish']!r}）")
    sess.close()
    sys.exit(0 if fail == 0 else 1)


if __name__ == '__main__':
    main()