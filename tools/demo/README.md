# `tools/demo/` — scripted end-to-end walkthrough

A single shell script that exercises the full Kars stack across the
three substrates we support: local Docker (`kars dev`), local Kind
(`kars up` flow exercised through the E2E infra helpers), and a
real AKS cluster (`kars up`).

The narrative companion is **[`docs/demo-script.md`](../../docs/demo-script.md)** —
that doc is what a human reads/delivers; this directory is what
automation runs.

## What it does

Four scenarios, in order, shared across all three modes:

| Step | Resource | Demonstrates |
|------|----------|--------------|
| 1    | `InferencePolicy` + `KarsSandbox`            | Sandbox brought up Ready (router echo); zero credentials inside the agent. |
| 2    | `ToolPolicy` (web.fetch → approval-required) | Tool gating via a separate CRD; rate-limit + Telegram approval channel surfaced in the operator TUI. |
| 3    | `EgressApproval` (api.stripe.com, PT10M)     | Time-boxed widening of the egress allowlist; phase transitions Pending → Active observable. |
| 4    | `KarsEval` (run-now, jailbreak-baseline)     | Reproducible conformance run via a Job; status.history append + `kars eval show`. |

In dev mode, only step 1 runs (there is no K8s control plane to apply
the rest against).

## Running it

```bash
# Local Docker
bash tools/demo/run-demo.sh --mode dev

# Local Kind (bring the cluster up first)
bash tests/e2e/infra-e2e.sh up
bash tools/demo/run-demo.sh --mode kind

# Real AKS (you must already be context-switched into the cluster
# that `kars up` created)
bash tools/demo/run-demo.sh --mode aks
```

Useful flags:

- `--skip-cleanup` — leave resources behind for inspection.
- `--timeout 300` — extend per-step wait (default 180s; AKS pulls can be slow on cold caches).

## What it does **not** do

Per `principles.md §6` (dev-only landing) and the project's no-magic
posture:

- **It never calls `az`.** Bring your own auth (`az login` for AKS,
  `kars login` for dev / kind).
- **It never creates clusters.** Use `kars up` for AKS and
  `tests/e2e/infra-e2e.sh up` for Kind.
- **It does not seed Foundry resources.** Memory Store, Bing Grounding,
  and Content Safety provisioning are operator concerns and out of
  scope (see [the CRD reference](../../docs/api/crd-reference.md#karsmemory--memory-store-binding)
  for the project-MI gotcha).
- **It does not exercise A2A / mesh.** Inter-agent demos belong in
  `examples/demo-clawshield/` — that scenario has dedicated assets.

## How automation-friendly is it?

Exit codes are stable: `0` ok, `1` missing dependency, `2` scenario
failure, `3` cleanup failure, `4` unsupported mode. The script is
`set -euo pipefail` clean and `bash -n` lint-clean (CI runs both).
Wire it into a nightly job by running `--mode kind` against a fresh
infra-e2e cluster.

## Scenarios as documentation

The four YAML files in `scenarios/` are intentionally **minimal but
schema-accurate** — they round-trip against the live CRDs in
`deploy/helm/kars/templates/crd-*.yaml`. If a CRD field is
renamed, this script's `kubectl apply` will fail loudly. Treat the
scenarios as a smoke test for CRD-shape regressions in addition to a
demo aid.
