#!/usr/bin/env python3
"""Parse rust-code-analysis JSON output into a complexity summary table.

Usage: rust-code-analysis-cli --metrics -O json -p src/ | python3 scripts/complexity-report.py
"""
import json
import sys

funcs = []


def walk(node, file=""):
    kind = node.get("kind", "")
    if kind == "function":
        name = node.get("name", "<anon>")
        cc = node.get("metrics", {}).get("cognitive", {}).get("sum", 0)
        sloc = node.get("metrics", {}).get("loc", {}).get("sloc", 0)
        if cc and cc > 0:
            funcs.append((cc, sloc, file, name))
    for space in node.get("spaces", []):
        walk(space, file)


for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        entry = json.loads(line)
        walk(entry, entry.get("name", ""))
    except json.JSONDecodeError:
        continue

funcs.sort(reverse=True)
print(f"{'Complexity':>10}  {'SLOC':>6}  Function")
print(f"{'─' * 10:>10}  {'─' * 6:>6}  {'─' * 50}")
for cc, sloc, f, name in funcs[:20]:
    short_f = f.replace("src/", "")
    print(f"{cc:>10.0f}  {sloc:>6.0f}  {short_f}::{name}")
