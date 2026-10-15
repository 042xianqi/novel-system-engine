#!/usr/bin/env python3
# l5gen.py —— L5 正文生成闭环（第 20 批，现行 Rust 架构版）
#
# 为什么不用 05_管线框架/l5.py：旧 Python 引擎已删（Go/Python 调度层 2026-10-13 移除），
#   l5.py 依赖的 kb_quotes 表不存在。本脚本用**现行组件**重建同一闭环：
#     ne_inject（MCP，取注入包） → LLM（workbuddy 端点，风格铁律 prompt）
#     → ne_check（MCP，16 门禁 + P0/P1 质检） → 写 04_正文/卷01/第N章.md
#
# 素材源（唯一真相源）：
#   · 风格模板：05_管线框架/prompts/l5_正文.md（§1 铁律 + §3 约束 + 注入块格式）
#   · 细纲：03_叙事规划/细纲/卷0N.md（经 ne_inject 读出）
#   · 角色/事件卡：02_资料库（经 ne_inject 读出）
#
# 用法：
#   python3 tools/l5gen.py --chapter 1             # 生成第1章（含质检）
#   python3 tools/l5gen.py --chapter 1 --dry-run   # 只组装 prompt，不调 LLM
#   python3 tools/l5gen.py --chapter 1 --force     # 覆写已存在文件
import subprocess, json, os, sys, re, time, argparse, urllib.request

ROOT = '/sdcard/Download/小说相关备份/projects/projects/系统/资料库'
REPO = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..')
BIN = os.path.join(REPO, 'target', 'release', 'ne-mcp')
STYLE = os.path.join(ROOT, '05_管线框架', 'prompts', 'l5_正文.md')
TEXT_DIR = os.path.join(ROOT, '04_正文', '卷01')
ENDPOINT = 'http://127.0.0.1:8788/v1/chat/completions'
MODEL = 'glm-5.3-flash'
KEY = 'sk-9c8bc3e9c09df023e7ca572d058e5d17af62226d18297a14'


class Mcp:
    def __init__(self, root):
        self.p = subprocess.Popen([BIN, '--root', root], stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, bufsize=1)
        self._id = 0
        self._send('initialize', {'protocolVersion': '2025-06-18', 'capabilities': {},
                                  'clientInfo': {'name': 'l5gen', 'version': '0'}})
        self._send('notifications/initialized', {}, expect=False)

    def _send(self, m, p, expect=True):
        if expect:
            self._id += 1
            mid = self._id
            self.p.stdin.write(json.dumps({'jsonrpc': '2.0', 'id': mid, 'method': m, 'params': p}) + '\n')
            self.p.stdin.flush()
            while True:
                line = self.p.stdout.readline()
                if not line:
                    raise RuntimeError('EOF ne-mcp')
                d = json.loads(line)
                if d.get('id') == mid:
                    return d
        self.p.stdin.write(json.dumps({'jsonrpc': '2.0', 'method': m, 'params': p}) + '\n')
        self.p.stdin.flush()

    def call(self, tool, args):
        r = self._send('tools/call', {'name': tool, 'arguments': args})
        if 'error' in r:
            return True, {'protocol_error': r['error']}
        res = r['result']
        txt = res['content'][0]['text'] if res.get('content') else ''
        try:
            return res.get('isError', False), json.loads(txt)
        except Exception:
            return res.get('isError', False), txt

    def close(self):
        try:
            self.p.stdin.close(); self.p.wait(timeout=5)
        except Exception:
            self.p.kill()


def build_messages(packet, chapter, style_text, scene_note=''):
    pk = packet['packet']
    ch = pk['chapter']
    banned = pk.get('banned_words', [])
    chars = pk.get('chars', [])
    mats = pk.get('materials', [])
    world = pk.get('world')
    prev_tail = pk.get('prev_tail')
    events = pk.get('events', [])
    char_txt = '\n\n'.join(f"【角色卡·{c['code']}】\n{c.get('content','')}" for c in chars) or '（本章无独立角色卡；主角=陈寂 CHAR-99-001）'
    mat_txt = '\n\n'.join(f"【{m['code']}】\n{m.get('content','') if m.get('is_full') else m.get('summary','')}" for m in mats)
    world_txt = f"\n### 世界观基线（全局铁则·必守）\n{world}\n" if world else ''
    tail_txt = f"\n### 前文末尾（衔接用）\n{prev_tail}\n" if prev_tail else ''
    ev_txt = f"\n### 事件状态\n{json.dumps(events, ensure_ascii=False)}\n" if events else ''
    user = (
        f"##### 本章任务：卷01 第{chapter}章 · {packet.get('title','')} #####\n\n"
        f"### 本章细纲要点（必须发生，含最高优先级约束）\n{ch['outline']}\n\n"
        f"### 本章出场角色卡\n{char_txt}\n"
        f"{world_txt}{ev_txt}{tail_txt}\n"
        f"### 关联资料卡（概念名可直接用，不写编号）\n{mat_txt}\n\n"
        f"### 禁词（命中即打回）\n{', '.join(banned) if banned else '（无）'}\n\n"
        f"### 输出要求\n"
        f"- 直接输出第{chapter}章正文，2500-4000 字，不写标题、不写元评论。\n"
        f"- 严格遵守系统消息里的作者风格铁律（§1）与输出约束（§3）。\n"
        f"- 细纲要点的每个事件都必须发生；决定→代价→变化三拍落地。\n"
        f"- 本章最高优先级约束优先于任何定版台词要求。"
    )
    if scene_note:
        user += f"\n- 【场景边界·重要】{scene_note}"
    return [{'role': 'system', 'content': style_text}, {'role': 'user', 'content': user}]


