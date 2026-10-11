#!/usr/bin/env python3
"""架构门禁 —— crate 依赖白名单 + import 黑名单 + 文件行数棘轮。

学太墟 architecture-policy.json 的思路：**policy 即代码**，
新增依赖/超长文件必须显式登记，否则门禁失败。

用法：
    python3 scripts/arch_check.py            # 检查（CI 用）
    python3 scripts/arch_check.py --sync-baseline  # 把当前超限文件写进基线（维护用）
"""
import json
import os
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
POLICY = ROOT / "architecture-policy.json"


def load_policy():
    with open(POLICY, encoding="utf-8") as f:
        return json.load(f)


def parse_crate_path_deps(crate_dir: Path):
    """解析 Cargo.toml 里的 path 依赖 → 本项目的 crate 名集合。"""
    cargo = crate_dir / "Cargo.toml"
    if not cargo.is_file():
        return set()
    text = cargo.read_text(encoding="utf-8")
    deps = set()
    # [dependencies] / [dev-dependencies] 段里的 path = "../ne-xxx"
    for m in re.finditer(r'path\s*=\s*"\.\./([A-Za-z0-9_\-]+)"', text):
        deps.add(m.group(1))
    return deps


def crate_dirs():
    cdir = ROOT / "crates"
    return {p.name: p for p in cdir.iterdir() if p.is_dir() and (p / "Cargo.toml").is_file()}


def check_deps(policy, crates):
    errors = []
    known = {c["name"]: c for c in policy["crates"]}
    for name, path in sorted(crates.items()):
        spec = known.get(name)
        if spec is None:
            errors.append(
                f"[未登记 crate] `{name}` 不在 architecture-policy.json 的 crates 里；"
                f"请在 policy 中登记其 mayDependOn"
            )
            continue
        allowed = set(spec.get("mayDependOn", []))
        actual = parse_crate_path_deps(path)
        for dep in sorted(actual):
            if dep not in allowed:
                errors.append(
                    f"[非法依赖] crate `{name}` 依赖了 `{dep}`，"
                    f"但 policy 未授权（mayDependOn={sorted(allowed)}）"
                )
    return errors


def check_import_bans(policy, crates):
    errors = []
    for ban in policy.get("importBans", []):
        pat = re.compile(ban["pattern"])
        targets = ban.get("crates", [])
        for name in targets:
            if name not in crates:
                continue
            for rs in (crates[name] / "src").rglob("*.rs"):
                for i, line in enumerate(rs.read_text(encoding="utf-8", errors="replace").splitlines(), 1):
                    if pat.search(line):
                        rel = rs.relative_to(ROOT)
                        errors.append(f"[{ban['id']}] {rel}:{i} — {ban['message']}")
    return errors


def check_file_lines(policy, crates):
    """文件行数棘轮：超限文件必须在基线里，且不得超过基线的当前值。"""
    limit = policy["global"]["maxFileLines"]
    baseline_path = ROOT / policy["global"]["baselineFile"]
    baseline = {}
    if baseline_path.is_file():
        baseline = json.loads(baseline_path.read_text(encoding="utf-8")).get("files", {})

    errors, current = [], {}
    for name, path in sorted(crates.items()):
        for rs in (path / "src").rglob("*.rs"):
            n = len(rs.read_text(encoding="utf-8", errors="replace").splitlines())
            rel = str(rs.relative_to(ROOT))
            if n <= limit:
                continue
            current[rel] = n
            if rel not in baseline:
                errors.append(
                    f"[超长文件] {rel} 有 {n} 行 > 上限 {limit}；"
                    f"请拆分，或运行 --sync-baseline 登记（棘轮只许下调）"
                )
            elif n > baseline[rel]:
                errors.append(
                    f"[棘轮上涨] {rel} 从基线 {baseline[rel]} 涨到 {n} 行；只许下调不许上涨"
                )
    return errors, current


def sync_baseline(policy, crates):
    _, current = check_file_lines(policy, crates)
    baseline_path = ROOT / policy["global"]["baselineFile"]
    baseline_path.write_text(
        json.dumps(
            {"_doc": "棘轮基线：这些文件行数超上限。只许下调不许上涨。",
             "files": dict(sorted(current.items()))},
            ensure_ascii=False, indent=2,
        ) + "\n",
        encoding="utf-8",
    )
    print(f"已同步基线：{len(current)} 个超限文件 → {baseline_path.relative_to(ROOT)}")


def main():
    policy = load_policy()
    crates = crate_dirs()
    if "--sync-baseline" in sys.argv:
        sync_baseline(policy, crates)
        return 0

    errors = []
    errors += check_deps(policy, crates)
    errors += check_import_bans(policy, crates)
    line_errors, _ = check_file_lines(policy, crates)
    errors += line_errors

    if errors:
        print("架构门禁失败：", file=sys.stderr)
        for e in errors:
            print("  " + e, file=sys.stderr)
        return 1
    print(f"架构门禁通过（{len(crates)} 个 crate，上限 {policy['global']['maxFileLines']} 行/文件）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
