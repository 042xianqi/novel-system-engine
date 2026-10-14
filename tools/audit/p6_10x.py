#!/usr/bin/env python3
# P6: 全模块总体 10 轮稳定性测试
import subprocess, json, sys
R="/sdcard/Download/小说相关备份/projects/projects/系统/资料库"
NA="/root/novel-system/target/release/ne-agent"
PRB="/root/novel-system/target/release/examples/p1_probe"
G07="/root/novel-system/target/release/examples/g07test"

def run(cmd):
    return subprocess.run(cmd, capture_output=True, text=True, timeout=120)

results=[]
for i in range(1,11):
    row={"round":i}
    # A 注入侧
    try:
        r=run([NA,"--work-root",R,"novel","--volume","\u537701","--chap-id","2","--lib",R,"--prompt-only","--json"])
        t=json.loads(r.stdout)["prompt"]
        row["inject"]=len(t)
    except Exception as e:
        row["inject"]="ERR"
    # B 探针
    try:
        r=run([PRB])
        row["probe"]=r.stdout.strip().split("\n")[-1]
    except Exception:
        row["probe"]="ERR"
    # C G07/G13
    try:
        r=run([G07])
        row["ooc"]="2角色" if "2 \u4e2a\u89d2\u8272" in r.stdout else "?"
        row["g13"]="offline13" if "\u79bb\u7ebf 13" in r.stdout else "?"
    except Exception:
        row["ooc"]="ERR"
    results.append(row)
    print("round %2d inject=%s probe=%s ooc=%s g13=%s"%(i,row.get("inject"),row.get("probe","")[:14],row.get("ooc"),row.get("g13")))

# 稳定性判定
inj=[r.get("inject") for r in results]
stable = len(set(str(x) for x in inj))==1 and all("0 FAIL" in str(r.get("probe","")) for r in results)
print("\n=== 10 \u8f6e\u7ed3\u679c ===")
print("inject \u5b57\u6570\u96c6:", sorted(set(str(x) for x in inj)))
print("\u7a33\u5b9a\u6027:", "PASS" if stable else "CHECK")
