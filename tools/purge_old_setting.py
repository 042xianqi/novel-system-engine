#!/usr/bin/env python3
"""选项A-扩展：清除「旧开局（渊汐城/沉渊巷/汐潮文明）」整套卡片及其派生数据。

删除集（20 码 → 16 行，因为 CIV-99-002/LAW-99-001/002/003 无实体行）：
  地点   LOC-99-001/002 + LOCA-99-001/002  （渊汐城 / 遗忘区·沉渊巷）
  文明   CIV-99-011 + CIVI-99-011          （汐潮文明）
  治理体 CIVI-99-051 + ORG-99-051          （潮汐议会）
  科技   TECH-99-002 / TECH-99-051          （水相熵流体悟 / 流体信息论）
  法则   CIV-99-002 / LAW-99-001/002/003    （无实体行，仅清引用）
  人物   CHAR-99-051 / CHAR-99-052 / CHAR-99-095（顾青灯）
  力量   POWR-99-026 + TECH-99-026          （熵增心法）
  底账   CIVI-99-901                        （文明全量普查登记表）

清理的派生表（无 Rust 侧重建命令，必须手工清）：
  materials_fts / *_state 投影 / card_usage / catalog_index / import_ledger /
  renumber_map / dynamics_events；并修 2 处保留下级卡内的文本引用。

用法：
  python3 tools/purge_old_setting.py --dry-run
  python3 tools/purge_old_setting.py --apply
"""
import argparse, glob, json, os, shutil, sqlite3, sys, datetime

R = os.environ.get("S10_ROOT", os.environ.get("NOVEL_LIB_ROOT", "/sdcard/Download/小说相关备份/projects/projects/系统/资料库"))
DB = os.path.join(R, "novel.db")
BK = os.path.join(R, "09_冷备份", "pre_s10_20261011_0148")
DUMP = os.path.join(BK, "old_setting_dump")

DEL = [
    "LOC-99-001", "LOC-99-002", "LOCA-99-001", "LOCA-99-002",   # 渊汐城/沉渊巷
    "CIV-99-011", "CIVI-99-011",                                # 汐潮文明
    "CIVI-99-051", "ORG-99-051",                                # 潮汐议会
    "TECH-99-002", "TECH-99-051",                               # 水相熵流/流体信息论
    "CIV-99-002", "LAW-99-001", "LAW-99-002", "LAW-99-003",     # 无实体行
    "CHAR-99-051", "CHAR-99-052", "CHAR-99-095",                # 人物
    "POWR-99-026", "TECH-99-026",                               # 熵增心法
    "CIVI-99-901",                                              # 文明普查底账
]

# 保留卡内的文本引用修正：{卡码: (旧行片段, 新行片段)}
TEXT_FIXES = {
    "CIVI-99-911": ("地理空壳文件（LOCA-99-002/005/011/012/016/017/023 等无标题）待清理或补全。",
                    "地理空壳文件（LOCA-99-005/011/012/016/017/023 等无标题）待清理或补全。"),
    "POWR-99-902": ("| POWR-99-026 | 热力学心法（第二定律） | 通用法则 | 热力学第二定律 |\n", ""),
}


