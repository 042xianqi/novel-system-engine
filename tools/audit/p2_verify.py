#!/usr/bin/env python3
import subprocess, json, os
R = os.environ.get('NOVEL_LIB_ROOT', os.environ.get("NOVEL_LIB_ROOT", "/sdcard/Download/小说相关备份/projects/projects/系统/资料库"))
NA="/root/novel-system/target/release/ne-agent"
PRB="/root/novel-system/target/release/examples/p1_probe"
P=0; F=0
def ck(cond,msg):
    global P,F
    if cond: P+=1; print("  [OK] "+msg)
    else: F+=1; print("  [FAIL] "+msg)

# 1 注入侧
r=subprocess.run([NA,"--work-root",R,"novel","--volume","\u537701","--chap-id","2","--lib",R,"--prompt-only","--json"],capture_output=True,text=True)
try:
    t=json.loads(r.stdout)["prompt"]
    ks=["\u89d2\u8272\u60c5\u7eea\u5feb\u7167","\u6280\u6cd5\u5361","\u91d1\u53e5\u53c2\u8003","\u610f\u8c61\u5e93\u53c2\u8003","\u8f76\u4e8b","\u4e16\u754c\u52bf\u529b\u72b6\u6001","\u52a8\u529b\u5b66\u6a21\u578b\u63a8\u6f14","\u4e16\u754c\u7269\u7406\u63a8\u6f14","\u4e16\u754c\u72b6\u6001\u8868","\u5168\u6587\u68c0\u7d22"]
    n=sum(1 for k in ks if k in t)
    ck(n>=10, "\u6ce8\u5165\u4fa7\u5757\u6570=%d/10"%n)
except Exception as e:
    ck(False,"\u6ce8\u5165\u4fa7\u89e3\u6790\u5931\u8d25 %s"%e)

# 2 探针
r=subprocess.run([PRB],capture_output=True,text=True)
last=r.stdout.strip().split("\n")[-1]
ck("0 FAIL" in last, "\u63a2\u9488: "+last)

# 3 门禁 dry-run
r=subprocess.run([NA,"--work-root",R,"novel","--volume","\u537701","--chap-id","1","--lib",R,"--dry-run","--json"],capture_output=True,text=True)
try:
    d=json.loads(r.stdout)
    ck("verdict" in d, "\u95e8\u7981 verdict=%s p0=%d p1=%d"%(d.get("verdict"),d.get("p0",-1),d.get("p1",-1)))
except Exception as e:
    ck(False,"\u95e8\u7981\u89e3\u6790\u5931\u8d25 %s"%e)

print("\n=== %d OK / %d FAIL ==="%(P,F))
