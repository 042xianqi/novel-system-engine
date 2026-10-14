#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""export_schema.py —— 从既有真库导出**通用建库 schema**。

排除 FTS5 shadow 表（*_data/_idx/_docsize/_config/_content）与 sqlite_* 内部表，
避免 executescript 重建时 "table already exists"。

用法:
    python3 tools/newbook/export_schema.py <真库 novel.db> [输出 schema.sql]
"""
import sqlite3
import sys
import os

SHADOW_SUFFIX = ("_data", "_idx", "_docsize", "_config", "_content")


def is_shadow(name: str) -> bool:
    return any(name.endswith(s) for s in SHADOW_SUFFIX)


def main() -> int:
    src = sys.argv[1] if len(sys.argv) > 1 else "/sdcard/Download/小说相关备份/projects/projects/系统/资料库/novel.db"
    out = sys.argv[2] if len(sys.argv) > 2 else os.path.join(os.path.dirname(os.path.abspath(__file__)), "schema.sql")
    con = sqlite3.connect(src)
    rows = con.execute(
        "SELECT type,name,sql FROM sqlite_master WHERE sql IS NOT NULL "
        "ORDER BY CASE type WHEN 'table' THEN 1 WHEN 'index' THEN 2 WHEN 'trigger' THEN 3 ELSE 4 END, name"
    ).fetchall()
    tables, others = [], []
    for t, n, s in rows:
        if n.startswith("sqlite_") or is_shadow(n):
            continue
        (tables if t == "table" else others).append(s + ";")
    head = [
        "-- 通用建库 schema（从既有真库导出；FTS shadow 表已剔除，由虚表自动重建）",
        "-- 用法: python3 tools/newbook/init_newbook.py <新库根>",
        "",
    ]
    open(out, "w", encoding="utf-8").write("\n".join(head + tables + others) + "\n")
    print(f"导出 {len(tables)} 表 + {len(others)} 索引/触发器 -> {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())