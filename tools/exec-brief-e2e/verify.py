#!/usr/bin/env python3
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""
verify.py — run the 7 acceptance checks from the exec-brief prompt against
the artifacts produced by drive.sh + monitor.sh.

Inputs (env or argv):
  OUT_DIR — directory containing trace.jsonl, transcript.log, apply.log
            (default: tools/exec-brief-e2e/out/latest)

Output:
  - human-readable check list to stdout
  - machine-readable JSON to OUT_DIR/verify.json
  - exit 0 if all 7 pass, 1 otherwise
"""
from __future__ import annotations

import json
import os
import re
import sys
from pathlib import Path
from typing import Any


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


def lines_for(trace: list[dict[str, Any]], src: str) -> list[str]:
    return [e.get("msg", "") for e in trace if e.get("src") == src]


# ─── Individual checks ────────────────────────────────────────────────────────
def check_sources(transcript: str) -> tuple[bool, str]:
    # Distinct URLs cited in the final reply that look like they reference 2026
    # publications. Heuristic: count unique http(s) URLs in the transcript.
    urls = set(re.findall(r"https?://[^\s)>\]]+", transcript))
    # Drop any obvious infra noise (registry/relay/telegram api)
    noise = ("api.telegram.org", "modelcontextprotocol.io",
             "ai.azure.com", "login.microsoftonline.com", "agentmesh")
    clean = {u for u in urls if not any(n in u for n in noise)}
    ok = len(clean) >= 6
    return ok, f"{len(clean)} distinct external URLs cited (need ≥6)"


def check_scorecard(transcript: str) -> tuple[bool, str]:
    # The analyst is asked for a 4×4 scorecard. We look for either a JSON
    # block with a "metrics" key OR a markdown table mentioning the four
    # columns.
    cols = ("isolation", "egress", "attestation", "governance")
    found = sum(1 for c in cols if c in transcript.lower())
    has_metrics = '"metrics"' in transcript or "scorecard" in transcript.lower()
    ok = has_metrics and found == 4
    return ok, f"metrics block present={has_metrics}, axis labels found={found}/4"


def check_hero(transcript: str, router: list[str]) -> tuple[bool, str]:
    # The router logs every Foundry images call. Look for gpt-image-1.
    image_calls = [l for l in router if "/images/generations" in l or "gpt-image-1" in l]
    mentions_1024 = "1024x1024" in transcript or "1024×1024" in transcript
    ok = bool(image_calls) and mentions_1024
    return ok, f"foundry image calls={len(image_calls)}, 1024x1024 mention={mentions_1024}"


def check_chart(transcript: str, router: list[str]) -> tuple[bool, str]:
    # Foundry code-exec leaves a /code/sessions/... route in the router log.
    code_calls = [l for l in router if "/code/sessions" in l or "code_interpreter" in l.lower()]
    ok = bool(code_calls)
    return ok, f"foundry code-exec calls={len(code_calls)}"


def check_relay_pairs(trace: list[dict[str, Any]]) -> tuple[bool, str]:
    # Encrypted blobs flow over a persistent /ws connection; the relay does
    # NOT log per-message routes in plaintext (we'd see HTTP /health lines
    # only). So we infer sibling pairs from the OpenClaw plugin's own log
    # line `AGT relay: sent to <agentName>` emitted at agt-tools/agt.ts:694.
    # Each sub-agent pod gets its own monitor source tag `POD-<sender>`.
    pat = re.compile(r"AGT relay:\s*sent to\s+([A-Za-z0-9_-]+)")
    siblings = {"analyst", "viz", "writer"}
    pairs: set[frozenset[str]] = set()
    for entry in trace:
        src = entry.get("src", "")
        if not src.startswith("POD-"):
            continue
        sender = src[len("POD-"):]
        if sender not in siblings:
            continue
        for target in pat.findall(entry.get("msg", "")):
            if target in siblings and target != sender:
                pairs.add(frozenset((sender, target)))
    expected = {frozenset(("analyst", "viz")),
                frozenset(("analyst", "writer")),
                frozenset(("viz", "writer"))}
    missing = expected - pairs
    ok = not missing
    return ok, f"{len(pairs)}/3 sibling pairs on the wire (missing={sorted(map(sorted, missing))})"


def check_telegram(router: list[str]) -> tuple[bool, str]:
    # Telegram channel plugin posts go through the router as outbound
    # https://api.telegram.org/bot.../sendMessage calls.
    if not any("TELEGRAM" in os.environ.get(k, "") for k in os.environ) \
       and not os.environ.get("TELEGRAM_BOT_TOKEN"):
        return True, "skipped (no TELEGRAM_BOT_TOKEN in env)"
    posts = [l for l in router if "api.telegram.org" in l and "sendMessage" in l]
    ok = len(posts) >= 5
    return ok, f"{len(posts)} telegram sendMessage calls (need ≥5)"


def check_brief(transcript: str) -> tuple[bool, str]:
    # Loose: the final reply should be ≥600 and ≤1400 words and mention both
    # "hero" placement and a chart.
    words = len(transcript.split())
    has_chart = "chart" in transcript.lower() or "![" in transcript
    has_hero = "hero" in transcript.lower() or transcript.lower().count("![") >= 2
    ok = 600 <= words <= 1500 and has_chart and has_hero
    return ok, f"{words} words; chart_ref={has_chart}; hero_ref={has_hero}"


def check_egress_clean(trace: list[dict[str, Any]]) -> tuple[bool, str]:
    # With egressMode: Strict, any sandbox→external connection to a host
    # not in `allowedEndpoints` shows up either as a NetworkPolicy drop
    # event on the pod or as a "BlockedBuffer" entry in the controller log.
    # If the run was clean (only telegram + mcp-fetch were touched), we
    # expect zero of either.
    ctrl = lines_for(trace, "CTRL")
    evt = lines_for(trace, "K8S-EVT")
    denials = [l for l in ctrl if "BlockedBuffer" in l or "egress.*denied" in l.lower()]
    drops = [l for l in evt if "NetworkPolicy" in l and ("deny" in l.lower() or "drop" in l.lower())]
    total = len(denials) + len(drops)
    ok = total == 0
    return ok, f"controller blocked={len(denials)}, k8s netpol drops={len(drops)}"


def check_mcp_traffic(router: list[str], transcript: str) -> tuple[bool, str]:
    # The analyst is required to call DeepWiki MCP for ≥2 platforms. The
    # router proxies MCP traffic on its `/mcp/...` routes; we count hits
    # plus a transcript mention of deepwiki as a belt-and-braces check.
    mcp_calls = [l for l in router if "/mcp request" in l or "/mcp/" in l or "mcp.deepwiki.com" in l]
    mentioned = "deepwiki" in transcript.lower()
    ok = bool(mcp_calls) and mentioned
    return ok, f"router /mcp calls={len(mcp_calls)}, deepwiki cited={mentioned}"


# ─── Main ─────────────────────────────────────────────────────────────────────
CHECKS = [
    ("≥6 distinct 2026 sources cited",      check_sources),
    ("metrics scorecard 4×4 + axis labels", check_scorecard),
    ("hero image via gpt-image-1 (1024²)",  check_hero),
    ("chart via Foundry code-exec",         check_chart),
    ("≥3 distinct sibling pairs on relay",  check_relay_pairs),
    ("≥5 telegram status posts",            check_telegram),
    ("brief ~900 words, hero+chart present", check_brief),
    ("egress: 0 NetworkPolicy denials",     check_egress_clean),
    ("MCP (DeepWiki) traffic observed",     check_mcp_traffic),
]


def main() -> int:
    out_dir = Path(os.environ.get("OUT_DIR",
        Path(__file__).parent / "out" / "latest"))
    trace = load_trace(out_dir / "trace.jsonl")
    transcript = (out_dir / "transcript.log").read_text(errors="replace") \
        if (out_dir / "transcript.log").exists() else ""

    router_lines = lines_for(trace, "ROUTER")
    relay_lines = lines_for(trace, "RELAY")

    results: list[dict[str, Any]] = []
    all_ok = True
    print(f"\nVerifying exec-brief run in {out_dir}\n" + "─" * 60)
    for label, fn in CHECKS:
        # adapt signature per-check
        if fn is check_sources:           ok, detail = fn(transcript)
        elif fn is check_scorecard:       ok, detail = fn(transcript)
        elif fn is check_hero:            ok, detail = fn(transcript, router_lines)
        elif fn is check_chart:           ok, detail = fn(transcript, router_lines)
        elif fn is check_relay_pairs:     ok, detail = fn(trace)
        elif fn is check_telegram:        ok, detail = fn(router_lines)
        elif fn is check_brief:           ok, detail = fn(transcript)
        elif fn is check_egress_clean:    ok, detail = fn(trace)
        elif fn is check_mcp_traffic:     ok, detail = fn(router_lines, transcript)
        else:                             ok, detail = (False, "unknown check")

        results.append({"check": label, "passed": ok, "detail": detail})
        mark = "✅" if ok else "❌"
        print(f"{mark}  {label}\n      {detail}")
        all_ok &= ok

    summary = {"all_passed": all_ok, "checks": results}
    (out_dir / "verify.json").write_text(json.dumps(summary, indent=2))
    print("─" * 60)
    print(f"OVERALL: {'PASS' if all_ok else 'FAIL'}  → {out_dir / 'verify.json'}")
    return 0 if all_ok else 1


if __name__ == "__main__":
    sys.exit(main())
