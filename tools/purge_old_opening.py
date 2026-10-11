#!/usr/bin/env python3
"""选项A：归档并移除 DB 中「旧开局（渊汐城/沉渊巷）」章节与其伏笔。

背景：正式细纲 03_叙事规划/细纲/卷01.md 为「青牛镇/太玄宗」开局，
      而 novel.db 的 chapters（第1、3章）与 foreshadow（12 条）仍是已作废的
      「渊汐城/沉渊巷」旧开局口径。旧开局词在 1800 章细纲中出现 0 次。

本脚本：
  1. 归档（JSON + 磁盘正文副本）到 09_冷备份/pre_s10_20261011_0148/old_chapter_dump/
  2. 备份 novel.db → novel.db.before-chapter-purge
  3. 删除 chapters 旧开局行 + 与之锚定的 foreshadow 行

用法：
  python3 tools/purge_old_opening.py --dry-run   # 只归档+打印将删内容
  python3 tools/purge_old_opening.py --apply     # 归档 + 备份 + 删除
"""
import argparse, json, os, shutil, sqlite3, sys, datetime

R = os.environ.get("S10_ROOT", "/sdcard/Download/小说相关备份/projects/projects/系统/资料库")
DB = os.path.join(R, "novel.db")
BK = os.path.join(R, "09_冷备份", "pre_s10_20261011_0148")
DUMP = os.path.join(BK, "old_chapter_dump")

# 旧开局「指纹」词：出现在章节 outline 里即视为旧口径
# 旧开局「指纹」词：出现在章节 outline 里即视为旧口径。
#
# ⚠ 2026-10-11 修正：**不得**把「推演面板」列为作废标记。
#   `推演面板` 是《残响之河》**正式设定**：EVT-99-005（定稿·D4正典）明写
#   「金手指 = 推演面板 U0–U8 + 存在之基」，且 卷01.md 出现 5 次、全文「推演」147 次。
#   初版误将其列为标记，经取证已剔除（未造成误删：被删行的命中词是 渊汐城/沉渊巷/无编码）。
OLD_MARKERS = ["渊汐城", "沉渊巷", "无编码", "汐潮"]


def is_old_opening(text: str) -> bool:
    t = text or ""
    return any(m in t for m in OLD_MARKERS)


def official_chapter_titles():
    """正式细纲 卷01 的 `章号 -> 标题`（唯一权威来源）。"""
    import re
    p = os.path.join(R, "03_叙事规划", "细纲", "卷01.md")
    if not os.path.exists(p):
        p = os.path.join(R, "09_冷备份", "pre_renumber_20261010_1505",
                         "03_叙事规划", "细纲", "卷01.md")
    titles = {}
    if os.path.exists(p):
        txt = open(p, encoding="utf-8").read()
        for m in re.finditer(r"^### 第(\d+)章\s*(.+)$", txt, re.M):
            titles[int(m.group(1))] = m.group(2).strip()
    return titles


def foreshadow_is_legacy(content: str, titles: dict) -> bool:
    """伏笔作废判定：内容引用了旧开局词，**或**其「首现第N章 XXX」
    的标题与正式细纲第 N 章标题不一致（说明锚在已作废的章上）。"""
    import re
    if is_old_opening(content):
        return True
    m = re.search(r"首现第(\d+)章\s*([^，,]+)", content or "")
    if m:
        n, cited = int(m.group(1)), m.group(2).strip()
        official = titles.get(n, "")
        if cited and official and cited != official:
            return True
    return False


