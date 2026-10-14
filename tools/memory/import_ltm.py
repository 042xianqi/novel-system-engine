#!/usr/bin/env python3
"""记忆子系统素材导入工具（需求文档 §五 · 一次性导入）。

用法:
    python3 tools/memory/import_ltm.py <库根>            # 导入 L0/L0.5/L1/L2 + 锚点
    python3 tools/memory/import_ltm.py <库根> --dry-run  # 只打印，不写库

原则（§五.2）: 运行期不再读 md；本工具是**唯一**的 md → DB 入口。
导入后修改设定请走 DB（system_config / long_term_memory），md 仅作外部备份。
"""
import json
import os
import sqlite3
import sys

# ── L0-L2 素材 → long_term_memory（category, key, 相对路径, priority）
SOURCES = [
    ("顶层设定", "整本概括", "03_叙事规划/00_整本概括.md", 10),
    ("顶层设定", "故事脊梁", "03_叙事规划/00_故事脊梁.md", 11),
    ("分卷大纲", "叙事总纲", "03_叙事规划/00_叙事总纲.md", 20),
    ("分卷大纲", "宏大对抗总规划", "03_叙事规划/00_宏大对抗总规划.md", 21),
    ("分卷大纲", "规划约束护栏", "03_叙事规划/护栏·规划约束.md", 22),
]

# 跨卷锚点元（§二 SYS_LAYER_1「跨卷锚点元规则」，来自细纲目录既有表）
ANCHOR_PATH = "03_叙事规划/细纲/跨卷线锚点表.md"

# 全局铁则素材（本轮 v5 新建）
IRONLAW_DIR = "02_资料库/_规范/全局铁则"
IRONLAW_FILES = [
    ("世界观铁则", "全局世界观铁则", "_全局世界观铁则.md", 1),
    ("角色静态档案", "全局角色档案库", "_全局角色档案库.md", 2),
]


def read(root, rel):
    p = os.path.join(root, rel)
    if not os.path.exists(p):
        return None
    with open(p, encoding="utf-8", errors="replace") as f:
        return f.read()


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    root = sys.argv[1]
    dry = "--dry-run" in sys.argv
    db = os.path.join(root, "novel.db")
    if not os.path.exists(db):
        print(f"库根无 novel.db: {root}")
        return 2

    conn = sqlite3.connect(db)
    # 确保表存在（schema 由 Rust init_schema 建；此处兜底）
    schema = os.path.join(os.path.dirname(__file__), "..", "..",
                          "crates", "ne-agent", "src", "memory_schema.sql")
    if os.path.exists(schema):
        with open(schema, encoding="utf-8") as f:
            conn.executescript(f.read())

    rows = []
    missing = []
    for cat, key, rel, pri in SOURCES:
        txt = read(root, rel)
        if txt is None:
            missing.append(rel)
            continue
        rows.append((cat, key, txt, pri, rel))
    for cat, key, fn, pri in IRONLAW_FILES:
        rel = f"{IRONLAW_DIR}/{fn}"
        txt = read(root, rel)
        if txt is None:
            missing.append(rel)
            continue
        rows.append((cat, key, txt, pri, rel))
    # 锚点元
    at = read(root, ANCHOR_PATH)
    if at is None:
        missing.append(ANCHOR_PATH)
    else:
        rows.append(("锚点元", "跨卷线锚点表", at, 5, ANCHOR_PATH))

    print(f"待导入 {len(rows)} 条；缺失 {len(missing)} 条")
    for m in missing:
        print(f"  ⚠ 缺失: {m}")
    for cat, key, txt, pri, rel in rows:
        print(f"  {cat}/{key}: {len(txt)} 字符  ← {rel}")

    if dry:
        print("[dry-run] 未写库")
        return 0

    for cat, key, txt, pri, rel in rows:
        conn.execute(
            """INSERT INTO long_term_memory(category,key,content,priority,source)
               VALUES(?,?,?,?,?)
               ON CONFLICT(category,key) DO UPDATE SET
                 content=excluded.content, priority=excluded.priority,
                 source=excluded.source, updated_at=datetime('now')""",
            (cat, key, txt, pri, rel),
        )
    conn.commit()

    # 运行参数 seed（与 Rust MemConfig::seed 同值；按真实模型 128k 窗口取值）
    DEFAULTS = [
        ("mem.memory_inject_max_token", "24000",
         "记忆注入片段 token 上限（文档 280000 超真实窗口，按 128k 窗口取值）"),
        ("mem.total_input_max_token", "96000", "总输入 token 上限，超限直接报错不调模型"),
        ("mem.short_term_window", "8", "短期记忆窗口（章）"),
        ("mem.snapshot_interval", "30", "中期快照间隔（章）"),
        ("mem.model_context_window", "128000", "模型上下文窗口"),
        ("mem.model_max_output", "32000", "模型单次最大输出"),
    ]
    for k, v, note in DEFAULTS:
        conn.execute(
            "INSERT INTO system_config(key,value,note) VALUES(?,?,?) "
            "ON CONFLICT(key) DO NOTHING",
            (k, v, note),
        )
    conn.commit()

    n = conn.execute("SELECT count(*) FROM long_term_memory").fetchone()[0]
    print(f"导入完成；long_term_memory 现有 {n} 条")
    for r in conn.execute("SELECT category, key, length(content) FROM long_term_memory ORDER BY priority"):
        print("  ", r)
    return 0


if __name__ == "__main__":
    sys.exit(main())