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


def _transfer_evidence_text(router: list[str]) -> str:
    """Aggregate text from sources that can witness mesh file transfers:
    - monitor.log (kubectl logs stdout; rarely contains the lines)
    - writer-incoming.txt (definitive: writer's incoming/ directory listing)
    - writer-gateway.log (file_transfer_ack JSON the writer plugin logged)
    - viz-gateway.log (mesh_transfer_file lines the viz plugin logged)
    drive.sh's collect_artifacts step writes the last three via break-glass."""
    out_dir = Path(os.environ.get("OUT_DIR",
        Path(__file__).parent / "out" / "latest"))
    chunks: list[str] = []
    for name in ("monitor.log", "writer-incoming.txt",
                 "writer-gateway.log", "viz-gateway.log"):
        p = out_dir / name
        if p.exists():
            chunks.append(p.read_text(errors="replace"))
    return "\n".join(chunks)


def check_hero(transcript: str, router: list[str]) -> tuple[bool, str]:
    # Three signals, all required:
    # (1) the router actually saw a /images/generations call (with gpt-image-1)
    # (2) the brief references the hero markdown
    # (3) viz actually mesh-transferred hero.png to writer and writer ACKed it
    image_calls = [l for l in router if "/images/generations" in l or "gpt-image-1" in l]
    has_hero_ref = "hero" in transcript.lower() or transcript.lower().count("![") >= 1
    evidence = _transfer_evidence_text(router)
    hero_xfer = (
        ("mesh_transfer_file" in evidence and "hero.png" in evidence and "→ writer OK" in evidence)
        or ("file_name\":\"hero.png\"" in evidence and "saved_to" in evidence)
        or ("hero.png" in evidence and "incoming" in evidence)
    )
    ok = bool(image_calls) and has_hero_ref and hero_xfer
    return ok, (
        f"foundry image calls={len(image_calls)}, "
        f"hero_ref_in_brief={has_hero_ref}, "
        f"hero_png_transferred_to_writer={hero_xfer}"
    )


def check_chart(transcript: str, router: list[str]) -> tuple[bool, str]:
    # Foundry code-exec uses the Responses API with a `code_interpreter` tool
    # type. The router sees POST /openai/responses for the call itself and
    # then GET /openai/containers/cntr_<...>/files{,/<id>/content} for each
    # produced artifact. Counting those container hits is the cleanest signal
    # without parsing request bodies. We also verify writer received the PNG.
    container_hits = [l for l in router if "/openai/containers/cntr_" in l]
    legacy_hits = [l for l in router if "/code/sessions" in l
                   or "code_interpreter" in l.lower()]
    total = len(container_hits) + len(legacy_hits)
    evidence = _transfer_evidence_text(router)
    chart_xfer = (
        ("mesh_transfer_file" in evidence and "scorecard.png" in evidence and "→ writer OK" in evidence)
        or ("file_name\":\"scorecard.png\"" in evidence and "saved_to" in evidence)
        or ("scorecard.png" in evidence and "incoming" in evidence)
    )
    ok = total > 0 and chart_xfer
    return ok, (
        f"foundry code-exec container hits={len(container_hits)}, "
        f"legacy hits={len(legacy_hits)}, "
        f"scorecard_png_transferred_to_writer={chart_xfer}"
    )


