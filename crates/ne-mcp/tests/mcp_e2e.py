#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""ne-mcp 端到端 MCP 客户端测试。

协议：stdio + newline-delimited JSON-RPC 2.0（MCP 2025-06-18）。

用法：
    python3 _mcp_test_lab/mcp_client.py <ne-mcp二进制> <资料库根>

不做写操作：ne_db_query 只发 SELECT/PRAGMA，且库为只读副本。
"""
import json
import subprocess
import sys
import time

BIN = sys.argv[1] if len(sys.argv) > 1 else "target/debug/ne-mcp"
ROOT = sys.argv[2] if len(sys.argv) > 2 else "."

PASS, FAIL = [], []


def check(name, cond, detail=""):
    (PASS if cond else FAIL).append(name)
    print(f"  {'✅' if cond else '❌'} {name}" + (f" — {detail}" if detail else ""))
    return cond


class McpClient:
    def __init__(self, bin_path, root):
        self.p = subprocess.Popen(
            [bin_path, "--root", root],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, bufsize=1,
        )
        self._id = 0

    def send(self, method, params=None, notify=False):
        """发一行 JSON-RPC；notify=True 时不带 id（通知类无响应）。"""
        self._id += 1
        msg = {"jsonrpc": "2.0", "method": method}
        if not notify:
            msg["id"] = self._id
        if params is not None:
            msg["params"] = params
        self.p.stdin.write(json.dumps(msg, ensure_ascii=False) + "\n")
        self.p.stdin.flush()
        if notify:
            return None
        line = self.p.stdout.readline()
        if not line:
            raise RuntimeError(f"服务无响应（method={method}）")
        return json.loads(line)

    def call_tool_raw(self, name, args=None):
        """返回完整 result（含 isError 标记）。"""
        r = self.send("tools/call", {"name": name, "arguments": args or {}})
        return r.get("result", {})

    def call_tool(self, name, args=None):
        """调 tools/call，返回解析后的 content text（若为 JSON 则已 parse）。"""
        r = self.send("tools/call", {"name": name, "arguments": args or {}})
        res = r.get("result", {})
        content = res.get("content", [])
        if not content:
            return {"__raw__": res}
        txt = content[0].get("text", "")
        try:
            return json.loads(txt)
        except Exception:
            return {"__text__": txt}

    def close(self):
        try:
            self.p.stdin.close()
        except Exception:
            pass
        try:
            self.p.wait(timeout=5)
        except Exception:
            self.p.kill()


def main():
    print(f"═══ ne-mcp 端到端测试 ═══")
    print(f"  二进制: {BIN}")
    print(f"  资料库: {ROOT}")
    print()

    c = McpClient(BIN, ROOT)
    try:
        # ── 1. 协议握手 ──
        print("【1】协议握手")
        r = c.send("initialize", {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "test-client", "version": "1.0"},
        })
        si = r.get("result", {}).get("serverInfo", {})
        check("initialize 返回 result", "result" in r, r.get("error", ""))
        check("protocolVersion 正确",
              r.get("result", {}).get("protocolVersion") == "2025-06-18",
              r.get("result", {}).get("protocolVersion"))
        check("serverInfo.name = taixu-novel-engine",
              si.get("name") == "taixu-novel-engine", si.get("name"))
        check("capabilities.tools 存在", "tools" in r.get("result", {}).get("capabilities", {}))

        # 通知类（无 id，不应有响应）
        c.send("notifications/initialized", notify=True)

        # ping
        r = c.send("ping")
        check("ping 有响应", "result" in r)

        # ── 2. tools/list ──
        print("\n【2】tools/list")
        r = c.send("tools/list")
        tools = r.get("result", {}).get("tools", [])
        names = [t["name"] for t in tools]
        check("工具数为 6", len(tools) == 6, f"实际 {len(tools)}")
        expect = {"funnel", "ne_cardgen", "ne_inject", "ne_check", "ne_db", "ne_db_query"}
        check("6 个工具名齐全", set(names) == expect,
              f"缺 {expect - set(names)}，多 {set(names) - expect}")
        for t in tools:
            check(f"  {t['name']} 有 inputSchema", "inputSchema" in t and
                  t["inputSchema"].get("type") == "object")

        # ── 3. 未知方法 / 未知工具 / 参数校验（MCP 规范 -32602）──
        print("\n【3】错误处理")
        r = c.send("no/such/method")
        check("未知方法返回 -32601", r.get("error", {}).get("code") == -32601,
              str(r.get("error")))

        r = c.send("tools/call", {"name": "no_such_tool", "arguments": {}})
        check("未知工具返回 -32602", r.get("error", {}).get("code") == -32602,
              str(r.get("error")))

        # 参数校验：schema required 必须被强制
        for tool, args, key in [
            ("ne_inject", {"volume": "01"}, "chapter"),
            ("ne_inject", {"chapter": 1}, "volume"),
            ("ne_cardgen", {}, "volume"),
            ("ne_check", {}, "text"),
            ("ne_db_query", {}, "sql"),
        ]:
            r = c.send("tools/call", {"name": tool, "arguments": args})
            err = r.get("error", {})
            check(f"{tool} 缺 {key} → -32602",
                  err.get("code") == -32602 and key in err.get("message", ""),
                  f"code={err.get('code')} msg={err.get('message')}")

        r = c.send("tools/call", {"name": "ne_inject",
                                  "arguments": {"volume": "01", "chapter": "不是数字"}})
        err = r.get("error", {})
        check("ne_inject chapter 类型错误 → -32602",
              err.get("code") == -32602, f"code={err.get('code')} msg={err.get('message')}")

        r = c.send("tools/call", {"name": "ne_cardgen", "arguments": {"volume": "  "}})
        err = r.get("error", {})
        check("ne_cardgen volume 空串 → -32602",
              err.get("code") == -32602, f"code={err.get('code')}")

        # ── 4. ne_check（纯离线，不依赖资料库）──
        print("\n【4】ne_check（离线质检）")
        # 4a. 干净文本不应 reject
        good = "。".join(["他走进屋子看着桌上的灯那灯很旧他没有说话窗外风很大"] * 3) + "？"
        good = ""
        para = "他走进屋子，看着桌上的灯。那灯很旧。他没有说话。窗外风很大。"
        while len(good.replace("\n", "").replace(" ", "")) < 2600:
            good += para + "\n\n"
        good += "他忽然想到一个问题：那只手究竟是谁的？"
        out = c.call_tool("ne_check", {"text": good, "chapter": 3})
        v = out.get("verdict", out.get("verdict", "?"))
        check("ne_check 干净文本非 reject", str(v).lower() != "reject",
              f"verdict={v}")
        check("ne_check 返回 findings", "findings" in out or "__text__" in out)

        # 4b. 禁词必须 reject
        bad = good + "归墟"
        out = c.call_tool("ne_check", {"text": bad, "chapter": 3})
        check("ne_check 禁词判 reject", str(out.get("verdict", "")).lower() == "reject",
              f"verdict={out.get('verdict')}")

        # 4c. 空 text → 协议级 -32602（已在 §3 覆盖，这里略）

        # ── 5. funnel ──
        print("\n【5】funnel")
        out = c.call_tool("funnel", {})
        txt = json.dumps(out, ensure_ascii=False)
        check("funnel 有返回", bool(out))
        check("funnel 提到 stage/层", any(k in txt for k in ("stage", "L0", "L5", "待推进")),
              txt[:100])

        # ── 6. ne_db ──
        print("\n【6】ne_db（直连副本库）")
        out = c.call_tool("ne_db", {})
        txt = json.dumps(out, ensure_ascii=False)
        check("ne_db 有返回", bool(out))
        check("ne_db 报告表/行数", any(k in txt for k in ("tables", "rows", "count", "行")),
              txt[:120])

        # ── 7. ne_db_query（只读）──
        print("\n【7】ne_db_query（只读）")
        out = c.call_tool("ne_db_query", {"sql": "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name LIMIT 5", "limit": 5})
        txt = json.dumps(out, ensure_ascii=False)
        check("ne_db_query SELECT 成功", bool(out) and "error" not in txt.lower()[:200],
              txt[:150])

        out = c.call_tool("ne_db_query", {"sql": "PRAGMA table_info(materials)", "limit": 10})
        txt = json.dumps(out, ensure_ascii=False)
        check("ne_db_query PRAGMA 成功", bool(out), txt[:120])

        # 写操作必须被拒，且走 result + isError（不是协议错误）
        for sql in ["DELETE FROM materials", "DROP TABLE materials",
                    "UPDATE materials SET name='x'", "INSERT INTO materials VALUES(1)"]:
            res = c.call_tool_raw("ne_db_query", {"sql": sql, "limit": 1})
            txt = json.dumps(res, ensure_ascii=False)
            check(f"ne_db_query 拒绝写操作({sql.split()[0]})",
                  any(k in txt for k in ("拒绝", "只读", "SELECT", "不允许")),
                  txt[:110])
            check(f"   ↑ 带 isError=true（{sql.split()[0]}）",
                  res.get("isError") is True, f"isError={res.get('isError')}")

        # 正常 SELECT 不应带 isError
        res = c.call_tool_raw("ne_db_query", {"sql": "SELECT 1 AS a", "limit": 1})
        check("正常 SELECT 无 isError", res.get("isError") is not True,
              f"isError={res.get('isError')}")

        # 存在的工具但执行失败（不存在的表）→ isError
        res = c.call_tool_raw("ne_db_query", {"sql": "SELECT * FROM no_such_table_xyz", "limit": 1})
        check("查询不存在的表 → isError=true", res.get("isError") is True,
              json.dumps(res, ensure_ascii=False)[:110])

        # ── 8. ne_cardgen / ne_inject ──
        print("\n【8】ne_cardgen / ne_inject")
        out = c.call_tool("ne_cardgen", {"volume": "01"})
        txt = json.dumps(out, ensure_ascii=False)
        check("ne_cardgen vol=01 有返回", bool(out), txt[:120])

        out = c.call_tool("ne_inject", {"volume": "01", "chapter": 1, "allow_missing": True})
        txt = json.dumps(out, ensure_ascii=False)
        check("ne_inject vol=01 ch=1 有返回", bool(out), txt[:120])

    finally:
        c.close()

    print()
    print("═" * 50)
    print(f"  通过 {len(PASS)} / 失败 {len(FAIL)}")
    if FAIL:
        print("  失败项：")
        for f in FAIL:
            print(f"    - {f}")
    print("═" * 50)
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