def main():
    ap = argparse.ArgumentParser()
    g = ap.add_mutually_exclusive_group(required=True)
    g.add_argument("--dry-run", action="store_true")
    g.add_argument("--apply", action="store_true")
    args = ap.parse_args()

    con = sqlite3.connect(DB)
    con.row_factory = sqlite3.Row
    ph = ",".join("?" * len(DEL))

    # ── 盘点各表命中 ─────────────────────────────────────────
    plan = {}  # 表 -> (SQL, 参数, 说明)

    plan["materials"] = (f"DELETE FROM materials WHERE code IN ({ph})", DEL)
    plan["materials_fts"] = (f"DELETE FROM materials_fts WHERE code IN ({ph})", DEL)
    plan["card_usage"] = (f"DELETE FROM card_usage WHERE code IN ({ph})", DEL)
    plan["catalog_index"] = (f"DELETE FROM catalog_index WHERE code IN ({ph})", DEL)
    plan["renumber_map"] = (
        f"DELETE FROM renumber_map WHERE old_code IN ({ph}) OR new_code IN ({ph})", DEL + DEL)
    plan["import_ledger"] = (
        f"DELETE FROM import_ledger WHERE dst_key IN ({ph})", DEL)

    # *_state 投影：多为 code / source_card 两列
    state_tables = {
        "map_state": "code", "tech_state": "code", "power_state": "code",
        "civ_state": "code", "civ_relation_state": "code", "civ_sub_state": "code",
        "char_state": "char_code", "char_emotion": "char_code",
        "lore_state": "code", "culture_state": "code", "elixir_state": "code",
        "item_state": "code", "treasure_state": "code", "world_state": "code",
        "timeline_state": "code", "anomaly_state": "code", "technique_items": "code",
    }
    for t, col in state_tables.items():
        cols = [x[1] for x in con.execute(f"PRAGMA table_info({t})")]
        if col not in cols:
            continue
        plan[f"{t}({col})"] = (
            f"DELETE FROM {t} WHERE {col} IN ({ph})", DEL)

    # dynamics_events：FS-* 事件引用了已删伏笔（FS-LOC-99-001）
    plan["dynamics_events"] = (
        "DELETE FROM dynamics_events WHERE subject LIKE '%FS-LOC-99-001%'"
        " OR subject LIKE '%FS-LOC-99-002%' OR subject LIKE '%FS-ORG-99-051%'"
        " OR subject LIKE '%FS-TECH-99-051%'", ())

    print("=" * 64)
    print("将删除（按表统计）")
    print("=" * 64)
    total = 0
    for name, (sql, params) in plan.items():
        try:
            cnt_sql = sql.replace("DELETE FROM", "SELECT COUNT(*) FROM", 1)
            n = con.execute(cnt_sql, params).fetchone()[0]
        except Exception as e:
            n = f"(err {e})"
        if isinstance(n, int):
            total += n
        print(f"  {name:34s} {n}")
    print(f"  {'合计':34s} {total}")

    print("\n将修正保留卡内文本引用：")
    for code, (old, new) in TEXT_FIXES.items():
        body = con.execute("SELECT content FROM materials WHERE code=?", (code,)).fetchone()
        ok = body and old in body[0]
        print(f"  {code}: {'✅ 命中待替换片段' if ok else '⚠ 未命中（跳过）'}")

    if args.dry_run:
        print("\n[dry-run] 未写任何文件、未改库。")
        return

    # ── 归档 ─────────────────────────────────────────────────
    os.makedirs(DUMP, exist_ok=True)
    stamp = datetime.datetime.now().strftime("%Y%m%d_%H%M%S")
    snapshot = {}
    for code in DEL:
        row = con.execute("SELECT * FROM materials WHERE code=?", (code,)).fetchone()
        if row:
            snapshot[code] = dict(row)
    json.dump(snapshot, open(f"{DUMP}/materials_removed_{stamp}.json", "w"),
              ensure_ascii=False, indent=1)
    # 卫星表快照
    sat = {}
    for name, (sql, params) in plan.items():
        t = name.split("(")[0]
        cols_sql = sql.replace("DELETE FROM", "SELECT * FROM", 1)
        try:
            rows = [dict(r) for r in con.execute(cols_sql, params)]
            if rows:
                sat[name] = rows
        except Exception:
            pass
    json.dump(sat, open(f"{DUMP}/derived_tables_removed_{stamp}.json", "w"),
              ensure_ascii=False, indent=1)
    print(f"\n📦 归档 -> {DUMP}")

    # ── 备份 ─────────────────────────────────────────────────
    bak = os.path.join(BK, f"novel.db.before-oldsetting-purge-{stamp}")
    con.close()
    shutil.copy2(DB, bak)
    print(f"💾 备份 -> 09_冷备份/pre_s10_20261011_0148/{os.path.basename(bak)}")

    # ── 执行 ─────────────────────────────────────────────────
    con = sqlite3.connect(DB)
    cur = con.cursor()
    cur.execute("BEGIN")
    for name, (sql, params) in plan.items():
        cur.execute(sql, params)
        if cur.rowcount:
            print(f"  🗑  {name}: {cur.rowcount} 行")
    # 文本修正
    for code, (old, new) in TEXT_FIXES.items():
        row = cur.execute("SELECT content FROM materials WHERE code=?", (code,)).fetchone()
        if row and row[0] and old in row[0]:
            cur.execute("UPDATE materials SET content=?, content_chars=length(?) WHERE code=?",
                        (row[0].replace(old, new), row[0].replace(old, new), code))
            print(f"  ✏  {code}: 修正文本引用")
    con.commit()

    # ── 校验 ─────────────────────────────────────────────────
    print("\n" + "=" * 64)
    print("校验")
    print("=" * 64)
    left = con.execute(f"SELECT COUNT(*) FROM materials WHERE code IN ({ph})", DEL).fetchone()[0]
    print(f"  materials 残留旧设定行: {left}  {'✅' if left == 0 else '❌'}")
    print(f"  materials 总数: {con.execute('SELECT COUNT(*) FROM materials').fetchone()[0]}")
    print(f"  materials_fts 总数: {con.execute('SELECT COUNT(*) FROM materials_fts').fetchone()[0]}")
    ic = con.execute("PRAGMA integrity_check").fetchone()[0]
    print(f"  integrity_check: {ic}")
    con.close()


if __name__ == "__main__":
    main()
