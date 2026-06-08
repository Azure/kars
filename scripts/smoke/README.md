<!--
Copyright (c) Microsoft Corporation.
Licensed under the MIT License.
-->

# kars OOTB smoke gate

`fresh-machine-ootb.sh` reproduces a brand-new-contributor experience
end-to-end so we can verify "out of the box" actually means what we say
it means.

## Why this exists

The Hermes-support PR shipped 16 OOTB blockers in one debugging session
that no unit test caught — every single one was a wire-format failure
across module boundaries that only surfaces when the full flow runs
against a real cluster. Filed in
[`ootb-fresh-machine-gate`][1] as the regression-protection gap; this
script is the concrete implementation.

[1]: ../../docs/blueprints/07-kars-sre-proposal.md#validation-gate

## What it does

1. Wipes every piece of carried state (kind cluster, AGT clone,
   `~/.kars`, npm-link)
2. Fresh git clone of the current branch HEAD into `/tmp/kars-ootb-smoke`
3. `cd cli && npm ci && npm run build && npm link`
4. `kars dev --target local-k8s` (non-interactive — needs a Copilot
   token in `$KARS_OOTB_COPILOT_TOKEN`)
5. `kars add` one OpenClaw + one Hermes sandbox
6. Polls until both pods reach Running 2/2 (default 5-min timeout)
7. Tears down (or `--keep`)

Exits 0 iff every step succeeds. On failure prints the precise command
that failed AND the diagnostic the operator would see, so the
regression is reproducible from the script output alone.

## Usage

```bash
# Default — wipe everything, full walkthrough, tear down on success:
export KARS_OOTB_COPILOT_TOKEN="gho_..."   # GitHub OAuth token with Copilot scope
bash scripts/smoke/fresh-machine-ootb.sh

# Reuse existing state (skip the wipe — faster, less authoritative):
bash scripts/smoke/fresh-machine-ootb.sh --no-wipe

# Don't tear down on success (leave cluster up for manual inspection):
bash scripts/smoke/fresh-machine-ootb.sh --keep

# Test a specific branch (default: whatever is checked out in /tmp/kars-ootb-smoke):
bash scripts/smoke/fresh-machine-ootb.sh --branch hermes/act1-docker-smoke-fixes
```

## When to run

- Before merging any PR that touches:
  `cli/src/commands/dev/`, `controller/src/reconciler/`,
  `deploy/helm/`, `sandbox-images/`, `runtimes/`,
  `inference-router/src/spawn/`, `inference-router/src/forward_proxy.rs`
- Before cutting a release
- After any cargo-deny / advisory feed roll that touches Rust deps
- As a CI lane on a docker-enabled GHA runner (todo
  `ootb-fresh-machine-gate` — needs runner provisioning)

## What it catches that unit tests don't

Every blocker in the 2026-06-08 Hermes-support session would have been
caught by this script. Examples:
- AGT auto-clone missing — kars dev fails before chart applies
- Stale CRD — kars add bundle rejected with ValidationError
- Stale controller image — pod ImagePullBackOff
- KARS_DEV_PROFILE not in static overlay — same
- Hermes runtime image not loaded into kind — same
- `cp -a` perm error in hermes entrypoint — pod CrashLoopBackOff
- Hermes ignored picked model — verifiable post-spawn (next iteration)
- SpawnRequest missing `role` — `kars add` exits non-zero

## Limitations (today)

- Needs a real Copilot OAuth token. Can't run on a public hosted CI
  runner without secret wiring.
- Single-cluster — doesn't cover AKS, federation, etc. (separate
  scripts: `tests/e2e/interop/aks_*.sh`)
- Doesn't yet validate model routing (post-spawn: send a chat
  completion, assert the model echoed back matches what was picked).
  Tracked as a follow-up extension to this script.

## When to extend

Adding a new runtime to `WIRED_KINDS`: append a `run_kars_add foo
--runtime foo` call between the existing OpenClaw + Hermes pair so
every wired runtime is exercised on every smoke run.
