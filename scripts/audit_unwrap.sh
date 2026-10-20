#!/usr/bin/env bash
# audit_unwrap.sh —— 生产路径 unwrap/expect/panic 静态审计 + 风险分级（决策二）
#
# # 为什么需要这个脚本
# 裸 grep 统计会严重失真：实测 `world_gov.rs` 有 70 处 `.unwrap()`、`note_tag_store.rs`
# 有 62 处，但**它们全部位于 `#[cfg(test)]` 测试模块内**（测试里 fail-fast 是正确写法）。
# 若不区分就下结论，会得出"生产路径到处是 panic"的错误判断——本项目早期就是这么
# 误判的（白皮书初稿把这两个文件列为"生产热点"，实为测试代码）。
#
# # 判定规则（保守、可验证、零误报生产）
#   1. 路径含 `/tests/` 或 `examples/` 的 → 非生产（集成测试 / 示例程序）；
#   2. 文件名以 `_tests.rs` 结尾或名为 `tests.rs` → 测试模块；
#   3. 其余文件：**首个 `#[cfg(test)]` 之前的行 = 生产路径**。
#
# # 风险分级（对生产命中逐条定级，而非一刀切）
#   A 常量正则构造   `Regex::new(<字面量>).unwrap()/expect()` —— 只在字面量写错时炸，
#                    确定性、可静态验证；建议改 `LazyLock` 一次性建（顺带提速）。
#   B 正则契约保证   `m.get(0).unwrap()` / `caps.get(1).unwrap()` —— 匹配成功即保证存在；
#                    低危，但建议加注释说明依据。
#   C 逻辑守卫型     `opt.unwrap()` 由前文 `if let Some` / `is_some()` 守卫 —— 当下安全，
#                    但重构易失效；建议改 `let Some(x) = ... else`。
#   D 外部输入型     JSON/DB/CLI/网络输入直接 unwrap —— **真隐患**，必须改 Result/Option。
#   E 其它           需人工判断。
#
# 用法：
#   scripts/audit_unwrap.sh                 # 全仓汇总 + 分级统计
#   scripts/audit_unwrap.sh <file.rs>...    # 指定文件，输出逐行 + 级别
#   scripts/audit_unwrap.sh --class D       # 只列 D 级（真隐患）
# 退出码：0 = 无 D 级命中；1 = 存在 D 级命中（可用于 CI 拦截）

set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

python3 - "$@" <<'PY'
import re, sys, pathlib

args = sys.argv[1:]
only_class = None
explicit = []
for a in args:
    if a in ('--class',) or a.startswith('--class='):
        only_class = a.split('=')[1].upper() if '=' in a else None
    elif a.endswith('.rs'):
        explicit.append(a)
if '--class' in args:
    i = args.index('--class')
    if i + 1 < len(args):
        only_class = args[i+1].upper()

PAT = re.compile(r'\.unwrap\(\)|\.expect\(|panic!\(|unimplemented!\(|todo!\(')
TEST_MARK = re.compile(r'#\[cfg\(test\)\]')
REGEX_CTOR = re.compile(r'Regex::new\(')
GET0 = re.compile(r'\.get\(0\)\s*\.(unwrap|expect)\(')
# 外部输入来源的关键词（D 级判定）
EXTERNAL = re.compile(r'\b(from_str|serde_json|from_value|query_row|read_to_string|recv|read_line|file_stem|env::var)\b')
# 正则上下文：命中附近出现 captures/Regex 时，get(N).unwrap() 属捕获组取值（B 级）
REGEX_CTX = re.compile(r'\.captures\(|Regex::new|captures_iter')

def strip_comment(line: str) -> str:
    """剥掉行注释，避免把注释里提到的 unwrap 当成真实调用（假警报）。"""
    out, in_str, i, n = [], False, 0, len(line)
    while i < n:
        c = line[i]
        if in_str:
            if c == '\\':
                out.append(line[i:i+2]); i += 2; continue
            if c == '"':
                in_str = False
            out.append(c); i += 1; continue
        if c == '"':
            in_str = True; out.append(c); i += 1; continue
        if c == '/' and i + 1 < n and line[i+1] == '/':
            break
        out.append(c); i += 1
    return ''.join(out)

