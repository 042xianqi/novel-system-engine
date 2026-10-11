#!/usr/bin/env python3
"""把 workflows/taixu/*.json 安装进太墟 App 的 workflows 表。

为什么这样做（不是"绕开"而是当前唯一通路）：
    太墟工作流只存在于 App 的 Room DB（表 `workflows.jsonContent`），
    没有文件级导入通道。故：host root 拷 DB → /sdcard/Download（与 Linux
    沙箱共享）→ 这里用 sqlite3 写入 → 拷回 → 重启 App 让 Room 重新加载。

安全：
    * 写前自动备份整库到 workflows/_backup/<时间戳>/taixu.db
    * 用 SQLite 事务；任何一条失败整体回滚
    * 安装前用 WorkflowValidator 的规则做静态校验（不用起 Kotlin 也能挡住
      坏定义：id/name 非空、节点唯一、超时范围、DAG 无环、边引用有效…）

用法：
    python3 workflows/install_taixu_workflows.py --db /path/taixu.db [--dry-run]
    python3 workflows/install_taixu_workflows.py --list
    python3 workflows/install_taixu_workflows.py --db … --uninstall ne_build
"""
from __future__ import annotations

import argparse
import json
import shutil
import sqlite3
import sys
import time
from pathlib import Path

WF_DIR = Path(__file__).resolve().parent / "taixu"
BACKUP_DIR = Path(__file__).resolve().parent / "_backup"

VALID_NODE_TYPES = {
    "TRIGGER", "BASH_COMMAND", "PROCESS_SERVICE", "AGENT_INFERENCE",
    "SUBAGENT_DELEGATE", "TAIXU_BUILD", "CONDITION_BRANCH", "HUMAN_APPROVAL",
    "HOST_ACTION", "DELAY", "SET_VARIABLE", "TERMINAL_OUTPUT",
}
VALID_FAILURE = {"ABORT", "CONTINUE", "RETRY_ONCE", "ASK_USER"}
VALID_PORTS = {"output", "success", "failure"}


def load_defs() -> list[dict]:
    defs = []
    for f in sorted(WF_DIR.glob("*.json")):
        defs.append((f, json.loads(f.read_text(encoding="utf-8"))))
    return defs


def validate(d: dict) -> list[str]:
    """复刻 WorkflowValidator 的规则（够挡住结构性错误）。"""
    errs: list[str] = []

    def err(msg):
        errs.append(f"{d.get('id', '?')}: {msg}")

    if not str(d.get("id", "")).strip():
        errs.append("id 不能为空")
    if not str(d.get("name", "")).strip():
        errs.append("name 不能为空")
    nodes = d.get("nodes") or []
    if not nodes:
        errs.append("nodes 不能为空")
        return errs

    ids = [n.get("id") for n in nodes]
    if len(ids) != len(set(ids)):
        errs.append("节点 ID 必须唯一")
    for n in nodes:
        nid = n.get("id", "?")
        if not str(nid).strip():
            errs.append("节点 ID 不能为空")
        if not str(n.get("title", "")).strip():
            errs.append(f"节点 {nid} 标题不能为空")
        if n.get("type") not in VALID_NODE_TYPES:
            errs.append(f"节点 {nid} 类型非法：{n.get('type')}")
        t = n.get("timeoutSeconds", 300)
        if not (isinstance(t, int) and 1 <= t <= 3600):
            errs.append(f"节点 {nid} 超时须为 1–3600：{t}")
        fp = n.get("failurePolicy", "ABORT")
        if fp not in VALID_FAILURE:
            errs.append(f"节点 {nid} failurePolicy 非法：{fp}")

    known = set(ids)
    edges = d.get("edges") or []
    eids = [e.get("id") for e in edges]
    if len(eids) != len(set(eids)):
        errs.append("连接 ID 必须唯一")
    for e in edges:
        if e.get("fromNodeId") not in known or e.get("toNodeId") not in known:
            errs.append(f"边 {e.get('id')} 引用了不存在的节点")
        if e.get("fromNodeId") == e.get("toNodeId"):
            errs.append(f"边 {e.get('id')} 自连接")
        if str(e.get("fromPort", "output")).lower() not in VALID_PORTS:
            errs.append(f"边 {e.get('id')} 输出端口非法：{e.get('fromPort')}")
        if str(e.get("toPort", "input")).lower() != "input":
            errs.append(f"边 {e.get('id')} 输入端口只能是 input")

    # DAG 环检测（Kahn）
    indeg = {n: 0 for n in ids}
    out: dict[str, list[str]] = {}
    for e in edges:
        if e.get("fromNodeId") in indeg and e.get("toNodeId") in indeg:
            indeg[e["toNodeId"]] += 1
            out.setdefault(e["fromNodeId"], []).append(e["toNodeId"])
    ready = [n for n, v in indeg.items() if v == 0]
    seen = 0
    while ready:
        n = ready.pop()
        seen += 1
        for m in out.get(n, []):
            indeg[m] -= 1
            if indeg[m] == 0:
                ready.append(m)
    if seen != len(ids):
        errs.append("工作流必须是无环图（DAG）")
    return errs