def main():
    ap = argparse.ArgumentParser()
    g = ap.add_mutually_exclusive_group(required=True)
    g.add_argument("--dry-run", action="store_true")
    g.add_argument("--apply", action="store_true")
    args = ap.parse_args()

    if not os.path.exists(DB):
        sys.exit(f"❌ 真值库不存在: {DB}")
    con = sqlite3.connect(DB)
    con.row_factory = sqlite3.Row

    # ── 1. 找出旧开局章节 ─────────────────────────────────────
    ch_cols = [x[1] for x in con.execute("PRAGMA table_info(chapters)")]
    chapters = [dict(zip(ch_cols, r)) for r in con.execute("SELECT * FROM chapters")]
    old_ch = [c for c in chapters if is_old_opening(c.get("title")) or is_old_opening(c.get("outline"))]
    old_nos = sorted({c["chapter_no"] for c in old_ch})
    print(f"chapters 共 {len(chapters)} 行；判定为旧开局 {len(old_ch)} 行 -> 章号 {old_nos}")

    # ── 2. 找出锚定旧开局的伏笔 ───────────────────────────────
    official = official_chapter_titles()
    if not official:
        sys.exit("❌ 取不到正式细纲标题（卷01.md），拒绝判废以免误删")
    fs_cols = [x[1] for x in con.execute("PRAGMA table_info(foreshadow)")]
    fores = [dict(zip(fs_cols, r)) for r in con.execute("SELECT * FROM foreshadow")]
    old_fs = [f for f in fores if foreshadow_is_legacy(f.get("content"), official)]
    print(f"foreshadow 共 {len(fores)} 行；判定为旧开局锚定 {len(old_fs)} 行")
    for f in old_fs:
        print(f"    {f.get('id')}  首现 第{f.get('planted_chapter')}章  {(f.get('content') or '')[:44]}")

    if not old_ch and not old_fs:
        print("没有可清理的行，退出。")
        return

    # ── 3. 归档 ──────────────────────────────────────────────
    os.makedirs(DUMP, exist_ok=True)
    stamp = datetime.datetime.now().strftime("%Y%m%d_%H%M%S")
    json.dump(old_ch, open(f"{DUMP}/chapters_removed_{stamp}.json", "w"),
              ensure_ascii=False, indent=1)
    json.dump(old_fs, open(f"{DUMP}/foreshadow_removed_{stamp}.json", "w"),
              ensure_ascii=False, indent=1)
    # 磁盘正文副本（防丢）
    src_ch = os.path.join(R, "04_正文", "卷01", "第1章.md")
    if os.path.exists(src_ch):
        shutil.copy2(src_ch, f"{DUMP}/第1章.md.bak")
        print(f"  已归档磁盘正文副本 -> old_chapter_dump/第1章.md.bak")
    print(f"📦 归档完成: {DUMP}")

    if args.dry_run:
        print("\n[dry-run] 未做任何删除。将执行：")
        print(f"   DELETE chapters WHERE chapter_no IN {tuple(old_nos)}")
        print(f"   DELETE foreshadow WHERE id IN {tuple(f.get('id') for f in old_fs)}")
        return

    # ── 4. 备份 DB ───────────────────────────────────────────
    bak = f"{DB}.before-chapter-purge-{stamp}"
    shutil.copy2(DB, bak)
    print(f"💾 已备份 DB -> {os.path.basename(bak)}")

    # ── 5. 删除 ──────────────────────────────────────────────
    cur = con.cursor()
    cur.execute("BEGIN")
    if old_nos:
        q = f"DELETE FROM chapters WHERE chapter_no IN ({','.join('?' * len(old_nos))})"
        cur.execute(q, old_nos)
        print(f"🗑  删除 chapters {cur.rowcount} 行")
    if old_fs:
        ids = [f["id"] for f in old_fs]
        q = f"DELETE FROM foreshadow WHERE id IN ({','.join('?' * len(ids))})"
        cur.execute(q, ids)
        print(f"🗑  删除 foreshadow {cur.rowcount} 行")
    con.commit()

    # ── 6. 校验 ──────────────────────────────────────────────
    n_ch = con.execute("SELECT COUNT(*) FROM chapters").fetchone()[0]
    n_fs = con.execute("SELECT COUNT(*) FROM foreshadow").fetchone()[0]
    print(f"\n✅ 完成：chapters {len(chapters)} -> {n_ch}；foreshadow {len(fores)} -> {n_fs}")
    con.close()


if __name__ == "__main__":
    main()
