#!/usr/bin/env python3
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""
format_demo.py — live storyboard renderer for the e2e harness.

Watches drive.log + trace.jsonl in real time and renders a clean,
demo-friendly view to stdout. Suppresses health-check / leader-election
noise; surfaces the meaningful milestones (apply, sandbox Ready, prompt
posted, sub-agents spawned, mesh KNOCK, web search, image generation,
code-exec, file transfer, final reply) with one-line evidence pulled
from the underlying JSON.

Usage:
    # tail an existing run
    python3 tools/e2e-harness/format_demo.py [out_dir]

    # invoked by run.sh --demo (foreground, exits when driver finishes)
    DEMO=1 ./tools/e2e-harness/run.sh

Design:
    * Single-screen-friendly: each milestone is one line, ≤120 chars.
    * Section headers gate phase transitions visually.
    * Counters (sources cited, files transferred, image calls) tick
      live so the operator can show the dashboard portal mid-stream
      while the count climbs.
    * Final summary mirrors verify.py's check results once it lands.
    * Never blocks: only printing, never executing kubectl/az.
"""
from __future__ import annotations

import json
import os
import re
import signal
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional


# ─── ANSI palette ────────────────────────────────────────────────────────────
if os.environ.get("NO_COLOR") or not sys.stdout.isatty():
    BOLD = DIM = RESET = ""
    CYAN = GREEN = YELLOW = MAGENTA = BLUE = RED = WHITE = ""
else:
    BOLD = "\033[1m"
    DIM = "\033[2m"
    RESET = "\033[0m"
    CYAN = "\033[36m"
    GREEN = "\033[32m"
    YELLOW = "\033[33m"
    MAGENTA = "\033[35m"
    BLUE = "\033[34m"
    RED = "\033[31m"
    WHITE = "\033[37m"


ICON_OK = f"{GREEN}✓{RESET}"
ICON_NEW = f"{CYAN}✦{RESET}"
ICON_PHASE = f"{BLUE}▸{RESET}"
ICON_INFO = f"{DIM}·{RESET}"
ICON_BOLT = f"{YELLOW}⚡{RESET}"
ICON_MESH = f"{MAGENTA}⇄{RESET}"
ICON_FILE = f"{CYAN}📄{RESET}" if sys.stdout.encoding and "utf" in sys.stdout.encoding.lower() else f"{CYAN}≡{RESET}"
ICON_AGENT = f"{MAGENTA}◉{RESET}"
ICON_LLM = f"{YELLOW}✱{RESET}"
ICON_FAIL = f"{RED}✗{RESET}"


@dataclass
class Counter:
    sub_agents: set[str] = field(default_factory=set)
    foundry_chat_calls: int = 0
    image_gen_calls: int = 0
    code_exec_calls: int = 0
    web_search_calls: int = 0
    mcp_calls: int = 0
    file_transfers: int = 0
    mesh_relay_connects: set[str] = field(default_factory=set)
    foundry_byte_total: int = 0


# Pre-compile the suppression regex once. Anything matching is dropped
# from the demo view — these lines are pure noise (health probes,
# leader election retries, blocklist housekeeping).
SUPPRESS_RE = re.compile(
    r"(GET /health|leader-election|Blocklist (refresh|reloaded|enabled|loaded|refreshed)|"
    r"Could not create token-budget|Sidecar auth mode|TrustGraph projection|"
    r"Foundry stream headers received|Stream response status|Sending upstream|"
    r"Forwarding (inference|SSE)|graceful shutdown|Native AGT governance initialized|"
    r"Discovered 1 McpServer|Loaded (policy|seed)|InferencePolicy loaded|"
    r"KarsMemory binding|EgressAllowlist bundle|Egress allowlist replaced|"
    r"AGT governance: loaded|Mounting /|Mounted /|Auth mode:|kars Inference Router starting|"
    r"Registry topology|Listening on 0\.0\.0\.0|Forward proxy listening|"
    r"\[entrypoint\]|\[kars\] (OpenClaw configured|Foundry \+ governance|ClawHub|@kars/mesh|"
    r"mesh provider|AGT governance|Plugin installed|Inference router provided|"
    r"Gateway running|Node host)|"
    r"egress-guard:|prekeys|CONNECT (request|blocked)|"
    r"AGT relay WebSocket proxy disconnected|"
    r"Failed to open audit JSONL writer)"
)
# NOTE: "Proxying Foundry Agent API" + "Foundry Agent API complete" are
# NOT in SUPPRESS_RE — the ROUTER handler counts them by `path` field
# to surface web_search (/openai/responses), code_execute (/openai/containers),
# and chat turns (/openai/conversations).


def print_phase(title: str) -> None:
    print()
    print(f"  {ICON_PHASE} {BOLD}{title}{RESET}")


def print_step(text: str, icon: str = ICON_OK) -> None:
    print(f"      {icon} {text}")


def print_detail(text: str) -> None:
    print(f"        {DIM}{text}{RESET}")


def parse_inner(msg: str) -> tuple[Optional[str], dict]:
    """Extract the human-readable message + fields from a router/ctrl JSON line."""
    try:
        inner = json.loads(msg)
    except Exception:
        return None, {}
    fields = inner.get("fields") or {}
    message = fields.get("message") or inner.get("message")
    return message, fields


def follow(path: Path, replay: bool = False):
    """Generator: yield lines, or yield None on no-data so the caller
    can pump the other source. Critical for live mode where drive.log
    and trace.jsonl write at very different rates — if this blocked
    internally, the main loop would never get to the trace generator.
    Replay mode exits at EOF.
    """
    last_size = 0
    last_growth = time.time()
    fh = None
    while True:
        if not path.exists():
            if replay:
                return
            yield None
            if time.time() - last_growth > 120:
                return
            continue
        if fh is None:
            fh = path.open("r", encoding="utf-8", errors="replace")
        line = fh.readline()
        if line:
            last_growth = time.time()
            yield line.rstrip("\n")
        else:
            if replay:
                return
            cur = path.stat().st_size
            if cur < last_size:
                # File rotated — reopen
                fh.close()
                fh = None
                last_size = 0
                continue
            last_size = cur
            yield None


def handle_drive_line(line: str, state: dict, ctr: Counter) -> None:
    """Surface high-level driver events. Drive lines are sparse and already curated."""
    if "applying" in line and "/manifests/*.yaml" in line:
        print_phase("Applying scenario manifests")
        state["phase"] = "apply"
    elif re.search(r"  -> (\d+-[^\s]+\.yaml)$", line):
        m = re.search(r"  -> (\d+-[^\s]+\.yaml)$", line)
        if m:
            print_step(f"kubectl apply: {m.group(1)}", ICON_NEW)
    elif "waiting for KarsSandbox" in line:
        m = re.search(r"KarsSandbox/(\S+) → Ready", line)
        sandbox = m.group(1) if m else "?"
        print_phase(f"Waiting for KarsSandbox/{sandbox} → Ready")
        state["phase"] = "wait_ready"
        state["sandbox"] = sandbox
    elif "sandbox Ready" in line:
        print_step(f"KarsSandbox/{state.get('sandbox','?')} is Ready", ICON_OK)
    elif "posting" in line and "prompt" in line:
        print_phase(f"Posting prompt to {state.get('sandbox','?')} gateway")
        state["phase"] = "prompt"
        # Inline the prompt text — this is the operator's reference for the demo.
        prompt_path = state.get("prompt_path")
        if prompt_path and prompt_path.exists():
            text = prompt_path.read_text(encoding="utf-8", errors="replace").strip()
            print()
            print(f"        {DIM}┌─ Prompt ──────────────────────────────────────────────────────────┐{RESET}")
            for prl in text.splitlines():
                # Wrap long lines at 64 chars for the demo panel.
                while len(prl) > 64:
                    print(f"        {DIM}│{RESET} {prl[:64]}")
                    prl = prl[64:]
                print(f"        {DIM}│{RESET} {prl}")
            print(f"        {DIM}└───────────────────────────────────────────────────────────────────┘{RESET}")
            print()
    elif "gateway reachable" in line:
        print_step("Gateway reachable via port-forward", ICON_OK)
    elif "session_id=" in line:
        m = re.search(r"session_id=(\S+)", line)
        if m:
            print_detail(f"session_id={m.group(1)}")
            print_phase("Agent at work — watching mesh + Foundry traffic")
            state["phase"] = "running"
    elif "prompt completed" in line:
        print_phase("Final reply received")
        state["phase"] = "done"
        # Surface response stats (length, references) inline from response.json.
        rp = state.get("response_path")
        if rp and rp.exists():
            try:
                resp = json.loads(rp.read_text(encoding="utf-8", errors="replace"))
                content = resp["choices"][0]["message"]["content"]
                usage = resp.get("usage", {})
                words = len(content.split())
                refs = len(set(re.findall(r"https?://\S+", content)))
                images = len(re.findall(r"!\[[^\]]*\]\(", content))
                print_step(f"{words} words, {refs} distinct sources, {images} embedded images", ICON_OK)
                print_detail(
                    f"tokens: {usage.get('prompt_tokens','?')} in / "
                    f"{usage.get('completion_tokens','?')} out / "
                    f"{usage.get('total_tokens','?')} total"
                )
            except Exception:
                pass
    elif "trace.jsonl assembled" in line:
        m = re.search(r"(\d+) lines", line)
        if m:
            print_step(f"trace.jsonl assembled — {m.group(1)} events", ICON_INFO)
    elif "driver done" in line:
        m = re.search(r"OUT_DIR=(\S+)", line)
        if m:
            print_detail(f"out_dir={m.group(1)}")


def handle_trace_line(line: str, state: dict, ctr: Counter) -> None:
    """Surface mesh + Foundry events from the per-pod monitor stream."""
    try:
        ev = json.loads(line)
    except Exception:
        return
    src = ev.get("src", "?")
    msg = ev.get("msg", "")
    # NOTE: do NOT apply SUPPRESS_RE at the raw line level — the inner
    # JSON often contains a noise message AND useful path/method fields
    # we need (e.g. Foundry path-based counting). Suppression happens
    # inside each src branch after parsing.

    # CTRL emits CRD reconcile events — surface the sub-agent creations.
    if src == "CTRL":
        inner_msg, fields = parse_inner(msg)
        if not inner_msg:
            return
        if SUPPRESS_RE.search(inner_msg):
            return
        if "Reconciling KarsSandbox" in inner_msg:
            name = fields.get("name")
            if name and name not in state.get("seen_reconciles", set()):
                state.setdefault("seen_reconciles", set()).add(name)
                # Don't print here — too noisy. Just track.
        elif "Sandbox is now Ready" in inner_msg or "phase=Ready" in inner_msg:
            name = fields.get("name") or fields.get("sandbox")
            if name and name in {"analyst", "viz", "writer"} and name not in state.get("seen_ready", set()):
                state.setdefault("seen_ready", set()).add(name)
                print_step(f"sub-agent {ICON_AGENT} {name} sandbox Ready", ICON_NEW)
        return

    # ROUTER emits all the interesting stuff: spawns, mesh, foundry.
    if src.startswith("ROUTER"):
        inner_msg, fields = parse_inner(msg)
        if not inner_msg:
            return
        # Path-aware Foundry API counting: the "Proxying" line carries
        # the method+path that distinguishes web_search vs code_execute
        # vs chat. Only count the *request* edge to avoid double-counting.
        if inner_msg == "Proxying Foundry Agent API":
            path = (fields or {}).get("path", "")
            method = (fields or {}).get("method", "")
            if "/openai/responses" in path:
                ctr.web_search_calls += 1
                if ctr.web_search_calls in (1, 2, 5, 10, 20):
                    print_step(f"Foundry web_search via /openai/responses ({ctr.web_search_calls})", ICON_BOLT)
            elif "/openai/containers" in path:
                ctr.code_exec_calls += 1
                if ctr.code_exec_calls <= 4 and method == "POST":
                    print_step(f"Foundry code_execute via /openai/containers #{ctr.code_exec_calls}", ICON_BOLT)
            elif "/openai/conversations" in path and method == "POST":
                ctr.foundry_chat_calls += 1
                if ctr.foundry_chat_calls in (1, 5, 10, 20):
                    print_step(f"agent LLM turn #{ctr.foundry_chat_calls}", ICON_LLM)
            return
        if inner_msg == "Foundry Agent API complete" or inner_msg == "Foundry complete":
            return  # quiet — request edge already counted
        if SUPPRESS_RE.search(inner_msg):
            return

        if "Sub-agent sandbox created" in inner_msg:
            child = fields.get("child", "?")
            parent = fields.get("parent", "?")
            if child not in ctr.sub_agents:
                ctr.sub_agents.add(child)
                print_step(f"spawned sub-agent {ICON_AGENT} {BOLD}{child}{RESET} (parent={parent})", ICON_NEW)
        elif "AGT relay WebSocket proxy connected" in inner_msg:
            # One per pod — useful "mesh online" signal for the parent.
            who = fields.get("sandbox") or src
            if who not in ctr.mesh_relay_connects:
                ctr.mesh_relay_connects.add(who)
                if len(ctr.mesh_relay_connects) == 1:
                    print_step("parent ↔ AGT relay connected (E2E mesh online)", ICON_MESH)
                else:
                    print_step(f"sub-agent mesh online ({len(ctr.mesh_relay_connects)}/4 peers)", ICON_MESH)
        elif "/mcp request" in inner_msg:
            tool = fields.get("tool") or fields.get("method") or "?"
            if "tools/call" in (fields.get("method") or ""):
                ctr.mcp_calls += 1
                print_step(f"MCP tools/call → {tool}", ICON_BOLT)
        elif "Image generation request" in inner_msg:
            ctr.image_gen_calls += 1
            print_step(f"Foundry image_generation #{ctr.image_gen_calls} (gpt-image-1)", ICON_BOLT)
        elif "Image generation complete" in inner_msg:
            sz = fields.get("size") or fields.get("bytes")
            extra = f" — {sz} bytes" if sz else ""
            print_detail(f"image returned{extra}")
        return

    if src == "RELAY":
        inner_msg, _ = parse_inner(msg)
        if inner_msg and ("WebSocket /ws" in inner_msg or "connection open" in inner_msg):
            # Already covered by ROUTER's mesh-online signal.
            return

    if src.startswith("POD"):
        # POD lines are extremely noisy. Pick only the milestone-worthy ones.
        if "Gateway running" in msg and src == "POD":
            # parent gateway up — covered by drive log
            return
        if "mesh_transfer_file" in msg or "file_transfer_ack" in msg:
            ctr.file_transfers += 1
            sender = src.replace("POD-", "")
            print_step(f"E2E file transfer ({sender}) #{ctr.file_transfers}", ICON_FILE)


def print_header(out_dir: Path, scenario: str) -> None:
    print()
    print(f"  {BOLD}{BLUE}┏━━━ kars e2e harness — live demo view ━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓{RESET}")
    print(f"  {BOLD}{BLUE}┃{RESET}  scenario  {BOLD}{scenario}{RESET}")
    print(f"  {BOLD}{BLUE}┃{RESET}  out_dir   {DIM}{out_dir}{RESET}")
    print(f"  {BOLD}{BLUE}┃{RESET}  context   {DIM}kubectl context = current; portal links shown below{RESET}")
    print(f"  {BOLD}{BLUE}┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛{RESET}")
    print()
    print(f"  {DIM}Open in parallel for the demo:{RESET}")
    print(f"    {DIM}• Headlamp:   {RESET}http://localhost:4466/")
    print(f"    {DIM}• Grafana:    {RESET}http://localhost:3000/   (admin/admin)")
    print(f"    {DIM}• Prometheus: {RESET}http://localhost:19091/")
    print(f"    {DIM}• Portal:     {RESET}https://portal.azure.com → kars-* resource group")


def print_summary(out_dir: Path, ctr: Counter) -> None:
    verify = out_dir / "verify.json"
    if not verify.exists():
        # verify.py may not have run yet — wait briefly.
        for _ in range(20):
            if verify.exists():
                break
            time.sleep(0.5)
    print()
    print(f"  {BOLD}{BLUE}━━━ counters ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━{RESET}")
    print(f"    {ICON_AGENT} sub-agents spawned        {BOLD}{len(ctr.sub_agents)}{RESET}  {DIM}({', '.join(sorted(ctr.sub_agents)) or '-'}){RESET}")
    print(f"    {ICON_MESH} mesh peers online         {BOLD}{len(ctr.mesh_relay_connects)}{RESET}")
    print(f"    {ICON_BOLT} Foundry image_generation  {BOLD}{ctr.image_gen_calls}{RESET}")
    print(f"    {ICON_BOLT} Foundry code_execute      {BOLD}{ctr.code_exec_calls}{RESET}")
    print(f"    {ICON_BOLT} Foundry web_search        {BOLD}{ctr.web_search_calls}{RESET}")
    print(f"    {ICON_BOLT} MCP tools/call            {BOLD}{ctr.mcp_calls}{RESET}")
    print(f"    {ICON_FILE} E2E file transfers        {BOLD}{ctr.file_transfers}{RESET}")
    if verify.exists():
        try:
            data = json.loads(verify.read_text(encoding="utf-8"))
            all_passed = data.get("all_passed")
            checks = data.get("checks", [])
            print()
            print(f"  {BOLD}{BLUE}━━━ verify.py ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━{RESET}")
            for c in checks:
                icon = ICON_OK if c.get("passed") else ICON_FAIL
                name = c.get("check", "?")
                print(f"    {icon} {name}")
                if c.get("detail"):
                    print(f"        {DIM}{c['detail']}{RESET}")
            print()
            verdict = f"{GREEN}{BOLD}ALL CHECKS PASSED{RESET}" if all_passed else f"{RED}{BOLD}SOME CHECKS FAILED{RESET}"
            print(f"  {verdict}")
        except Exception as e:
            print(f"  {DIM}(could not parse verify.json: {e}){RESET}")
    print()


def run_replay(out_dir: Path, state: dict, ctr: Counter) -> None:
    """Replay an existing run by walking drive.log narratively and dumping
    all trace events under the "Agent at work" phase (where they belong).

    We can't reliably timestamp-merge because monitor.sh's `%3N` date
    format is GNU-only — on macOS the `ts` field is malformed
    ("12:39:07.3NZ"). Phase-bucketed replay sidesteps that entirely
    and yields the narrative an operator wants for the demo: setup
    steps first, mesh+Foundry activity second, final reply third.
    """
    drive_log = out_dir / "drive.log"
    trace = out_dir / "trace.jsonl"

    drive_lines = drive_log.read_text(encoding="utf-8", errors="replace").splitlines() if drive_log.exists() else []
    trace_lines = trace.read_text(encoding="utf-8", errors="replace").splitlines() if trace.exists() else []

    # Phase 1: drive lines up to and including session_id=… (apply → Ready → posted)
    cursor = 0
    for i, line in enumerate(drive_lines):
        handle_drive_line(line, state, ctr)
        cursor = i + 1
        if "session_id=" in line:
            break

    # Phase 2: dump every trace line (in file order, which is roughly
    # chronological — monitor.sh appends concurrently from multiple
    # log-tailers so it's not perfectly sorted, but it's the best
    # available signal). Counters get bumped, and the more interesting
    # events (spawn, mesh online, first web_search, first image,
    # MCP tools/call) print inline.
    for line in trace_lines:
        handle_trace_line(line, state, ctr)

    # Phase 3: remaining drive lines (prompt completed → driver done)
    for line in drive_lines[cursor:]:
        handle_drive_line(line, state, ctr)


def main() -> int:
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    flags = [a for a in sys.argv[1:] if a.startswith("--")]
    replay = "--replay" in flags
    out_arg = args[0] if args else None
    if out_arg:
        out_dir = Path(out_arg)
    else:
        out_dir = Path(os.environ.get("OUT_DIR") or Path(__file__).parent / "out" / "latest")
    if not out_dir.exists():
        print(f"out_dir not found: {out_dir}", file=sys.stderr)
        return 1
    out_dir = out_dir.resolve()

    scenario = os.environ.get("SCENARIO") or "exec-brief"
    scenario_dir = Path(__file__).parent / "scenarios" / scenario

    state = {
        "phase": "init",
        "prompt_path": scenario_dir / "prompt.txt",
        "response_path": out_dir / "response.json",
    }
    ctr = Counter()

    print_header(out_dir, scenario)

    if replay:
        run_replay(out_dir, state, ctr)
        print_summary(out_dir, ctr)
        return 0

    drive_log = out_dir / "drive.log"
    trace = out_dir / "trace.jsonl"

    # Block until at least drive.log appears.
    waited = 0
    while not drive_log.exists():
        time.sleep(0.5)
        waited += 1
        if waited > 60:
            print(f"  {ICON_FAIL} drive.log never appeared", file=sys.stderr)
            return 1

    drive_gen = follow(drive_log)
    trace_gen = follow(trace) if trace.exists() else None

    sigint = {"got": False}
    def _sig(_s, _f): sigint["got"] = True
    signal.signal(signal.SIGINT, _sig)

    drive_done = drive_gen is None
    trace_done = trace_gen is None
    idle_since_done = None
    # Interleave: pull from both generators round-robin. Each generator
    # yields either a line (process it) or None (no data right now —
    # try the other source).
    while not sigint["got"] and not (drive_done and trace_done):
        progress = False
        if not drive_done:
            try:
                line = next(drive_gen)
                if line is not None:
                    handle_drive_line(line, state, ctr)
                    progress = True
            except StopIteration:
                drive_done = True
        if trace_gen is None and trace.exists():
            trace_gen = follow(trace)
            trace_done = False
        if not trace_done and trace_gen is not None:
            try:
                line = next(trace_gen)
                if line is not None:
                    handle_trace_line(line, state, ctr)
                    progress = True
            except StopIteration:
                trace_done = True
        if not progress:
            # Stay alive after the driver finishes so the trace drain
            # AND verify.py have time to land. Exit only when either:
            #   * verify.json exists (verify.py wrote it — done)
            #   * SIGINT (cleanup() in run.sh)
            #   * 90s after phase=done with no verify.json (verify
            #     either skipped or crashed)
            if state.get("phase") == "done":
                verify_json = out_dir / "verify.json"
                if verify_json.exists():
                    # Give it one more drain pass to catch any late
                    # mesh-teardown events, then exit.
                    if idle_since_done is None:
                        idle_since_done = time.time()
                    elif time.time() - idle_since_done > 2:
                        break
                else:
                    if idle_since_done is None:
                        idle_since_done = time.time()
                    elif time.time() - idle_since_done > 90:
                        break
            time.sleep(0.05)

    print_summary(out_dir, ctr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
