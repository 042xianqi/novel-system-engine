#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""E2E：模拟设置中心的一次完整保存，验证三处同步 + 还原。

步骤：
  1) GET 读现状（拿到 provider 列表与掩码）
  2) POST 改 agens 模型 → glm-5.3-flash
  3) 校验 OMP 池 / data 池 / 两份 config.yml 全部更新
  4) 校验 l5.py load_config() 读到新模型
  5) POST 改回 → deepseek-v4.1-flash
  6) 最后校验全部回到基线

不做任何"应该"的推断，每步都打印实测值。
"""
import importlib.util
import json
import subprocess
import sys

API = "http://127.0.0.1:8270/api/config/model"
OMP = "/root/.omp/agent/models.yml"
OMP_CFG = "/root/.omp/agent/config.yml"
DATA = "/sdcard/Download/小说相关备份/projects/projects/系统/资料库/05_管线框架/data/models.yml"
DATA_CFG = "/sdcard/Download/小说相关备份/projects/projects/系统/资料库/05_管线框架/data/config.yml"
L5 = "/sdcard/Download/小说相关备份/projects/projects/系统/资料库/05_管线框架/l5.py"

fail = []


def chk(cond, label, extra=""):
    if cond:
        print("  ✅ %s %s" % (label, extra))
    else:
        print("  ❌ %s %s" % (label, extra))
        fail.append(label)


def curl(method, body=None):
    cmd = ["curl", "-s", "-X", method, API, "-H", "Content-Type: application/json"]
    if body is not None:
        cmd += ["-d", json.dumps(body, ensure_ascii=False)]
    r = subprocess.run(cmd, capture_output=True, text=True, timeout=60)
    return json.loads(r.stdout)


def l5_model():
    spec = importlib.util.spec_from_file_location("l5m", L5)
    m = importlib.util.module_from_spec(spec)
    try:
        spec.loader.exec_module(m)
    except SystemExit:
        pass
    return m.load_config()


def omp_providers():
    """OMP 池顶层 provider 名（缩进 2）"""
    out = []
    for ln in open(OMP, encoding="utf-8"):
        if len(ln) - len(ln.lstrip(" ")) == 2 and ln.strip().endswith(":"):
            out.append(ln.strip()[:-1])
    return out


print("=" * 64)
print(" ① GET 读现状")
print("=" * 64)
g = curl("GET")
chk(g.get("ok") is True, "read ok")
chk(len(g.get("providers", [])) >= 1, "至少 1 个 provider")
role = g.get("model_role", {})
chk(role.get("consistent") is True, "两份角色绑定一致", "→ %r" % role.get("effective"))
print("  providers: %r" % [(p["name"], p["models"]) for p in g["providers"]])

print()
print("=" * 64)
print(" ② POST 改 agens → glm-5.3-flash")
print("=" * 64)
r = curl(
    "POST",
    {
        "provider": "agens",
        "base_url": "http://127.0.0.1:8788/v1",
        "model": "glm-5.3-flash",
    },
)
chk(r.get("ok") is True, "write ok")
for t in r.get("targets", []):
    chk(t.get("ok") is True, "target %s" % t.get("label"), "→ " + t.get("detail", ""))

print()
print("=" * 64)
print(" ③ 校验三处文件")
print("=" * 64)
op = omp_providers()
chk(op == ["agens"], "OMP 池只含 agens（无 workbuddy 泄漏）", "→ %r" % op)
omp_txt = open(OMP, encoding="utf-8").read()
chk("glm-5.3-flash" in omp_txt, "OMP 池含新模型")
chk("apiKeys:" in omp_txt, "OMP 池仍是 apiKeys 结构（l5.py 认）")
chk("keyPool:" not in omp_txt, "OMP 池**没有**被写成 keyPool")

data_txt = open(DATA, encoding="utf-8").read()
chk("glm-5.3-flash" in data_txt, "data 池含新模型")
chk("keyPool:" in data_txt, "data 池仍是 keyPool 结构")
chk("workbuddy" in data_txt, "data 池保留了 workbuddy")

chk("glm-5.3-flash" in open(OMP_CFG, encoding="utf-8").read(), "OMP config.yml 角色已更新")
chk("glm-5.3-flash" in open(DATA_CFG, encoding="utf-8").read(), "data config.yml 角色已更新")

# 缩进契约
bad = []
for txt, name in ((omp_txt, "omp"), (data_txt, "data")):
    for ln in txt.split("\n"):
        if not ln.strip() or ln.strip().startswith("#"):
            continue
        n = len(ln) - len(ln.lstrip(" "))
        t = ln.strip()
        if t == "models:" and n != 4:
            bad.append("%s: models 缩进 %d" % (name, n))
        if t.startswith("- id:") and n != 6:
            bad.append("%s: - id 缩进 %d" % (name, n))
chk(not bad, "两份文件缩进契约正确", "" if not bad else "→ " + "; ".join(bad))

print()
print("=" * 64)
print(" ④ l5.py 真解析器读到什么")
print("=" * 64)
cfg = l5_model()
chk(cfg["model"] == "glm-5.3-flash", "l5 读到新模型", "→ %r" % cfg["model"])
chk(cfg["base_url"].startswith("http"), "l5 读到 base_url", "→ %r" % cfg["base_url"])
chk(len(cfg["keys"]) >= 1, "l5 读到 key", "→ %r" % cfg["keys"])

print()
print("=" * 64)
print(" ⑤ POST 改回 → deepseek-v4.1-flash")
print("=" * 64)
r2 = curl(
    "POST",
    {
        "provider": "agens",
        "base_url": "http://127.0.0.1:8788/v1",
        "model": "deepseek-v4.1-flash",
    },
)
chk(r2.get("ok") is True, "还原 write ok")

print()
print("=" * 64)
print(" ⑥ 还原后校验")
print("=" * 64)
cfg2 = l5_model()
chk(cfg2["model"] == "deepseek-v4.1-flash", "l5 模型已还原", "→ %r" % cfg2["model"])
chk(omp_providers() == ["agens"], "OMP 池仍只含 agens")
g2 = curl("GET")
chk(g2["model_role"]["consistent"] is True, "两份角色绑定一致",
    "→ %r" % g2["model_role"]["effective"])

print()
print("=" * 64)
if fail:
    print(" ❌ 失败 %d 项：" % len(fail))
    for f in fail:
        print("    - " + f)
else:
    print(" 🎉 E2E 全部通过：读 → 改 → 三处同步 → l5 生效 → 改回 → 一致")
print("=" * 64)
sys.exit(1 if fail else 0)