def is_test_file(p: pathlib.Path) -> bool:
    parts = set(p.parts)
    if 'tests' in parts or 'examples' in parts:
        return True
    if p.name == 'tests.rs' or p.name.endswith('_tests.rs'):
        return True
    return False

def classify(lines, idx, line) -> str:
    """idx 为 0-based 行号"""
    # 往回看 4 行，捕捉多行链式调用
    ctx = '\n'.join(lines[max(0, idx-10):idx+1])
    if REGEX_CTOR.search(ctx):
        return 'A'
    if GET0.search(line):
        return 'B'
    # 捕获组取值：`c.get(1).unwrap().as_str()` 且上文是正则 → B 级（匹配成功即保证存在）
    if re.search(r'\.get\(\d+\)\s*\.(unwrap|expect)\(', ctx) and REGEX_CTX.search(ctx):
        return 'B'
    if EXTERNAL.search(ctx):
        return 'D'
    # 前 6 行内有守卫关键字 → C
    guard = '\n'.join(lines[max(0, idx-6):idx])
    if re.search(r'if let Some|is_some\(\)|is_ok\(\)|if .*\.len\(\)|assert|match ', guard):
        return 'C'
    return 'E'

def audit(path: pathlib.Path):
    lines = path.read_text(errors='replace').split('\n')
    if is_test_file(path):
        return [], sum(len(PAT.findall(l)) for l in lines), True
    cut = len(lines)
    for i, l in enumerate(lines):
        if TEST_MARK.search(l):
            cut = i
            break
    code = [strip_comment(l) for l in lines]
    prod = []
    for i, l in enumerate(code[:cut]):
        if PAT.search(l):
            prod.append((i+1, lines[i].strip(), classify(code, i, l)))
    tz = sum(len(PAT.findall(l)) for l in code[cut:])
    return prod, tz, False

files = [pathlib.Path(p) for p in explicit]
if not files:
    files = sorted(p for p in pathlib.Path('crates').rglob('*.rs') if 'target' not in p.parts)

tot_prod = tot_test = 0
by_class = {}
hot = []
for f in files:
    try:
        prod, tn, _ = audit(f)
    except Exception as e:
        print(f'  跳过 {f}: {e}')
        continue
    tot_test += tn
    if prod:
        tot_prod += len(prod)
        for _, _, c in prod:
            by_class.setdefault(c, []).append((f, _))
        hot.append((len(prod), f, prod))
hot.sort(key=lambda x: (-x[0], str(x[1])))

print('═' * 76)
print(f'生产路径 unwrap/expect/panic 审计   （扫描 {len(files)} 个 .rs 文件）')
print('═' * 76)

if explicit:
    for n, f, prod in hot:
        print(f'\n■ {f}  生产命中 {n} 处')
        for ln, txt, c in prod:
            if only_class and c != only_class:
                continue
            print(f'  [{c}] {ln:>5}: {txt[:104]}')
    if not hot:
        print('\n✅ 指定文件生产路径零命中')
else:
    print(f'\n【汇总】生产命中 {tot_prod} 处 | 测试/示例命中 {tot_test} 处')
    print('\n【按风险分级】')
    labels = {'A': '常量正则构造（低危·建议 LazyLock）',
              'B': '正则契约保证（低危）',
              'C': '逻辑守卫型（中危·重构易失效）',
              'D': '★外部输入型（真隐患·必须改 Result）',
              'E': '待人工判断'}
    for c in 'ABCDE':
        n = len(by_class.get(c, []))
        print(f'  [{c}] {n:>4} 处   {labels[c]}')
    if by_class.get('D'):
        print('\n【D 级真隐患清单】')
        for f, ln in by_class['D']:
            print(f'    {f}:{ln}')
    if hot:
        print('\n【生产命中热点 TOP12】')
        for n, f, _ in hot[:12]:
            print(f'  {n:>4} 处  {f}')

sys.exit(1 if by_class.get('D') else 0)
PY
