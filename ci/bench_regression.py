#!/usr/bin/env python3
"""Bench-regression gate (Phase 2 S16).

Reads a baseline JSON file with structure:

    {
      "_threshold_pct": 25,
      "groups": {
        "<group>/<bench>": {"median_ns": 12345}
      }
    }

…and a criterion `--output-format bencher` log on stdin / argv[2].
Bencher format lines look like:

    test reconcile_decision/n=100 ... bench:           4500 ns/iter (+/- 100)

An optional third file supplies baseline measurements from the base revision
on the same runner. The JSON still defines the required groups and threshold.

Exit non-zero if any required measurement is missing or a bench exceeds
`baseline * (1 + threshold/100)`.

Used by `.github/workflows/ci.yml`'s `Bench Regression` job. Keep this
script tiny + dep-free (Python stdlib only) so it doesn't add CI install
time.
"""

import json
import re
import sys
from pathlib import Path

LINE_RE = re.compile(r"^test\s+(\S+)\s+\.\.\.\s+bench:\s+([\d,]+)\s+ns/iter")


def parse_bencher(text: str) -> dict[str, int]:
    out: dict[str, int] = {}
    for line in text.splitlines():
        m = LINE_RE.match(line.strip())
        if not m:
            continue
        name = m.group(1)
        ns = int(m.group(2).replace(",", ""))
        out[name] = ns
    return out


def main(argv: list[str]) -> int:
    if len(argv) not in (3, 4):
        print(
            "usage: bench_regression.py <baselines.json> <bencher.txt> [base-bencher.txt]",
            file=sys.stderr,
        )
        return 2
    baselines = json.loads(Path(argv[1]).read_text())
    threshold_pct = baselines.get("_threshold_pct", 25)
    groups = baselines.get("groups", {})
    measured = parse_bencher(Path(argv[2]).read_text())
    base_measured = parse_bencher(Path(argv[3]).read_text()) if len(argv) == 4 else None

    failures: list[str] = []
    if not groups:
        print("Bench regression FAIL: no required benchmark groups", file=sys.stderr)
        return 1
    for name, info in groups.items():
        if name not in measured:
            failures.append(f"{name}: missing head measurement")
            continue
        if base_measured is not None and name not in base_measured:
            failures.append(f"{name}: missing base measurement")
            continue
        baseline = base_measured[name] if base_measured is not None else info["median_ns"]
        ceiling = baseline * (1 + threshold_pct / 100)
        actual = measured[name]
        status = "OK" if actual <= ceiling else "REGRESSED"
        print(f"  {status:10s} {name:50s} {actual:8d}ns  (baseline {baseline}ns, ceiling {int(ceiling)}ns)")
        if actual > ceiling:
            failures.append(f"{name}: {actual}ns > ceiling {int(ceiling)}ns")

    if failures:
        print("\nBench regression FAIL:", file=sys.stderr)
        for f in failures:
            print(f"  - {f}", file=sys.stderr)
        return 1
    print(f"\nAll {len(groups)} bench groups within +{threshold_pct}% baseline.")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
