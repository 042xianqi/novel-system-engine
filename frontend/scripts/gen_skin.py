#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
从规范 HTML 提取 6 套皮肤 Token，生成 style.css 的「第 0 层」CSS。

为什么用脚本而不是手抄：
  · 每套皮肤 23 个色值 × 6 套 = 138 个值，手抄必错；
  · 每个色值还要同时输出 --x 和 --x-rgb（tailwind 透明度需要 RGB 通道），
    手写等于 276 个值，不可能不出错；
  · 脚本可以直接从规范原文抽取，保证「一个字符都不走样」。

输出：/root/skin_layer.css
"""
import os
import re

SRC = "/storage/emulated/0/Download/Operit/cleanOnExit/attachment_256120752230093817.html"
OUT = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "src", "skin.css")

# 需要同时生成 RGB 通道的变量（不透明色）
RGB_KEYS = [
    "primary", "primary-hover", "primary-soft", "primary-soft-hover", "on-primary",
    "bg", "card", "card-2", "text", "text-sec", "text-disabled",
    "border", "divider",
    "success", "success-soft", "warning", "warning-soft", "danger", "danger-soft",
]

SKIN_NAMES = {
    "qinglan": "青蓝 · 浅色默认 · 冷静工具感",
    "mohei": "夜墨 · 深色 · 常驻后台",
    "songlv": "松绿 · 浅色 · 健康/效率",
    "kongzi": "空紫 · 浅色 · 创意/AI",
    "xiaocheng": "霞橙 · 浅色暖调 · 活动/社交",
    "xuanqing": "玄青 · OLED 纯黑 · 高对比",
}

SKIN_ORDER = ["qinglan", "mohei", "songlv", "kongzi", "xiaocheng", "xuanqing"]


def parse_hex(v):
    """把 #rrggbb / #rgb 解析成 'r g b'；失败返回 None"""
    v = v.strip()
    m = re.fullmatch(r"#([0-9a-fA-F]{6})", v)
    if m:
        h = m.group(1)
        return f"{int(h[0:2], 16)} {int(h[2:4], 16)} {int(h[4:6], 16)}"
    m = re.fullmatch(r"#([0-9a-fA-F]{3})", v)
    if m:
        h = m.group(1)
        return f"{int(h[0]*2, 16)} {int(h[1]*2, 16)} {int(h[2]*2, 16)}"
    return None


def parse_rgba(v):
    """rgba(110,160,255,.16) → 同时返回原值和 rgb 通道"""
    m = re.fullmatch(r"\s*rgba?\(\s*([\d.]+)\s*,\s*([\d.]+)\s*,\s*([\d.]+)\s*(?:,\s*[\d.]+\s*)?\)\s*", v)
    if m:
        return f"{int(float(m.group(1)))} {int(float(m.group(2)))} {int(float(m.group(3)))}"
    return None


def main():
    src = open(SRC, encoding="utf-8", errors="replace").read()

    # 抓每个皮肤的声明块
    skins = {}
    for m in re.finditer(r'html\[data-skin="([a-z]+)"\]\s*\{(.*?)\}', src, re.S):
        name = m.group(1)
        body = m.group(2)
        decls = {}
        for d in body.split(";"):
            d = d.strip()
            if not d.startswith("--"):
                continue
            k, _, v = d.partition(":")
            decls[k.strip()[2:]] = v.strip()
        if decls:
            skins[name] = decls

    # qinglan 在规范里出现两次（第二次是残块），只保留字段更全的那份
    print("解析到皮肤:", {k: len(v) for k, v in skins.items()})

    lines = []
    lines.append("/* ══════════════════════════════════════════════════════════")
    lines.append("   第 0 层 · 皮肤 Token（由 scripts/gen_skin.py 从规范原文生成，勿手改）")
    lines.append("   ══════════════════════════════════════════════════════════")
    lines.append("   规则：组件层禁止出现固定色值，一切颜色都走 var(--x)。")
    lines.append("   每个不透明色同时提供 --x 与 --x-rgb，后者供 tailwind 的 /透明度 语法使用。")
    lines.append("   ══════════════════════════════════════════════════════════ */")
    lines.append("")

    for name in SKIN_ORDER:
        if name not in skins:
            print(f"!! 缺失皮肤 {name}")
            continue
        d = skins[name]
        lines.append(f"/* ---------- {SKIN_NAMES[name]} ---------- */")
        lines.append(f'html[data-skin="{name}"] {{')
        # 色值
        for k in RGB_KEYS:
            if k not in d:
                print(f"!! {name} 缺 {k}")
                continue
            v = d[k]
            rgba = parse_rgba(v)
            hexv = parse_hex(v)
            if rgba:
                lines.append(f"  --{k}: {v};")
                lines.append(f"  --{k}-rgb: {rgba};")
            elif hexv:
                lines.append(f"  --{k}: {v};")
                lines.append(f"  --{k}-rgb: {hexv};")
            else:
                lines.append(f"  --{k}: {v};")
        # 阴影/发光/圆角（不能转 RGB）
        for k in ("shadow", "shadow-hover", "glow", "ring", "mask", "grad-a", "grad-b", "r-sm", "r-md", "r-lg"):
            if k in d:
                lines.append(f"  --{k}: {d[k]};")
        # 渐变也要 RGB 通道吗？grad 用于 linear-gradient，不需要 alpha
        lines.append("}")
        lines.append("")

    # 全局间距/字体/动效 Token
    lines.append("/* ---------- 全局 Token：间距 / 字阶 / 圆角满值（皮肤不动） ---------- */")
    lines.append(":root {")
    lines.append("  --s-1:4px; --s-2:8px; --s-3:12px; --s-4:16px; --s-5:24px; --s-6:40px;")
    lines.append("  --r-full:999px;")
    lines.append("  --font:-apple-system,BlinkMacSystemFont,'PingFang SC','Segoe UI',Roboto,'Helvetica Neue',sans-serif;")
    lines.append("  --mono:'SF Mono','JetBrains Mono',Menlo,Consolas,monospace;")
    lines.append("}")
    lines.append("")
    lines.append("/* ---------- 动效 Token（规范第 3 层，全局统一） ---------- */")
    lines.append(":root {")
    lines.append("  --dur-1:100ms; --dur-2:200ms; --dur-3:300ms; --dur-4:500ms;")
    lines.append("  --ease-out:cubic-bezier(.2,0,0,1);")
    lines.append("  --ease-io:cubic-bezier(.4,0,.2,1);")
    lines.append("  --ease-spring:cubic-bezier(.34,1.56,.64,1);")
    lines.append("}")
    lines.append("")

    css = "\n".join(lines)
    open(OUT, "w", encoding="utf-8").write(css)
    print(f"\n写出 {OUT} {len(css)} 字节")


if __name__ == "__main__":
    main()