def call_llm(messages, max_tokens=32768, temperature=0.8, retries=3):
    body = json.dumps({'model': MODEL, 'messages': messages,
                       'max_tokens': max_tokens, 'temperature': temperature}).encode()
    for i in range(retries):
        try:
            req = urllib.request.Request(ENDPOINT, data=body, headers={
                'Content-Type': 'application/json', 'Authorization': f'Bearer {KEY}'})
            with urllib.request.urlopen(req, timeout=600) as r:
                d = json.loads(r.read())
            return d['choices'][0]['message']['content'], d.get('usage', {})
        except Exception as e:
            print(f"  LLM 调用失败（第{i+1}次）：{e}")
            time.sleep(2 ** i)
    raise RuntimeError('LLM 调用三次均失败')


def strip_meta(text):
    """剥掉可能混入的【技法计划】行与 markdown 标题。"""
    lines = [ln for ln in text.split('\n') if not re.match(r'^\s*【技法计划', ln) and not re.match(r'^#{1,3}\s', ln)]
    return '\n'.join(lines).strip()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--chapter', type=int, required=True)
    ap.add_argument('--volume', default='01')
    ap.add_argument('--outline-from', type=int, default=None,
                    help='取细纲的章号（默认=--chapter）。场景级合并时用：如正文第2章取细纲第6章')
    ap.add_argument('--scene-note', default='',
                    help='追加进 prompt 的场景合并说明（如「本章须完整展开老屋夜场景，不重复涧底醒」）')
    ap.add_argument('--dry-run', action='store_true')
    ap.add_argument('--force', action='store_true')
    ap.add_argument('--skip-check', action='store_true')
    a = ap.parse_args()
    of = a.outline_from or a.chapter

    out_path = os.path.join(TEXT_DIR, f'第{a.chapter}章.md')
    if os.path.exists(out_path) and not a.force:
        print(f"❌ {out_path} 已存在（用 --force 覆写）"); sys.exit(1)

    style_text = open(STYLE, encoding='utf-8').read()
    mcp = Mcp(ROOT)
    err, packet = mcp.call('ne_inject', {'volume': a.volume, 'chapter': of})
    if err or not isinstance(packet, dict) or not packet.get('ok'):
        print(f"❌ ne_inject 失败：{packet}"); mcp.close(); sys.exit(2)
    messages = build_messages(packet, a.chapter, style_text, a.scene_note)
    print(f"=== L5 生成 | 卷{a.volume} 第{a.chapter}章（细纲源 ch{of}）| 注入卡 {packet.get('card_refs')} | prompt {sum(len(m['content']) for m in messages)} 字符 ===")

    if a.dry_run:
        os.makedirs('/tmp', exist_ok=True)
        dump = f'/tmp/l5gen_ch{a.chapter}_prompt.txt'
        open(dump, 'w', encoding='utf-8').write('\n\n=====USER=====\n\n'.join(m['content'] for m in messages))
        print(f"dry-run：prompt 已写 {dump}"); mcp.close(); sys.exit(0)

    t0 = time.time()
    text, usage = call_llm(messages)
    text = strip_meta(text)
    words = len(re.findall(r'[\u4e00-\u9fff]', text))
    print(f"  生成完成：{words} 汉字 | {time.time()-t0:.1f}s | tokens in={usage.get('prompt_tokens')} out={usage.get('completion_tokens')}")

    # 质检（ne_check）
    if not a.skip_check:
        err, chk = mcp.call('ne_check', {'text': text, 'chapter': a.chapter, 'volume': f'卷{a.volume}'})
        if isinstance(chk, dict):
            print(f"  质检 verdict={chk.get('verdict')} | P0={len(chk.get('p0',[]))} P1={len(chk.get('p1',[]))} "
                  f"| 门禁失败={[g for g,i in chk.get('gates',{}).items() if not i.get('pass')]}")
            for x in chk.get('p0', [])[:5]:
                print(f"    🔴 [{x.get('gate')}] {x.get('issue')} → {x.get('action','')}")

    os.makedirs(TEXT_DIR, exist_ok=True)
    open(out_path, 'w', encoding='utf-8').write(text + '\n')
    print(f"  已写入: {out_path}")
    mcp.close()


if __name__ == '__main__':
    main()