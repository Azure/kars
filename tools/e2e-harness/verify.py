#!/usr/bin/env python3
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""
verify.py — generic verifier for the e2e-harness.

Loads the scenario-specific check module from
`scenarios/$SCENARIO/checks.py` and runs the checks against the
artifacts produced by drive.sh + monitor.sh.

The generic harness is intentionally scenario-agnostic: it knows how to
load the trace, the transcript, and how to dispatch to the scenario's
`get_checks()` entry point. Anything scenario-specific (URL counts,
foundry endpoints, expected sibling pairs, channel signals) belongs in
the per-scenario `checks.py`.

Inputs (env or argv):
  SCENARIO — scenario name under scenarios/. Default: exec-brief.
  OUT_DIR  — directory containing trace.jsonl, transcript.log, apply.log.
             Default: tools/e2e-harness/out/latest.

Output:
  - human-readable check list to stdout
  - machine-readable JSON to OUT_DIR/verify.json
  - exit 0 if all checks pass, 1 otherwise
"""
from __future__ import annotations

import importlib.util
import json
import os
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable


# ─── Context passed to every check ────────────────────────────────────────────
@dataclass
class Context:
    """Bundle of artifacts and helpers a check may read.

    Constructed once per run by `main()` and passed verbatim to every
    check. Each scenario's `checks.py` typically reads `transcript`,
    `trace`, `router_lines`, and any per-sandbox log files in
    `out_dir`.
    """

    out_dir: Path
    scenario: str
    trace: list[dict[str, Any]]
    transcript: str
    router_lines: list[str]
    relay_lines: list[str]
    # Final delivered artifact (e.g. brief.md) collected by the platform's
    # `platform_collect_artifacts`. Populated when the scenario sets
    # SCENARIO_FINAL_ARTIFACT_PATH + SCENARIO_FINAL_ARTIFACT_SANDBOX in its
    # config.sh. Use this when the parent's transcript may have been
    # truncated by content-safety (Foundry can finish_reason=content_filter
    # mid-stream when echoing a long brief) — the artifact is the ground
    # truth of what the agent produced, the transcript is what the gateway
    # surfaced to the caller.
    final_artifact: str = ""
    extras: dict[str, Any] = field(default_factory=dict)

    def best_brief_text(self) -> str:
        """Return the most complete brief text available.

        Prefer the final artifact when present and longer than the
        transcript (signal that the transcript was truncated). Otherwise
        fall back to the transcript so older runs without
        `final-artifact.*` continue to work.
        """
        if self.final_artifact and len(self.final_artifact) > len(self.transcript):
            return self.final_artifact
        return self.transcript or self.final_artifact

    def lines_for(self, src: str) -> list[str]:
        """Pull every `msg` from the trace whose `src` matches `src`."""
        return [e.get("msg", "") for e in self.trace if e.get("src") == src]


# ─── Loading helpers ──────────────────────────────────────────────────────────
def load_trace(path: Path) -> list[dict[str, Any]]:
    if not path.exists():
        return []
    out: list[dict[str, Any]] = []
    for line in path.read_text(errors="replace").splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            out.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    return out


def load_scenario_checks(scenario_dir: Path) -> list[tuple[str, Callable[[Context], tuple[bool, str]]]]:
    """Dynamically import `<scenario_dir>/checks.py` and call its
    `get_checks()` to retrieve the list of check tuples."""
    checks_path = scenario_dir / "checks.py"
    if not checks_path.exists():
        raise SystemExit(f"ERR scenario checks module missing: {checks_path}")
    spec = importlib.util.spec_from_file_location(
        f"e2e_harness_scenario_checks_{scenario_dir.name}", checks_path
    )
    if spec is None or spec.loader is None:
        raise SystemExit(f"ERR could not load scenario module from {checks_path}")
    module = importlib.util.module_from_spec(spec)
    # Make `from verify import Context` available to the scenario module
    # without requiring it to manipulate sys.path. We register this verifier
    # module under the name "verify" so `TYPE_CHECKING` imports resolve.
    sys.modules.setdefault("verify", sys.modules[__name__])
    spec.loader.exec_module(module)
    if not hasattr(module, "get_checks"):
        raise SystemExit(
            f"ERR scenario module {checks_path} must define get_checks() "
            f"returning list[(label, callable(Context) -> (ok, detail))]"
        )
    return module.get_checks()


# ─── Main ─────────────────────────────────────────────────────────────────────
def main() -> int:
    scenario = os.environ.get("SCENARIO", "exec-brief")
    harness_root = Path(__file__).parent
    scenario_dir = harness_root / "scenarios" / scenario
    if not scenario_dir.is_dir():
        raise SystemExit(
            f"ERR scenario directory not found: {scenario_dir}\n"
            f"     Set SCENARIO=<name> to a directory under {harness_root}/scenarios/"
        )

    out_dir = Path(os.environ.get("OUT_DIR", harness_root / "out" / "latest"))
    trace = load_trace(out_dir / "trace.jsonl")
    transcript_path = out_dir / "transcript.log"
    transcript = transcript_path.read_text(errors="replace") \
        if transcript_path.exists() else ""

    # Optional ground-truth artifact. Platform-side collection writes it as
    # `final-artifact.<ext>` so the extension is preserved. We accept either
    # `.md` (markdown brief) or `.txt`; absent file ⇒ empty string and the
    # checks fall back to the transcript.
    final_artifact = ""
    for cand in ("final-artifact.md", "final-artifact.txt"):
        p = out_dir / cand
        if p.exists():
            final_artifact = p.read_text(errors="replace")
            break

    ctx = Context(
        out_dir=out_dir,
        scenario=scenario,
        trace=trace,
        transcript=transcript,
        router_lines=[e.get("msg", "") for e in trace if e.get("src") == "ROUTER"],
        relay_lines=[e.get("msg", "") for e in trace if e.get("src") == "RELAY"],
        final_artifact=final_artifact,
    )

    checks = load_scenario_checks(scenario_dir)

    results: list[dict[str, Any]] = []
    all_ok = True
    print(f"\nVerifying scenario '{scenario}' in {out_dir}\n" + "─" * 60)
    for label, fn in checks:
        try:
            ok, detail = fn(ctx)
        except Exception as e:  # never let a single check crash the run
            ok, detail = False, f"check raised {type(e).__name__}: {e}"
        results.append({"check": label, "passed": ok, "detail": detail})
        mark = "✅" if ok else "❌"
        print(f"{mark}  {label}\n      {detail}")
        all_ok &= ok

    out_dir.mkdir(parents=True, exist_ok=True)
    summary = {
        "scenario": scenario,
        "all_passed": all_ok,
        "checks": results,
    }
    (out_dir / "verify.json").write_text(json.dumps(summary, indent=2))
    print("─" * 60)
    print(f"OVERALL: {'PASS' if all_ok else 'FAIL'}  → {out_dir / 'verify.json'}")
    return 0 if all_ok else 1


if __name__ == "__main__":
    sys.exit(main())
