#!/usr/bin/env python3
"""Run .semgrep/bilterm.yml and fail only on findings above .semgrep/baseline.tsv.
Baseline lines: rule<TAB>file<TAB>count. A (rule,file) pair fails when its count exceeds the baseline
(missing = 0), so legacy hits are tolerated but new ones are not. Fewer hits only prints a hint.
  semgrep-gate.py                  check
  semgrep-gate.py --update-baseline  rewrite the baseline from the current tree (review the diff)"""
import collections, json, os, subprocess, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BASE = os.path.join(ROOT, ".semgrep", "baseline.tsv")
update = "--update-baseline" in sys.argv

p = subprocess.run(
    ["semgrep", "--metrics=off", "--disable-version-check", "--quiet", "--json",
     "--config", os.path.join(ROOT, ".semgrep", "bilterm.yml"), "src", "tests"],
    cwd=ROOT, capture_output=True, text=True)
try:
    data = json.loads(p.stdout)
except ValueError:
    print("semgrep produced no JSON:\n" + p.stderr[-2000:])
    sys.exit(1)
if data.get("errors"):
    print("semgrep errors:", json.dumps(data["errors"][:3])[:600])
    sys.exit(1)

cur = collections.Counter((r["check_id"].split(".")[-1], r["path"]) for r in data["results"])
if update:
    with open(BASE, "w") as f:
        for (rule, path), n in sorted(cur.items()):
            f.write(f"{rule}\t{path}\t{n}\n")
    print(f"baseline updated: {sum(cur.values())} hits in {len(cur)} (rule,file) pairs")
    sys.exit(0)

base = {}
if os.path.exists(BASE):
    for ln in open(BASE):
        parts = ln.rstrip("\n").split("\t")
        if len(parts) == 3:
            base[(parts[0], parts[1])] = int(parts[2])
bad = [(k, n, base.get(k, 0)) for k, n in sorted(cur.items()) if n > base.get(k, 0)]
fixed = sum(max(0, b - cur.get(k, 0)) for k, b in base.items())
for (rule, path), n, b in bad:
    lines = [str(r["start"]["line"]) for r in data["results"]
             if r["check_id"].endswith(rule) and r["path"] == path]
    print(f"NEW {rule} {path}: {n} > baseline {b} (lines {','.join(lines[:12])})")
if fixed:
    print(f"note: {fixed} baselined hit(s) gone; run scripts/semgrep-gate.py --update-baseline to ratchet")
print(f"semgrep gate: {sum(cur.values())} hits, {len(bad)} pair(s) over baseline")
sys.exit(1 if bad else 0)