def backup(db: Path) -> Path:
    BACKUP_DIR.mkdir(parents=True, exist_ok=True)
    stamp = time.strftime("%Y%m%d-%H%M%S")
    dest = BACKUP_DIR / stamp
    dest.mkdir(parents=True, exist_ok=True)
    for suffix in ("", "-wal", "-shm"):
        src = Path(str(db) + suffix)
        if src.exists():
            shutil.copy2(src, dest / src.name)
    return dest


def build_payloads(defs) -> list[tuple[Path, dict]]:
    """校验并规范化定义，返回 (文件, 待入库 payload)。"""
    now = int(time.time() * 1000)
    out = []
    for path, d in defs:
        errs = validate(d)
        if errs:
            print(f"✗ {path.name} 校验失败：", file=sys.stderr)
            for e in errs:
                print(f"    {e}", file=sys.stderr)
            raise SystemExit(1)
        payload = dict(d)
        payload.setdefault("iconName", "Play")
        payload.setdefault("description", "")
        payload.setdefault("category", "通用")
        payload["isBuiltin"] = False
        payload.setdefault("defaultVariables", {})
        for n in payload.get("nodes", []):
            n.setdefault("description", "")
            n.setdefault("config", {})
            n.setdefault("canvasX", 0.0)
            n.setdefault("canvasY", 0.0)
            n.setdefault("timeoutSeconds", 300)
            n.setdefault("failurePolicy", "ABORT")
        for e in payload.get("edges", []):
            e.setdefault("fromPort", "success")
            e.setdefault("toPort", "input")
        payload["createdAt"] = now
        payload["updatedAt"] = now
        out.append((path, payload))
    return out


def emit_sql(defs) -> str:
    """产出 INSERT/upsert SQL，供无 Python 的环境（如太墟 chroot）执行。"""
    now = int(time.time() * 1000)
    lines = ["BEGIN IMMEDIATE;"]
    for _path, p in build_payloads(defs):
        trig = p.get("trigger") or {}
        slash = trig.get("slashCommand") if trig.get("type") == "manual" else None
        j = json.dumps(p, ensure_ascii=False)

        def q(v):
            if v is None:
                return "NULL"
            return "'" + str(v).replace("'", "''") + "'"

        lines.append(
            "INSERT INTO workflows (id,name,description,category,isBuiltin,"
            "slashCommand,jsonContent,updatedAt) VALUES ("
            f"{q(p['id'])},{q(p['name'])},{q(p['description'])},{q(p['category'])},0,"
            f"{q(slash)},{q(j)},{now}) "
            "ON CONFLICT(id) DO UPDATE SET name=excluded.name,"
            "description=excluded.description,category=excluded.category,"
            "isBuiltin=0,slashCommand=excluded.slashCommand,"
            "jsonContent=excluded.jsonContent,updatedAt=excluded.updatedAt;"
        )
    lines.append("COMMIT;")
    return "\n".join(lines) + "\n"


