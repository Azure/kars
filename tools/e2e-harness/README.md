# kars e2e-harness

A scenario- and platform-pluggable end-to-end test harness for kars.

This was previously the `tools/exec-brief-e2e/` directory; it has been
refactored so the orchestration logic is independent of any one
scenario or deployment target. The original "executive-brief on AKS"
test is now one concrete scenario × platform pair under this framework.

## Layout

```
tools/e2e-harness/
├── drive.sh                 # generic outer loop (SCENARIO × PLATFORM)
├── verify.py                # generic verifier; dynamic-imports
│                            #   scenarios/<SCENARIO>/checks.py
├── monitor.sh               # live colour-coded tail (kubectl-based;
│                            #   skipped on docker)
├── run.sh                   # convenience wrapper (monitor + drive + verify)
├── platforms/
│   ├── aks.sh               # AKS / any K8s cluster you're already
│   │                        #   kubectl-logged into. Reference impl.
│   ├── local-k8s.sh         # Sources aks.sh; brings up a kind cluster
│   │                        #   via `kars dev --target local-k8s`.
│   └── docker.sh            # Single-host docker target via
│                            #   `kars dev --target docker`. No
│                            #   CRDs / no K8s API.
└── scenarios/
    └── exec-brief/
        ├── config.sh        # scenario bash knobs (sandbox names,
        │                    #   per-sub grep patterns, incoming dir)
        ├── prompt.txt       # the verbatim user prompt
        ├── checks.py        # `get_checks()` → list of (label, fn)
        ├── manifests/       # CRD manifests applied by K8s platforms
        └── (optional manifests-docker/)
                             # docker-only setup shell snippets
```

## Run

```bash
# default: SCENARIO=exec-brief PLATFORM=aks
SCENARIO=exec-brief PLATFORM=aks ./run.sh

# Demo mode — clean storyboard view to stdout (suppresses raw monitor
# stream, surfaces phases + Foundry/mesh counters + final verify panel).
# Raw logs still land in out/<runId>/ for verify.py.
DEMO=1 SCENARIO=exec-brief PLATFORM=aks ./run.sh

# Replay a completed run with the same storyboard (no live tail).
python3 tools/e2e-harness/format_demo.py --replay tools/e2e-harness/out/<runId>

# Replay at a pace tuned for demo recording (looks like a live run
# without the 6-minute wait — pre-bake the run, then replay):
#   --pace=1.0  → ~25s   (fast)
#   --pace=1.5  → ~37s   (natural — matches the 2-min demo runbook)
#   --pace=2.0  → ~50s   (slow, more voiceover headroom)
python3 tools/e2e-harness/format_demo.py --replay --pace=1.5 \
        tools/e2e-harness/out/latest
```

Output lands under `out/<runId>/`:

```
out/2025-…/
├── trace.jsonl              # JSONL of every event monitor.sh saw
├── transcript.log           # parent agent's reply (raw markdown)
├── brief.html               # transcript.log rendered as a polished HTML
│                            # page — auto-opens in your browser at the
│                            # end of the run. Image refs rewritten to
│                            # the local PNGs copied out of the writer
│                            # sandbox so hero/scorecard render inline.
├── *.png / *.jpg            # image artefacts copied from the writer
│                            # sandbox's incoming/ dir
├── platform-notes.txt       # platform caveats (e.g. kindnetd vs Cilium)
├── *-gateway.log            # per-sub-agent OpenClaw gateway logs
├── …-incoming.txt           # ls of any file-transfer destination dir
└── verify.json              # final verdict (`pass: bool`, per-check status)
```

Set `NO_OPEN_BROWSER=1` to skip the browser open at the end (the HTML
is always rendered regardless).

## Platforms

| Platform   | Bring-up                                  | Status |
|------------|-------------------------------------------|--------|
| `aks`      | Assumes `kubectl` context is set to a cluster where `kars up` has succeeded. | Validated against AKS (the original 9/9 PASS run). |
| `local-k8s`| Invokes `kars dev --target local-k8s --once`; creates a kind cluster + installs the chart if missing. | Scaffolded; not end-to-end-validated yet. **kindnetd does not enforce NetworkPolicy** — enable Cilium toggle for NP parity. |
| `docker`   | Invokes `kars dev --target docker --once`. No CRDs, no K8s API, no NetworkPolicy. | Scaffolded; not end-to-end-validated yet. Scenarios needing CRDs must ship a `manifests-docker/` overlay. |

The harness is honest about what each platform enforces. See
`platform-notes.txt` in any run's output dir for the layer-by-layer
caveat list specific to that run.

## Adding a scenario

A scenario is a directory under `scenarios/`. The required interface is:

| File                 | Purpose                                                                                  |
|----------------------|------------------------------------------------------------------------------------------|
| `prompt.txt`         | Verbatim user prompt the parent agent receives.                                          |
| `config.sh`          | Bash knobs: `SCENARIO_SANDBOX`, `SCENARIO_SUB_SANDBOXES=(…)`, optional grep patterns, optional `SCENARIO_INCOMING_SANDBOX` / `SCENARIO_INCOMING_PATH`. |
| `checks.py`          | Defines `get_checks() -> list[(label, callable)]`. Each callable takes a `verify.Context` and returns `(bool, message)`. |
| `manifests/`         | (K8s platforms) CRD YAML applied in lexical order via `kubectl apply -f`.                |
| `manifests-docker/`  | (Optional) shell snippets sourced by `platforms/docker.sh` for docker-specific setup.    |

`verify.Context` exposes:

```python
@dataclass
class Context:
    out_dir: Path
    scenario: str
    trace: list[dict]          # parsed trace.jsonl
    transcript: str            # transcript.log contents
    router_lines: list[str]    # router events from trace
    relay_lines: list[str]     # relay events from trace
    extras: dict[str, str]     # per-sub gateway logs, incoming.txt, …

    def lines_for(self, src: str) -> list[str]: ...
```

A check can therefore inspect any artifact the harness collected. The
exec-brief scenario uses 9 checks ranging from "trace has at least one
content-filter pass" to "viz's gateway log shows a successful
foundry_image_generation MCP tools/call".

## Adding a platform

A platform is a `.sh` file under `platforms/`. It must define (or
inherit from another helper) these functions:

| Function                       | Responsibility                                                                |
|--------------------------------|-------------------------------------------------------------------------------|
| `platform_preflight`           | Validate prerequisites; bring up cluster/container if missing.                |
| `platform_apply`               | Apply scenario manifests (or no-op for non-K8s platforms).                    |
| `platform_credentials`         | Wire scenario credentials (e.g. Telegram bot token) into the runtime.         |
| `platform_wait_for_sandbox`    | Block until the parent sandbox is reachable.                                  |
| `platform_post_prompt`         | POST the prompt to the gateway; write `transcript.log`.                       |
| `platform_collect_artifacts`   | Pull per-sub gateway logs and any `SCENARIO_INCOMING_*` listings.             |

`drive.sh` validates these are defined before calling any of them.

## What the harness does and does not test

The harness drives a real prompt through real components and verifies
real artifacts. It does NOT:

- Provision the cluster's Azure-side dependencies (Foundry project,
  Workload Identity, role assignments). `kars up` does that.
- Substitute for unit tests — checks are end-to-end and intentionally
  coarse-grained.
- Substitute for the `cargo test` suite or the CLI vitest suite.

For a layer-by-layer security walkthrough that this harness underpins,
see [`docs/use-cases/exec-brief-walkthrough.md`](../../docs/use-cases/exec-brief-walkthrough.md).
