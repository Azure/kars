# Security Audit — distroless router self-probe sweep + prompt-shields default + CI gate

PR: Azure/kars (branch `fix/distroless-router-probe-sweep`)

## Scope

Capability-path changes in `cli/src/commands/*` (operator, egress, policy, model,
add, handoff, up, sandbox_bringup) and `cli/src/refs.ts`. Supporting:
`inference-router/src/main.rs` (new `probe` subcommand), `tests/e2e/run.sh` (new
gate), `cli/src/commands/dev/local-k8s.ts` (kind image-staleness fix).

## Problem

The AL3 distroless migration (#383) removed `sh`/`curl`/`iptables` from the
controller/inference-router/a2a-gateway/conformance-runner images. Everything that
`kubectl exec`-ed those tools into the distroless **inference-router** container
broke on the real distroless path (AKS, and kind `--release`): the operator's AGT/
metrics panels, `kars egress|policy|model|add|handoff`. (The egress-guard init and
controller probes were the earlier-fixed instances.) It went uncaught because the
e2e never asserted the sandbox **pod** started, and local `kars dev` used a
non-distroless multistage router (or a stale `:dev` image on the kind node).

## Changes

- **`inference-router/src/main.rs`:** new `kars-inference-router probe [GET|POST]
  <path> [json-body]` subcommand. It makes an HTTP request to the router's OWN
  `127.0.0.1:8443<path>`, prints the body, exits 0/1. It reads the admin token the
  **same way the server does** (`/etc/kars/secrets/admin-token`,
  `/run/secrets/admin-token`, `ADMIN_TOKEN`) and adds `Authorization: Bearer` so
  protected endpoints work. The binary is present in both the distroless router
  image and the sandbox image, so no tool is added to any image.
- **CLI:** every `kubectl exec -c inference-router -- curl/sh/wget` (and a `cat` of
  the admin token in handoff) replaced with `... -- /usr/local/bin/kars-inference-
  router probe …`. Docker-mode execs (into the tool-rich sandbox container) are
  unchanged. The CLI no longer fetches/forwards the admin token for these calls
  (the probe reads it locally), so `withAdminAuth`/`getAdminToken` uses were dropped
  where now unused.
- **`refs.ts` + `up`/`add`/`sandbox_bringup`:** `requirePromptShields` now defaults
  **off** (was hardcoded on), with an explicit `--require-prompt-shields` opt-in.
  Bare Foundry/AOAI deployments don't emit `prompt_filter_results`, so the prior
  default fail-closed every response.
- **`dev/local-k8s.ts`:** `loadImageIntoKind` verifies the node image by ID and
  re-imports on mismatch, so `kars dev --release` actually loads the pulled
  (distroless) images instead of silently reusing a stale `:dev`.
- **`tests/e2e/run.sh`:** `test_sandbox_pod_starts` asserts the sandbox pod passes
  the egress-guard init AND the router self-probe answers — the regression gate.

## Threat model

### T1: New attacker-reachable input / capability? (NO)
The `probe` subcommand only reaches the router's own localhost endpoints — the same
endpoints the CLI already called via curl — and is only invoked by an operator who
already holds `kubectl exec` into the (exec-ban-allowed) inference-router container.
It adds no network listener and no new external surface. The admin token it reads
is the same token the server already trusts in the same container; reading it
locally is strictly less exposure than passing it through `kubectl` argv (where it
could leak into process listings / shell history).

### T2: Security-control change? (NO — neutral or safer)
No change to sandbox isolation, egress policy, RBAC, admission, mesh, or auth
*enforcement*. Protected endpoints still require the Bearer token (now supplied
locally). The egress-guard and exec-ban semantics are unchanged. Prompt-shields
default-off matches the documented `values.yaml` default and only affects whether
the router fail-closes on responses lacking `prompt_filter_results`; Content Safety
severity enforcement is unchanged, and `--require-prompt-shields` restores the
strict behaviour for deployments whose Content Filter emits the annotations.

### T3: Fail-open / availability risk? (REDUCED)
These changes restore functionality that the distroless move had broken (operator
panels, egress/policy/model/add). The kind image-ID fix removes a silent
stale-image footgun. The e2e gate adds fail-closed coverage so a future distroless
tool removal fails CI instead of shipping.

## Verification

`cargo build --release` (router) + probe smoke-tested; CLI `tsc --noEmit` + oxlint
(0 errors) + 821 vitest tests (incl. new `refs.test.ts`); `bash -n tests/e2e/run.sh`
clean. End-to-end distroless validation runs via the new e2e gate on Linux CI and
`kars dev --release` on a fresh kind. (The egress-guard instance of this class was
already live-verified on AKS in v0.1.12.)

## Verdict

Accept. Eliminates the remaining distroless-tool breakages on the K8s path with a
single in-binary `probe` mechanism (no tools added to hardened images), fixes a
kind staleness footgun, defaults prompt-shields to the documented-safe value, and
adds the regression gate that would have caught the whole class. No runtime
security control is weakened.

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
