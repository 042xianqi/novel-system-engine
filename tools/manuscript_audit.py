#!/usr/bin/env python3
# manuscript_audit.py —— world-gov 正文质检（第 20 批，只读、零外呼）
#
# 目的：用户要「干干净净的正文」。本工具用 ne_check 的 17 道门禁（同一实现，CLI/MCP 共用）
#       逐章质检 04_正文，产出一份「干净度报告」：
#         - 每章 verdict（pass/fix/reject）+ P0（阻断）/ P1（建议）清单
#         - 门禁失败明细
#         - 异常短章（疑似残缺）
#         - 实验变体文件（第1章_* 等，应清理的非正文）
#       全部**只读**：不改一个字、不写库。清理/修复动作由人决定。
#
# 用法：
#   python3 tools/manuscript_audit.py                 # 扫 卷01
#   python3 tools/manuscript_audit.py --volume 卷01 --json-only
import subprocess, json, os, sys, re, argparse, datetime, collections

ROOT = '/sdcard/Download/小说相关备份/projects/projects/系统/资料库'
TEXT_DIR = os.path.join(ROOT, '04_正文')
BIN = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'target', 'release', 'ne-mcp')
REPORT_DIR = os.path.join(ROOT, '07_勘误工坊', 'reports')
# 正式章名：第N章.md；其余（第1章_xxx）视为实验变体
OFFICIAL = re.compile(r'^第\d+章\.md$')

# ─── 第 20 批：正文污染检测启发式（只读识别，不做任何删除/移动）───
# AI 提问草稿特征：生成中途停下向用户确认，不含正文叙事
AI_QUESTION_MARKERS = [
    '动笔前', '先指出一个矛盾', '需要先确认', '请确认', '告诉我按哪个走',
    '二选一', '三选一', '可能的处理方式', '我的建议是方案', '请给我正确细纲',
    '若选', '按 A 还是 B', '这属于会打回',
]
# 重复开篇：取正文前 80 汉字做前缀指纹
def opening_fingerprint(text):
    body = re.sub(r'[#>*\-\s`]', '', text)
    return body[:80]


def detect_pollution(path, text):
    """返回污染标签列表：AI_QUESTION_DRAFT / DUPLICATE_OPENING（后者需跨章比对）。"""
    tags = []
    head = text[:400]
    if sum(1 for m in AI_QUESTION_MARKERS if m in head) >= 2:
        tags.append('AI_QUESTION_DRAFT')
    return tags