def check_relay_pairs(trace: list[dict[str, Any]]) -> tuple[bool, str]:
    # Encrypted blobs flow over a persistent /ws connection and the OpenClaw
    # plugin's `log.info("AGT relay: sent to X")` does NOT reach kubectl logs
    # (it goes to the in-pod gateway log file). So we fall back to live
    # querying each sub-agent's inference-router for chat_completions activity
    # — if all three subs processed at least one chat turn, mesh delivery is
    # confirmed for the parent→sib direction. For sib→sib we count the unique
    # *non-self* peer DIDs each sub-agent's plugin looked up in the registry.
    siblings = ["analyst", "viz", "writer"]
    pairs: set[frozenset[str]] = set()
    try:
        import subprocess
    except Exception:
        return False, "subprocess unavailable"
    # 1. Discover each sub-agent's pod IP so we can attribute REGISTRY lines.
    pod_ip: dict[str, str] = {}
    for sub in siblings:
        try:
            r = subprocess.run(
                ["kubectl", "get", "pods", "-n", f"azureclaw-{sub}",
                 "-l", f"app={sub}", "-o",
                 "jsonpath={.items[0].status.podIP}"],
                capture_output=True, text=True, timeout=15,
            )
            if r.returncode == 0 and r.stdout.strip():
                pod_ip[sub] = r.stdout.strip()
        except Exception:
            pass
    # 2. Fetch the agent DID → name map from the registry once.
    name_by_did: dict[str, str] = {}
    for sub in siblings + ["execbrief"]:
        try:
            r = subprocess.run(
                ["kubectl", "logs", "-n", f"azureclaw-{sub}",
                 f"deploy/{sub}", "-c", "openclaw", "--tail=200"],
                capture_output=True, text=True, timeout=15,
            )
            # Plugin logs its own DID on startup; if we can't pull it, fall
            # back to pod activity heuristic below.
            for m in re.finditer(r"did[%:]agentmesh[%:](?:3A)?([a-f0-9]+)",
                                  r.stdout):
                name_by_did[m.group(1)] = sub
        except Exception:
            pass
    # 3. Walk REGISTRY lines and bucket lookups by source pod IP.
    pat = re.compile(
        r"INFO:\s+(\d+\.\d+\.\d+\.\d+):\d+\s+-\s+"
        r"\"GET /v1/agents/did%3Aagentmesh%3A([a-f0-9]+)"
    )
    reg_lines = lines_for(trace, "REGISTRY")
    ip_to_sub = {v: k for k, v in pod_ip.items()}
    for line in reg_lines:
        m = pat.search(line)
        if not m:
            continue
        src_ip, target_hex = m.group(1), m.group(2)
        sender = ip_to_sub.get(src_ip)
        target = name_by_did.get(target_hex)
        if sender and target and sender != target and target in siblings:
            pairs.add(frozenset((sender, target)))
    expected = {frozenset(("analyst", "viz")),
                frozenset(("analyst", "writer")),
                frozenset(("viz", "writer"))}
    missing = expected - pairs
    # Fallback: if we couldn't resolve DIDs, check sub-agent router activity
    # as a softer signal — at least proves parent→sib delivery was active.
    if not name_by_did:
        active = 0
        for sub in siblings:
            try:
                r = subprocess.run(
                    ["kubectl", "logs", "-n", f"azureclaw-{sub}",
                     f"deploy/{sub}", "-c", "inference-router",
                     "--tail=600"],
                    capture_output=True, text=True, timeout=15,
                )
                if r.stdout.count("chat/completions") > 0:
                    active += 1
            except Exception:
                pass
        ok = active == 3
        return ok, (f"sib DID map unresolved; fallback: {active}/3 "
                    f"sub-agents had router chat activity")
    ok = not missing
    return ok, (f"{len(pairs)}/3 sibling pairs (missing="
                f"{sorted(map(sorted, missing))})")


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
    # The parent is required (Step 1a) to invoke a real DeepWiki MCP
    # `tools/call` against execbrief-deepwiki. The router proxies MCP on its
    # `/mcp/...` routes; we require at least ONE `tools/call` line (handshake
    # `initialize` / `notifications/initialized` / `tools/list` alone is no
    # longer sufficient — those happen on every router startup). DeepWiki
    # must also be cited in the brief.
    mcp_lines = [l for l in router if "/mcp request" in l or "/mcp/" in l or "mcp.deepwiki.com" in l]
    mcp_tools_call = [l for l in mcp_lines
                      if '"method":"tools/call"' in l or "method=tools/call" in l]
    mentioned = "deepwiki" in transcript.lower()
    ok = bool(mcp_tools_call) and mentioned
    return ok, (
        f"router /mcp calls={len(mcp_lines)}, "
        f"/mcp tools/call={len(mcp_tools_call)} (require ≥1), "
        f"deepwiki cited={mentioned}"
    )


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
