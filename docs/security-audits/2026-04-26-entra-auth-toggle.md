# Entra Agent ID auth toggle (`AGT_SKIP_ENTRA`)

**Date:** 2026-04-26
**Component:** `sandbox-images/openclaw/entrypoint.sh`, `controller/src/reconciler/mod.rs`, `deploy/helm/azureclaw/`

## Summary

Add an operator-level kill switch (`AZURECLAW_DISABLE_ENTRA_AUTH` on the
controller, `AGT_SKIP_ENTRA` on the sandbox) that short-circuits the
Workload-Identity → Entra token-exchange step in the sandbox entrypoint.
When the switch is on, the sandbox immediately registers with the AGT
registry as **anonymous tier** instead of burning ~123 seconds on
retries that cannot succeed.

The default for new deployments is **skip**, because the
`api://agentmesh` Entra app registration is *not yet provisioned* in any
of our subscriptions. Flipping back to `enabled: true` is the explicit
opt-in once Entra Agent ID provisioning has been wired end-to-end.

## Motivation

Empirical AKS observation (2026-04-26 dev cluster):

```
[entrypoint] Exchanging Workload Identity token for Entra ID access token...
[entrypoint] Entra token exchange failed after 123s (32 attempts) — agent will register as anonymous tier
```

Two distinct error paths:

1. **Parent agents** hit `AADSTS500011` ("the app `api://agentmesh` was
   not found in the tenant") and short-circuit at line 156 of
   `entrypoint.sh` after a single attempt.
2. **Sub-agents** receive a *different* response (empty body, IMDS
   timeout, or `AADSTS90002`-class errors caused by the per-pod KSA not
   being federated) — no short-circuit, full 32-retry × 4 s-cap loop.

The 123-second sub-agent boot delay is longer than the parent's tool-call
timeout. Result: spawn-and-message workflows fail because the parent's
`azureclaw_mesh_send` returns "no registry match" before the sub-agent
has even reached the OpenClaw plugin init step.

## Threat model

This is a **fail-open toggle** (anonymous tier) and is therefore worth
explicit threat-model documentation.

| Threat | Anonymous tier | Verified tier |
|---|---|---|
| KNOCK from unverified peer | Accepted iff trust ≥ `AGT_TRUST_THRESHOLD` (default 500); the registry's reputation score still applies | Accepted iff trust ≥ threshold; same gate, plus signed Entra `iss`/`aud` claim |
| Replay/forgery of identity | Identity is still bound to per-sandbox Ed25519 keypair generated at boot (libsodium); KNOCK still uses Signal X3DH | Same Ed25519 binding + Entra-signed proof of tenant membership |
| Cross-tenant impersonation | Possible iff attacker can register an AMID with a stolen `display_name` *and* the registry's reputation score reaches threshold | Blocked at registry by tenant-scoped `iss` validation |
| Confidentiality of mesh messages | Preserved (Double Ratchet) — independent of tier | Preserved |

The toggle does **not** weaken Signal-Protocol message encryption, key
custody, or seccomp/Landlock isolation. It only changes the registration
tier presented to the AGT registry.

## Operational risk

Acceptable for the current threat model because:

* The cluster-wide `agentmesh-relay` and `agentmesh-registry`
  endpoints are reachable only from inside the cluster (Service IPs,
  no Ingress).
* Inter-cluster mesh federation (when enabled via `meshPeer`) gates
  cross-cluster traffic with mTLS at the Cilium gateway, independently
  of AGT tier.
* Anonymous-tier registration is observable in the registry — operators
  can audit it via `kubectl exec deploy/registry -- curl ... /v1/registry/search`.

The toggle is also fully reversible: setting `azure.entraAuth.enabled: true`
in `values.yaml` and rolling out the controller flips behaviour back to
the original retry-with-fallback flow, no data migration needed.

## Code-path summary

* `sandbox-images/openclaw/entrypoint.sh` — guard the existing Entra
  block with `if [ "${AGT_SKIP_ENTRA:-0}" = "1" ]; then ...elif ...`.
  No retries are attempted in skip mode; one log line is emitted for
  observability.
* `controller/src/reconciler/mod.rs` — read the
  `AZURECLAW_DISABLE_ENTRA_AUTH` env var on the controller (defaulting
  to `1`), and inject `AGT_SKIP_ENTRA=1` into the sandbox's openclaw
  container env when the toggle is on.
* `deploy/helm/azureclaw/values.yaml` — add `azure.entraAuth.enabled`
  (default `false`).
* `deploy/helm/azureclaw/templates/controller-deployment.yaml` — wire
  the value through to `AZURECLAW_DISABLE_ENTRA_AUTH` on the controller
  pod.

## Phase 2 follow-up

Replace the operator-level boolean with controller-side **tenant
feature detection**: at startup, the controller can probe Entra for the
`api://agentmesh` SP using its own Workload Identity. If the SP is not
present, behave as if the toggle were on (and emit a warning event on
the `ClawSandbox` CRD). If present, behave as if the toggle were off.
This eliminates the manual flip step.

Tracked alongside the Entra Agent ID provisioner work on branch
`phase1/identity-provider-seam-entra-agent-id`.

## Sign-off

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