class McpSession:
    def __init__(self, root):
        self.p = subprocess.Popen([BIN, '--root', root], stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                  text=True, bufsize=1)
        self._id = 0
        self._send('initialize', {'protocolVersion': '2025-06-18', 'capabilities': {},
                                  'clientInfo': {'name': 'manuscript_audit', 'version': '0'}})
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

    def check(self, text, chapter, volume):
        r = self._send('tools/call', {'name': 'ne_check',
                                      'arguments': {'text': text, 'chapter': chapter, 'volume': volume}})
        res = r['result']
        return json.loads(res['content'][0]['text'])

    def close(self):
        try:
            self.p.stdin.close(); self.p.wait(timeout=5)
        except Exception:
            self.p.kill()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--volume', default='卷01')
    ap.add_argument('--json-only', action='store_true')
    ap.add_argument('--min-bytes', type=int, default=3000, help='低于此字节数标疑似残缺')
    a = ap.parse_args()

    vol_dir = os.path.join(TEXT_DIR, a.volume)
    files = sorted(os.listdir(vol_dir)) if os.path.isdir(vol_dir) else []
    official, variants = [], []
    for f in files:
        (official if OFFICIAL.match(f) else variants).append(f)
    # 正式章按章号排序
    official.sort(key=lambda x: int(re.match(r'^第(\d+)章', x).group(1)))

    sess = McpSession(ROOT)
    results, gate_fail = [], collections.Counter()
    print(f"=== 正文质检 | {a.volume} | 正式章 {len(official)} | 实验变体 {len(variants)} ===")
    for f in official:
        p = os.path.join(vol_dir, f)
        txt = open(p, encoding='utf-8').read()
        ch = int(re.match(r'^第(\d+)章', f).group(1))
        d = sess.check(txt, ch, a.volume)
        rec = {'file': f, 'chapter': ch, 'bytes': os.path.getsize(p), 'words': d.get('stats', {}).get('words'),
               'verdict': d.get('verdict'), 'p0': d.get('p0', []), 'p1': d.get('p1', []),
               'gates_failed': [g for g, i in d.get('gates', {}).items() if not i.get('pass')],
               'short': os.path.getsize(p) < a.min_bytes}
        for g in rec['gates_failed']:
            gate_fail[g] += 1
        results.append(rec)
        flag = '' if rec['verdict'] == 'pass' else (' ⚠️' if rec['verdict'] == 'fix' else ' ❌')
        sh = '  [疑似残缺]' if rec['short'] else ''
        print(f"  第{ch:>2}章 → {rec['verdict']:6s} | P0={len(rec['p0'])} P1={len(rec['p1'])} | 门禁失败={rec['gates_failed']}{flag}{sh}")
    sess.close()

    verdicts = collections.Counter(r['verdict'] for r in results)
    shorts = [r['file'] for r in results if r['short']]
    summary = {'volume': a.volume, 'official_count': len(official), 'variant_count': len(variants),
               'verdicts': dict(verdicts), 'gate_fail_top': dict(gate_fail.most_common()),
               'short_files': shorts, 'variants': variants, 'total_p0': sum(len(r['p0']) for r in results),
               'total_p1': sum(len(r['p1']) for r in results)}
    print(f"\n=== 干净度汇总 ===")
    print(f"  verdict: {dict(verdicts)}")
    print(f"  P0 总数 {summary['total_p0']} | P1 总数 {summary['total_p1']}")
    if gate_fail:
        print(f"  门禁失败 Top: {dict(gate_fail.most_common(5))}")
    if shorts:
        print(f"  疑似残缺（<{a.min_bytes}B）: {shorts}")
    if variants:
        print(f"  实验变体（应清理）: {variants}")
    if a.json_only:
        print(json.dumps(summary, ensure_ascii=False)); sys.exit(0)

    os.makedirs(REPORT_DIR, exist_ok=True)
    ts = datetime.datetime.now().strftime('%Y%m%d_%H%M%S')
    md = os.path.join(REPORT_DIR, f'manuscript_audit_{a.volume}_{ts}.md')
    js = os.path.join(REPORT_DIR, f'manuscript_audit_{a.volume}_{ts}.json')
    L = [f"# 正文质检报告 · {a.volume} · {ts}", '',
         f"正式章 {len(official)}｜实验变体 {len(variants)}｜verdict {dict(verdicts)}", '',
         f"P0（阻断）总数 {summary['total_p0']}｜P1（建议）总数 {summary['total_p1']}", '']
    if shorts:
        L += ['## ⚠️ 疑似残缺（需人工确认）', ''] + [f'- `{s}`' for s in shorts] + ['']
    if variants:
        L += ['## 🧹 实验变体（非正式正文，建议清理）', ''] + [f'- `{v}`' for v in variants] + ['']
    L += ['## 逐章明细', '', '| 章 | verdict | 字数 | P0 | P1 | 门禁失败 |', '|---|---|---|---|---|---|']
    for r in results:
        L.append(f"| 第{r['chapter']}章 | {r['verdict']} | {r['words']} | {len(r['p0'])} | {len(r['p1'])} | {','.join(r['gates_failed']) or '—'} |")
    L.append('')
    L += ['## P0/P1 明细', '']
    for r in results:
        if r['p0'] or r['p1']:
            L.append(f"### 第{r['chapter']}章")
            for x in r['p0']:
                L.append(f"- 🔴 P0 [{x.get('gate')}] {x.get('issue')} → {x.get('action','')}")
            for x in r['p1']:
                L.append(f"- 🟡 P1 [{x.get('gate')}] {x.get('issue')} → {x.get('action','')}")
            L.append('')
    with open(md, 'w', encoding='utf-8') as f:
        f.write('\n'.join(L))
    with open(js, 'w', encoding='utf-8') as f:
        json.dump({'summary': summary, 'results': results}, f, ensure_ascii=False, indent=2)
    print(f"\n报告: {md}\n数据: {js}")


if __name__ == '__main__':
    main()