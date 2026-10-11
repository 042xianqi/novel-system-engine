#!/usr/bin/env python3
"""S10 后全链路验收：在**沙箱**里删掉 02_资料库 副本，跑通 MCP 全链。

验收目标（对齐「改造后最小验证用例」）：
  1. 沙箱 live 根**不含 02_资料库**（模拟 S10 后状态）
  2. funnel       → 读到 L0–L5 阶段
  3. ne_db        → 直连真值库
  4. ne_inject    → 卡片**从 DB 回退**取得（card_source=db_only，missing_refs 为空）
  5. ne_cardgen   → 从细纲抽工单
  6. ne_check     → 跑门禁
  7. chapter_plan → 识别第2章缺口

用法： python3 tools/s10_verify_pipeline.py [ne-mcp 二进制] [资料库根]
"""
import json, os, subprocess, sys, shutil, tempfile

BIN = sys.argv[1] if len(sys.argv) > 1 else "target/release/ne-mcp"
R = sys.argv[2] if len(sys.argv) > 2 else "/sdcard/Download/小说相关备份/projects/projects/系统/资料库"

ok = fail = 0
def chk(cond, msg, extra=""):
    global ok, fail
    if cond:
        ok += 1; print(f"  ✅ {msg}")
    else:
        fail += 1; print(f"  ❌ {msg} {extra}")

def rpc(*calls):
    inp = "".join(json.dumps(c, ensure_ascii=False) + "\n" for c in calls)
    p = subprocess.run([BIN, "--root", R], input=inp, capture_output=True, text=True, timeout=120)
    out = []
    for line in p.stdout.splitlines():
        line = line.strip()
        if line.startswith("{"):
            out.append(json.loads(line))
    return out

def call(name, args, root=None):
    saved = R
    conn = [
        {"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"v","version":"1"}}},
        {"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":name,"arguments":args}},
    ]
    res = rpc(*conn)
    r = [x for x in res if x.get("id") == 2][0]["result"]
    txt = r["content"][0]["text"]
    try: return json.loads(txt), r.get("isError", False)
    except Exception: return txt, r.get("isError", False)

print("=" * 60)
print("S10 全链路验收（沙箱：无 02_资料库）")
print("=" * 60)

# ── 0. 构造沙箱：复制 live 根，**但不含 02_资料库** ──────────────
sb = tempfile.mkdtemp(prefix="s10_verify_")
try:
    shutil.copy(f"{R}/novel.db", f"{sb}/novel.db")
    os.makedirs(f"{sb}/03_叙事规划", exist_ok=True)
    shutil.copytree(f"{R}/03_叙事规划/细纲", f"{sb}/03_叙事规划/细纲", dirs_exist_ok=True)
    os.makedirs(f"{sb}/05_管线框架", exist_ok=True)
    shutil.copy(f"{R}/05_管线框架/funnel.yaml", f"{sb}/05_管线框架/funnel.yaml")
    assert not os.path.exists(f"{sb}/02_资料库"), "沙箱不应含 02_资料库"
    print(f"\n[0] 沙箱就绪: {sb}")
    chk(not os.path.exists(f"{sb}/02_资料库"), "沙箱不含 02_资料库（模拟 S10 后）")

    R = sb  # 后续全部走沙箱

    # ── 1. funnel ─────────────────────────────────────────────
    print("\n[1] funnel")
    d, e = call("funnel", {})
    chk(not e, "funnel 无错误", str(d)[:200])
    if isinstance(d, dict):
        ids = [s["id"] for s in d.get("all", [])]
        chk("L0" in ids and "L5" in ids, f"读到 L0–L5（{len(ids)} 层）")

    # ── 2. ne_db ──────────────────────────────────────────────
    print("\n[2] ne_db")
    d, e = call("ne_db", {})
    chk(not e, "ne_db 无错误")
    chk(isinstance(d, dict) and "card_usage_top" in d, "ne_db 返回卡用量")

    # ── 3. ne_inject（核心：DB 回退）────────────────────────────
    print("\n[3] ne_inject（卡片须从 DB 回退取得）")
    d, e = call("ne_inject", {"volume": "01", "chapter": 2, "allow_missing": True})
    chk(not e, "ne_inject 无错误", str(d)[:200])
    if isinstance(d, dict):
        chk(d.get("ok") is True, "ok=True")
        chk(d.get("card_source") == "db_only", f"card_source=db_only（实得 {d.get('card_source')}）")
        chk(d.get("missing_refs") == [], f"missing_refs 为空（实得 {d.get('missing_refs')}）")
        pkt = d.get("packet", {})
        mats = pkt.get("materials", [])
        chk(len(mats) > 0, f"取到资料卡 {len(mats)} 张")
        full = [m for m in mats if m.get("is_full")]
        chk(all(m.get("content") for m in full), f"{len(full)} 张全文卡 content 非空（DB 正文确实读到）")
        chk(len(pkt.get("chars", [])) > 0, f"取到角色快照 {len(pkt.get('chars', []))} 个")

        # 章节规划：选项A 执行后，旧开局 2 章已清，应从第1章（涧底醒）重新开始
        cp = d.get("chapter_plan", {})
        chk(cp.get("has_gap") is False and cp.get("generated_chapters") == [],
            f"无残留章节：generated={cp.get('generated_chapters')}, has_gap={cp.get('has_gap')}")
        chk(cp.get("default_target") == 1, f"default_target=1（实得 {cp.get('default_target')}）")

    # ── 4. ne_cardgen ─────────────────────────────────────────
    print("\n[4] ne_cardgen")
    d, e = call("ne_cardgen", {"volume": "01"})
    chk(not e, "ne_cardgen 无错误（细纲在位）", str(d)[:200])

    # ── 5. ne_check ───────────────────────────────────────────
    print("\n[5] ne_check")
    sample = "陈寂蹲在断水渠边，指尖发凉。他摸了摸怀里那枚残响，没有说话。"
    d, e = call("ne_check", {"text": sample, "chapter": 2, "volume": "01"})
    chk(not e, "ne_check 无错误", str(d)[:200])
    if isinstance(d, dict):
        chk("verdict" in d, f"返回 verdict（{d.get('verdict')}）")

    # ── 6. ne_db_query 只读闸门 ────────────────────────────────
    print("\n[6] ne_db_query 写操作须被拒")
    d, e = call("ne_db_query", {"sql": "DELETE FROM materials"})
    chk(e is True, "DELETE 被拒绝（isError=true）")

    print("\n" + "=" * 60)
    print(f"  通过 {ok} / 失败 {fail}")
    print("=" * 60)
finally:
    shutil.rmtree(sb, ignore_errors=True)

sys.exit(1 if fail else 0)
