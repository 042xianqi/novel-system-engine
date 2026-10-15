#!/usr/bin/env python3
# conflict_triage.py —— world-gov 观点池「终审分级工作表」生成器（第 19 批，只读、零外呼）
#
# 目的：把 179 条 pending 从「顺序堆」变成「按价值分级、可勾选」的决策材料，
#       让人工终审的黄金时间优先花在最高价值的条目上。
#
# 分级（按 AI 自判 verdict，从高到低）：
#   conflict  🔴 明确冲突（最可能是真矛盾，必看）
#   suspect   🟠 可疑（值得快速扫一眼）
#   unclear   🟡 端水/不确定（低价值，可批量忽略或抽查）
#   ok        🟢 判定无问题（可直接归档）
#
# 处理：
#   1. 去重：同一 probe_key 且 evidence 内容哈希相同 → 保留最小 id，其余标 dup_of
#   2. 每卡聚合：一张卡取最高优先级 verdict 作为该卡终审档位
#   3. 产出：JSON（机器用）+ Markdown 工作表（人用，带勾选列）
#
# 用法：
#   python3 tools/conflict_triage.py                # 默认输出到 07_勘误工坊/reports/
#   python3 tools/conflict_triage.py --json-only    # 只要统计
import sqlite3, json, hashlib, os, sys, argparse, datetime, collections

ROOT = '/sdcard/Download/小说相关备份/projects/projects/系统/资料库'
DB = os.path.join(ROOT, 'novel.db')
REPORT_DIR = os.path.join(ROOT, '07_勘误工坊', 'reports')
RANK = {'conflict': 0, 'suspect': 1, 'unclear': 2, 'ok': 3}
EMOJI = {'conflict': '🔴', 'suspect': '🟠', 'unclear': '🟡', 'ok': '🟢'}


def evidence_hash(vj_obj):
    """基于 verdict_json 的实质内容（verdict + evidence.findings + summary）做稳定哈希，用于识别重复观点。"""
    core = {
        'verdict': vj_obj.get('verdict'),
        'summary': vj_obj.get('summary'),
        'findings': vj_obj.get('evidence', {}).get('findings') if isinstance(vj_obj.get('evidence'), dict) else None,
    }
    s = json.dumps(core, ensure_ascii=False, sort_keys=True)
    return hashlib.sha1(s.encode('utf-8')).hexdigest()[:12]


def first_snippet(vj_obj):
    try:
        fs = vj_obj['evidence']['findings']
        if fs and isinstance(fs, list):
            return str(fs[0].get('card_snippet', ''))[:80].replace('\n', ' ')
    except Exception:
        pass
    return ''


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--json-only', action='store_true')
    ap.add_argument('--db', default=DB)
    a = ap.parse_args()

    conn = sqlite3.connect(a.db)
    rows = list(conn.execute(
        "SELECT id,probe_key,verdict_json,baseline_fp,ts,token_in,token_out "
        "FROM conflict_records WHERE review_status='pending' ORDER BY id"))
    conn.close()

    items, by_bucket, dup_groups = [], collections.defaultdict(list), collections.defaultdict(list)
    for (id_, key, vj, fp, ts, ti, to) in rows:
        try:
            obj = json.loads(vj)
        except Exception:
            obj = {'verdict': '<parse_err>', 'summary': vj[:100]}
        v = obj.get('verdict', '<none>')
        rec = {'id': id_, 'key': key, 'verdict': v,
               'summary': (obj.get('summary') or '')[:200],
               'snippet': first_snippet(obj),
               'ev_hash': evidence_hash(obj), 'baseline_fp': fp, 'ts': ts}
        by_bucket[v].append(rec)
        dup_groups[(key, rec['ev_hash'])].append(id_)
        items.append(rec)

    # 去重标记
    dup_of = {}
    for (key, h), ids in dup_groups.items():
        if len(ids) > 1:
            keep = min(ids)
            for i in ids:
                if i != keep:
                    dup_of[i] = keep
    for r in items:
        r['dup_of'] = dup_of.get(r['id'])

    # 每卡聚合（取最高优先级 verdict）
    per_key = {}
    for r in items:
        k = r['key']
        if k not in per_key or RANK.get(r['verdict'], 9) < RANK.get(per_key[k]['verdict'], 9):
            per_key[k] = r

    # 统计
    distinct_keys = len(per_key)
    key_buckets = collections.Counter(v['verdict'] for v in per_key.values())
    summary = {
        'total_pending': len(items),
        'distinct_keys': distinct_keys,
        'duplicates': len(dup_of),
        'by_verdict_rows': dict(collections.Counter(r['verdict'] for r in items)),
        'by_verdict_keys': dict(key_buckets),
    }
    print('=== 观点池分级统计 ===')
    print(f"  总条数 {summary['total_pending']} | 去重后卡数 {distinct_keys} | 重复 {summary['duplicates']}")
    for v in ['conflict', 'suspect', 'unclear', 'ok']:
        print(f"  {EMOJI.get(v,'?')} {v:9s} 行 {summary['by_verdict_rows'].get(v,0):3d} | 卡 {key_buckets.get(v,0):3d}")
    if a.json_only:
        print(json.dumps(summary, ensure_ascii=False)); return

    # 产出 Markdown 工作表
    os.makedirs(REPORT_DIR, exist_ok=True)
    ts = datetime.datetime.now().strftime('%Y%m%d_%H%M%S')
    md_path = os.path.join(REPORT_DIR, f'triage_{ts}.md')
    json_path = os.path.join(REPORT_DIR, f'triage_{ts}.json')
    lines = [f"# world-gov 终审工作表 · {ts}", '',
             f"总条数 {summary['total_pending']}｜去重后卡数 {distinct_keys}｜重复 {summary['duplicates']}", '',
             "> 终审方法：按 🔴 → 🟠 → 🟡 → 🟢 顺序；勾选列填 `valid` / `false_alarm` / `ignored`。",
             "> 落库命令：`world-gov conflict review <id> --status <结论>`", '']
    for v in ['conflict', 'suspect', 'unclear', 'ok']:
        recs = [r for r in items if r['verdict'] == v]
        if not recs:
            continue
        lines += [f"## {EMOJI.get(v,'?')} {v}（{len(recs)} 行 / {key_buckets.get(v,0)} 卡）", '',
                  '| id | 卡 key | 摘要 | 片段 | 重复 | 终审结论 |', '|---|---|---|---|---|---|']
        for r in sorted(recs, key=lambda x: x['id']):
            dup = f"dup#{r['dup_of']}" if r['dup_of'] else ''
            summ = r['summary'].replace('|', '/').replace('\n', ' ')[:90]
            snip = r['snippet'].replace('|', '/')[:50]
            lines.append(f"| {r['id']} | `{r['key']}` | {summ} | {snip} | {dup} | |")
        lines.append('')
    with open(md_path, 'w', encoding='utf-8') as f:
        f.write('\n'.join(lines))
    with open(json_path, 'w', encoding='utf-8') as f:
        json.dump({'summary': summary, 'items': items}, f, ensure_ascii=False, indent=2)
    print(f"\n工作表: {md_path}")
    print(f"数据:   {json_path}")


if __name__ == '__main__':
    main()