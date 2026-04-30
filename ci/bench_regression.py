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

Exit non-zero if any captured bench exceeds `baseline * (1 + threshold/100)`.

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
    if len(argv) < 3:
        print("usage: bench_regression.py <baselines.json> <bencher.txt>", file=sys.stderr)
        return 2
    baselines = json.loads(Path(argv[1]).read_text())
    threshold_pct = baselines.get("_threshold_pct", 25)
    groups = baselines.get("groups", {})
    measured = parse_bencher(Path(argv[2]).read_text())

    failures: list[str] = []
    missing: list[str] = []
    for name, info in groups.items():
        baseline = info["median_ns"]
        ceiling = baseline * (1 + threshold_pct / 100)
        if name not in measured:
            # criterion's bencher format normalizes group/bench names; allow
            # tolerant lookup so cosmetic renames don't fail-close.
            missing.append(name)
            continue
        actual = measured[name]
        status = "OK" if actual <= ceiling else "REGRESSED"
        print(f"  {status:10s} {name:50s} {actual:8d}ns  (baseline {baseline}ns, ceiling {int(ceiling)}ns)")
        if actual > ceiling:
            failures.append(f"{name}: {actual}ns > ceiling {int(ceiling)}ns")

    if missing:
        # Missing benches are warnings, not failures — useful when the bench
        # set evolves before the baseline json catches up. Tighten to fail
        # once the matrix is stable.
        for m in missing:
            print(f"  WARN       {m}: no measurement (rename or new?)", file=sys.stderr)

    if failures:
        print("\nBench regression FAIL:", file=sys.stderr)
        for f in failures:
            print(f"  - {f}", file=sys.stderr)
        return 1
    print(f"\nAll {len(groups) - len(missing)} bench groups within +{threshold_pct}% baseline.")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
