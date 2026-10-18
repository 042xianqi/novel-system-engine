#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P0 quansystem static probe. Output inventory.json."""
import os, re, json, sqlite3

LIB = os.environ.get('NOVEL_LIB_ROOT', os.environ.get("NOVEL_LIB_ROOT", "/sdcard/Download/小说相关备份/projects/projects/系统/资料库"))
ROOT = "/root/novel-system"
CRATES = os.path.join(ROOT, "crates")
OUT = os.path.join(ROOT, "tools/audit/inventory.json")

def rust_src_files():
    out = []
    for dp, _, fs in os.walk(CRATES):
        if 'target' in dp:
            continue
        for f in fs:
            if f.endswith('.rs') and '/src' in dp:
                out.append(os.path.join(dp, f))
    return out

def read(p):
    return open(p, encoding='utf-8', errors='ignore').read()

SRC = rust_src_files()
BLOBS = {f: read(f) for f in SRC}

def refs(term):
    out = set()
    for f, t in BLOBS.items():
        if term in t:
            out.add(f.split('/crates/')[-1])
    return sorted(out)

inv = {"meta": {}, "crates": [], "data_files": [], "db_tables": [], "prompts": [], "py_engines": []}

for d in sorted(os.listdir(CRATES)):
    srcd = os.path.join(CRATES, d, 'src')
    if not os.path.isdir(srcd):
        continue
    fns = []
    for f in sorted(os.listdir(srcd)):
        if not f.endswith('.rs'):
            continue
        t = read(os.path.join(srcd, f))
        fns.append({"file": f, "lines": t.count(chr(10)), "pub_fn": re.findall(r'pub fn (\w+)', t)})
    inv["crates"].append({"crate": d, "files": fns})

dd = os.path.join(LIB, "05_管线框架/data")
if os.path.isdir(dd):
    for f in sorted(os.listdir(dd)):
        if not f.endswith('.json'):
            continue
        r = refs(f[:-5])
        inv["data_files"].append({"file": f, "bytes": os.path.getsize(os.path.join(dd, f)), "ref_by": r, "used": bool(r)})

try:
    c = sqlite3.connect(os.path.join(LIB, "novel.db"))
    tabs = [r[0] for r in c.execute("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite%' ORDER BY name")]
    for t in tabs:
        n = c.execute('SELECT COUNT(*) FROM [%s]' % t).fetchone()[0]
        r = refs(t)
        inv["db_tables"].append({"table": t, "rows": n, "ref_by": r, "used": bool(r)})
except Exception as e:
    inv["meta"]["db_error"] = str(e)

pd = os.path.join(LIB, "05_管线框架/prompts")
if os.path.isdir(pd):
    for f in sorted(os.listdir(pd)):
        r = refs(f[:f.rfind('.')])
        inv["prompts"].append({"file": f, "ref_by": r, "used": bool(r)})

ed = os.path.join(LIB, "05_管线框架/engine")
if os.path.isdir(ed):
    for f in sorted(os.listdir(ed)):
        if f.endswith('.py'):
            inv["py_engines"].append({"file": f, "bytes": os.path.getsize(os.path.join(ed, f))})

inv["meta"].update({
    "lib": LIB, "crate_count": len(inv["crates"]), "src_files": len(SRC),
    "data_unused": [d["file"] for d in inv["data_files"] if not d["used"]],
    "tables_unused_nonempty": [t["table"] for t in inv["db_tables"] if not t["used"] and t["rows"] > 0],
    "tables_empty": [t["table"] for t in inv["db_tables"] if t["rows"] == 0],
    "prompts_unused": [p["file"] for p in inv["prompts"] if not p["used"]],
})
json.dump(inv, open(OUT, "w"), ensure_ascii=False, indent=1)
print("OK")