def install(db: Path, defs, dry: bool) -> int:
    con = sqlite3.connect(str(db))
    try:
        cols = {r[1] for r in con.execute("PRAGMA table_info(workflows)")}
        required = {"id", "name", "description", "category", "isBuiltin",
                    "slashCommand", "jsonContent", "updatedAt"}
        if not required <= cols:
            print(f"✗ workflows 表结构不符，缺：{sorted(required - cols)}", file=sys.stderr)
            return 2

        now = int(time.time() * 1000)
        installed = []
        for path, payload in build_payloads(defs):
            slash = None
            trig = payload.get("trigger") or {}
            if trig.get("type") == "manual":
                slash = trig.get("slashCommand")

            if not dry:
                con.execute(
                    "INSERT INTO workflows (id,name,description,category,isBuiltin,"
                    "slashCommand,jsonContent,updatedAt) VALUES (?,?,?,?,0,?,?,?) "
                    "ON CONFLICT(id) DO UPDATE SET name=excluded.name,"
                    "description=excluded.description,category=excluded.category,"
                    "isBuiltin=0,slashCommand=excluded.slashCommand,"
                    "jsonContent=excluded.jsonContent,updatedAt=excluded.updatedAt",
                    (payload["id"], payload["name"], payload["description"],
                     payload["category"], slash,
                     json.dumps(payload, ensure_ascii=False), now),
                )
            installed.append(payload["id"])

        if dry:
            print("（dry-run，未写入）将安装：")
        else:
            con.commit()
            print("已安装：")
        for i in installed:
            print(f"  • {i}")
        return 0
    finally:
        con.close()


def list_current(db: Path) -> int:
    con = sqlite3.connect(str(db))
    try:
        rows = con.execute(
            "SELECT id,name,category,isBuiltin FROM workflows ORDER BY category,id"
        ).fetchall()
        print(f"{'ID':42} {'名称':28} {'分类':10} 内置")
        for r in rows:
            print(f"{r[0]:42} {r[1]:28} {r[2]:10} {'是' if r[3] else '否'}")
        return 0
    finally:
        con.close()


def uninstall(db: Path, ids: list[str]) -> int:
    con = sqlite3.connect(str(db))
    try:
        for i in ids:
            con.execute("DELETE FROM workflows WHERE id=? AND isBuiltin=0", (i,))
        con.commit()
        print(f"已卸载 {len(ids)} 个（仅 isBuiltin=0 的会被删）")
        return 0
    finally:
        con.close()


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", type=Path, help="taixu.db 路径")
    ap.add_argument("--dry-run", action="store_true", help="只校验不写")
    ap.add_argument("--list", action="store_true", help="列出库中现有工作流")
    ap.add_argument("--uninstall", nargs="+", metavar="ID", help="按 id 卸载")
    ap.add_argument("--no-backup", action="store_true")
    ap.add_argument("--emit-sql", action="store_true",
                    help="把 upsert SQL 输出到 stdout（供无 Python 环境执行）")
    args = ap.parse_args()

    defs = load_defs()
    if not defs:
        print("✗ workflows/taixu/ 下没有 .json", file=sys.stderr)
        return 1

    if args.emit_sql:
        sys.stdout.write(emit_sql(defs))
        return 0

    if args.list:
        if not args.db:
            print("--list 需要 --db", file=sys.stderr)
            return 2
        return list_current(args.db)

    # 默认行为：校验（即使没给 --db 也先校验）
    if not args.db:
        all_ok = True
        for path, d in defs:
            errs = validate(d)
            if errs:
                all_ok = False
                print(f"✗ {path.name}")
                for e in errs:
                    print(f"    {e}")
            else:
                print(f"✓ {path.name}")
        return 0 if all_ok else 1

    if args.uninstall:
        b = backup(args.db)
        print(f"已备份 → {b}")
        return uninstall(args.db, args.uninstall)

    if not args.dry_run and not args.no_backup:
        b = backup(args.db)
        print(f"已备份 → {b}")
    return install(args.db, defs, args.dry_run)


if __name__ == "__main__":
    sys.exit(main())
