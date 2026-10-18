#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""init_newbook.py —— 从 schema.sql 建一个全新小说资料库骨架。

用法:
    python3 tools/newbook/init_newbook.py <新库根目录> [--title 书名] [--protagonist 主角]

产出:
    <root>/novel.db                            —— 64 表全量 schema（空白）
    <root>/05_管线框架/data/book_profile.json  —— 书档案（书名/主角/禁词…）
    <root>/03_叙事规划/细纲/                    —— 细纲目录
    <root>/04_正文/卷01/                        —— 正文目录
    <root>/02_资料库/                           —— 资料卡目录

新库默认**无任何《残响之河》专属值**；写章前用 `book_profile.json` 填本书设定。
"""
import argparse
import json
import os
import sqlite3
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
SCHEMA = os.path.join(HERE, "schema.sql")


def create_db(root: str) -> int:
    raw = open(SCHEMA, encoding="utf-8").read()
    sql = "\n".join(l for l in raw.splitlines() if not l.lstrip().startswith("--"))
    con = sqlite3.connect(os.path.join(root, "novel.db"))
    con.executescript(sql)
    con.commit()
    n = con.execute(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'"
    ).fetchone()[0]
    con.close()
    return n


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("root", help="新库根目录")
    ap.add_argument("--title", default="未命名", help="书名")
    ap.add_argument("--protagonist", default="主角", help="主角名")
    a = ap.parse_args()
    root = os.path.abspath(a.root)
    for d in ["05_管线框架/data", "03_叙事规划/细纲", "04_正文/卷01", "02_资料库"]:
        os.makedirs(os.path.join(root, d), exist_ok=True)
    n = create_db(root)
    bp = {
        "title": a.title,
        "protagonist": a.protagonist,
        "horror_keywords": [],
        "ability_keywords": [],
        "cost_keywords": ["代价", "失去", "模糊", "遗忘", "消散", "疼", "痛", "裂痕"],
        "g01_b": [],
        "g01_c": [],
        "g01_d": [],
        "canon_family": [],
        "canon_names": [],
        "myth_banned": [],
        "banned_words": [],
        "novel_id": os.path.basename(root),
    }
    bp_path = os.path.join(root, "05_管线框架/data/book_profile.json")
    json.dump(bp, open(bp_path, "w", encoding="utf-8"), ensure_ascii=False, indent=2)
    print(f"[OK] 新库已建：{root}")
    print(f"     表数 = {n}")
    print(f"     书档案 = {bp_path}（请按本书设定补齐各词表）")
    return 0


if __name__ == "__main__":
    sys.exit(main())