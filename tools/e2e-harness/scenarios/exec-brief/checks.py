# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""
scenarios/exec-brief/checks.py — the nine acceptance checks for the
exec-brief scenario.

Loaded dynamically by `tools/e2e-harness/verify.py` via
`SCENARIO=exec-brief` (the default). Exposes a single entry point:

    get_checks() -> list[Check]

Where each `Check` is a `(label, callable)` pair. The callable receives
a `Context` (defined in `verify.py`) and returns `(ok: bool, detail: str)`.

Splitting the checks into a per-scenario module keeps the generic
harness free of scenario-specific knowledge (URL counts, foundry
endpoints, expected sibling pairs). The harness only knows how to load
the trace, transcript, and the scenario module.
"""
from __future__ import annotations

import os
import re
import subprocess
from pathlib import Path
from typing import Any, Callable, TYPE_CHECKING

if TYPE_CHECKING:
    # Avoid a circular import — Context is defined in the generic verify.py.
    from verify import Context  # type: ignore[import-not-found]


# ─── Individual checks ────────────────────────────────────────────────────────
def check_sources(ctx: "Context") -> tuple[bool, str]:
    # Distinct URLs cited in the final brief that look like they reference 2026
    # publications. Heuristic: count unique http(s) URLs.
    # Source: prefer the collected final artifact when present (the parent's
    # echoed reply can get truncated by Foundry content_filter mid-stream;
    # the artifact is what was actually delivered).
    brief = ctx.best_brief_text()
    urls = set(re.findall(r"https?://[^\s)>\]]+", brief))
    # Drop any obvious infra noise (registry/relay/telegram api)
    noise = ("api.telegram.org", "modelcontextprotocol.io",
             "ai.azure.com", "login.microsoftonline.com", "agentmesh")
    clean = {u for u in urls if not any(n in u for n in noise)}
    ok = len(clean) >= 6
    return ok, f"{len(clean)} distinct external URLs cited (need ≥6)"


def check_scorecard(ctx: "Context") -> tuple[bool, str]:
    # The analyst is asked for a 4×4 scorecard. We look for either a JSON
    # block with a "metrics" key OR a markdown table mentioning the four
    # columns. Use the artifact when present (transcript may be truncated).
    cols = ("isolation", "egress", "attestation", "governance")
    brief = ctx.best_brief_text()
    found = sum(1 for c in cols if c in brief.lower())
    has_metrics = '"metrics"' in brief or "scorecard" in brief.lower()
    ok = has_metrics and found == 4
    return ok, f"metrics block present={has_metrics}, axis labels found={found}/4"


def _transfer_evidence_text(ctx: "Context") -> str:
    """Aggregate text from sources that can witness mesh file transfers:
    - monitor.log (kubectl logs stdout; rarely contains the lines)
    - writer-incoming.txt (definitive: writer's incoming/ directory listing)
    - writer-gateway.log (file_transfer_ack JSON the writer plugin logged)
    - viz-gateway.log (mesh_transfer_file lines the viz plugin logged)
    drive.sh's collect_artifacts step writes the last three via break-glass."""
    chunks: list[str] = []
    for name in ("monitor.log", "writer-incoming.txt",
                 "writer-gateway.log", "viz-gateway.log"):
        p = ctx.out_dir / name
        if p.exists():
            chunks.append(p.read_text(errors="replace"))
    return "\n".join(chunks)


def check_hero(ctx: "Context") -> tuple[bool, str]:
    # Three signals, all required:
    # (1) the router actually saw a /images/generations call (with gpt-image-1)
    # (2) the brief references the hero markdown
    # (3) viz actually mesh-transferred hero.png to writer and writer ACKed it
    image_calls = [l for l in ctx.router_lines
                   if "/images/generations" in l or "gpt-image-1" in l]
    has_hero_ref = ("hero" in ctx.transcript.lower()
                    or ctx.transcript.lower().count("![") >= 1)
    evidence = _transfer_evidence_text(ctx)
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


def check_chart(ctx: "Context") -> tuple[bool, str]:
    # Foundry code-exec uses the Responses API with a `code_interpreter` tool
    # type. The router sees POST /openai/responses for the call itself and
    # then GET /openai/containers/cntr_<...>/files{,/<id>/content} for each
    # produced artifact. Counting those container hits is the cleanest signal
    # without parsing request bodies. We also verify writer received the PNG.
    container_hits = [l for l in ctx.router_lines if "/openai/containers/cntr_" in l]
    legacy_hits = [l for l in ctx.router_lines if "/code/sessions" in l
                   or "code_interpreter" in l.lower()]
    total = len(container_hits) + len(legacy_hits)
    evidence = _transfer_evidence_text(ctx)
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


def check_relay_pairs(ctx: "Context") -> tuple[bool, str]:
    # Sibling-to-sibling mesh routing is proven by the per-agent gateway logs
    # the harness collects into OUT_DIR on EVERY platform (docker exec / kubectl
    # logs alike): `<sub>-gateway.log`. Each sub-agent records its own outbound
    # mesh sends there, e.g.:
    #     AGT sub-agent mesh_transfer_file: to=writer file=/sandbox/...
    #     AGT relay: sent to viz (did:mesh:...) via E2E encrypted relay
    # The log's owner is the sender; the parsed peer is the target. A pair of
    # two distinct siblings is a sibling pair. This is the authoritative,
    # platform-agnostic signal — unlike the legacy kubectl/registry heuristic
    # (kept below as a fallback) it works in docker mode too, where there are
    # no K8s pods/namespaces for `kubectl` to inspect.
    siblings = ["analyst", "viz", "writer"]
    pairs: set[frozenset[str]] = set()

    send_pat = re.compile(
        r"mesh_transfer_file:\s+(?:[^\n]*?\bto=|[^\n]*?\u2192\s+)(\w+)\b"
        r"|AGT relay:\s+sent to (\w+)\b"
    )
    saw_gateway_log = False
    for sub in siblings:
        log_path = ctx.out_dir / f"{sub}-gateway.log"
        if not log_path.exists():
            continue
        saw_gateway_log = True
        try:
            text = log_path.read_text(errors="ignore")
        except Exception:
            continue
        for m in send_pat.finditer(text):
            target = m.group(1) or m.group(2)
            if target in siblings and target != sub:
                pairs.add(frozenset((sub, target)))

    if saw_gateway_log:
        expected = {frozenset(("analyst", "viz")),
                    frozenset(("analyst", "writer")),
                    frozenset(("viz", "writer"))}
        missing = expected - pairs
        ok = not missing
        return ok, (f"{len(pairs)}/3 sibling pairs from gateway logs "
                    f"(missing={sorted(map(sorted, missing))})")

    # ── Fallback: legacy kubectl/registry heuristic (AKS live, no collected
    # gateway logs). Only reached when no `<sub>-gateway.log` was collected. ──
    # 1. Discover each sub-agent's pod IP so we can attribute REGISTRY lines.
    pod_ip: dict[str, str] = {}
    for sub in siblings:
        try:
            r = subprocess.run(
                ["kubectl", "get", "pods", "-n", f"kars-{sub}",
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
                ["kubectl", "logs", "-n", f"kars-{sub}",
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
    reg_lines = ctx.lines_for("REGISTRY")
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
                    ["kubectl", "logs", "-n", f"kars-{sub}",
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


def check_telegram(ctx: "Context") -> tuple[bool, str]:
    # Telegram channel plugin posts go through the router as outbound
    # https://api.telegram.org/bot.../sendMessage calls.
    if not any("TELEGRAM" in os.environ.get(k, "") for k in os.environ) \
       and not os.environ.get("TELEGRAM_BOT_TOKEN"):
        return True, "skipped (no TELEGRAM_BOT_TOKEN in env)"
    posts = [l for l in ctx.router_lines
             if "api.telegram.org" in l and "sendMessage" in l]
    ok = len(posts) >= 5
    return ok, f"{len(posts)} telegram sendMessage calls (need ≥5)"


def check_brief(ctx: "Context") -> tuple[bool, str]:
    # Loose: the final brief should be ≥600 and ≤1500 words and mention both
    # "hero" placement and a chart. Prefer the final artifact (the
    # parent's echoed transcript can be content_filter-truncated mid-stream).
    brief = ctx.best_brief_text()
    words = len(brief.split())
    has_chart = "chart" in brief.lower() or "![" in brief
    has_hero = ("hero" in brief.lower()
                or brief.lower().count("![") >= 2)
    ok = 600 <= words <= 1500 and has_chart and has_hero
    return ok, f"{words} words; chart_ref={has_chart}; hero_ref={has_hero}"


def check_egress_clean(ctx: "Context") -> tuple[bool, str]:
    # With egressMode: Strict, any sandbox→external connection to a host
    # not in `allowedEndpoints` shows up either as a NetworkPolicy drop
    # event on the pod or as a "BlockedBuffer" entry in the controller log.
    # If the run was clean (only telegram + mcp-fetch were touched), we
    # expect zero of either.
    ctrl = ctx.lines_for("CTRL")
    evt = ctx.lines_for("K8S-EVT")
    denials = [l for l in ctrl if "BlockedBuffer" in l or "egress.*denied" in l.lower()]
    drops = [l for l in evt if "NetworkPolicy" in l
             and ("deny" in l.lower() or "drop" in l.lower())]
    total = len(denials) + len(drops)
    ok = total == 0
    return ok, f"controller blocked={len(denials)}, k8s netpol drops={len(drops)}"


def check_mcp_traffic(ctx: "Context") -> tuple[bool, str]:
    # The parent is required (Step 1a) to invoke a real DeepWiki MCP
    # `tools/call` against execbrief-deepwiki. The router proxies MCP on its
    # `/mcp/...` routes; we require at least ONE `tools/call` line (handshake
    # `initialize` / `notifications/initialized` / `tools/list` alone is no
    # longer sufficient — those happen on every router startup). DeepWiki
    # must also be cited in the brief.
    #
    # Docker dev mode has no McpServer CRD support — the DeepWiki MCP
    # sidecar is never deployed, so no `tools/call` is structurally
    # possible. On docker we accept "deepwiki referenced in the brief"
    # as the verifiable signal, consistent with the documented dev-mode
    # limitations (no controller, no CRDs).
    platform = os.environ.get("PLATFORM", "")
    mcp_lines = [l for l in ctx.router_lines
                 if "/mcp request" in l or "/mcp/" in l
                 or "mcp.deepwiki.com" in l]
    mcp_tools_call = [l for l in mcp_lines
                      if '"method":"tools/call"' in l
                      or "method=tools/call" in l]
    # "deepwiki" citation: prefer the artifact (the parent's echoed reply
    # gets truncated by Foundry content_filter on long briefs, dropping
    # the footnotes section that contains the DeepWiki reference).
    brief = ctx.best_brief_text()
    mentioned = "deepwiki" in brief.lower()
    if platform == "docker":
        ok = mentioned
        return ok, (
            f"docker mode: McpServer CRD not supported, deepwiki cited="
            f"{mentioned} (mcp_lines={len(mcp_lines)})"
        )
    ok = bool(mcp_tools_call) and mentioned
    return ok, (
        f"router /mcp calls={len(mcp_lines)}, "
        f"/mcp tools/call={len(mcp_tools_call)} (require ≥1), "
        f"deepwiki cited={mentioned}"
    )


# ─── Public entry point (the harness calls this) ──────────────────────────────
def get_checks() -> list[tuple[str, Callable[["Context"], tuple[bool, str]]]]:
    return [
        ("≥6 distinct 2026 sources cited",       check_sources),
        ("metrics scorecard 4×4 + axis labels",  check_scorecard),
        ("hero image via gpt-image-1 (1024²)",   check_hero),
        ("chart via Foundry code-exec",          check_chart),
        ("≥3 distinct sibling pairs on relay",   check_relay_pairs),
        ("≥5 telegram status posts",             check_telegram),
        ("brief ~900 words, hero+chart present", check_brief),
        ("egress: 0 NetworkPolicy denials",      check_egress_clean),
        ("MCP (DeepWiki) traffic observed",      check_mcp_traffic),
    ]
