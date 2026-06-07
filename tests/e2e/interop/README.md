<!--
Copyright (c) Microsoft Corporation.
Licensed under the MIT License.
-->

# Cross-runtime interop harnesses

End-to-end harnesses that prove kars runtimes (OpenClaw, Hermes, …)
cross-talk correctly over the AGT mesh on a real cluster. They
complement the controller-reconcile coverage in `tests/e2e/run.sh`
(which only asserts that a `KarsSandbox` CR of a given `runtime.kind`
gets reconciled into a namespace + Deployment with the expected
runtime image), by exercising the **data plane**: live KNOCK +
X3DH + Double Ratchet + Signal Protocol session establishment
between two real, running agent pods, with real LLM completions
driving the request side.

| Script | Cluster | Sandboxes needed | External services required |
|---|---|---|---|
| `hermes_openclaw_bidi.sh`    | kind or AKS | `kars-execbrief-hermes-multi` (Hermes) + `kars-mesh-peer-openclaw` (OpenClaw) | Azure AI Foundry inference (OpenClaw's `/v1/chat/completions` is the trigger that drives initAGT and the mesh send) |
| `aks_full_suite.sh`          | AKS         | `execbrief`, `analyst`, `viz`, `writer`, `aks-hermes-bidi` | Azure AI Foundry; per-sandbox Entra Agent App RBAC grants on Foundry |
| `aks_hermes_hermes_bidi.sh`  | AKS         | `aks-hermes-bidi`, `aks-hermes-bidi-2`, `aks-mesh-peer-openclaw` | Azure AI Foundry |

## Why these don't run on every PR

Each harness depends on at least one external service that the
public CI environment has no credentials for:

- **Azure AI Foundry** — the OpenClaw `/v1/chat/completions` call
  is what triggers `initAGT()` in the plugin, which in turn opens
  the X3DH session against the relay. Without a real Foundry
  inference policy bound to the sandbox the harness can't generate
  the side effects it asserts on.
- **Azure Kubernetes Service** — the `aks_*` scripts assume a
  pre-deployed kars cluster with the right Entra Workload Identity
  federation already wired (per-sandbox Entra App, RBAC on Foundry,
  agentmesh relay + registry in the `agentmesh` namespace).

Because of that, the PR-time CI gate runs the unit-level coverage
that **doesn't** need external services:

- `Hermes Runtime (Python) Build & Test` — 140 pytest cases on
  `runtimes/hermes` (mesh worker, governance, handoff, peer roster,
  file transfer, foundry native tools, telemetry).
- `kars-agt-mesh (Python AGT mesh transport) Test` — 27 pytest
  cases on `runtimes/agt-mesh-python` including
  **`test_wire_format.py`** which gates the cross-runtime mesh
  contract by asserting the Python implementation produces
  byte-identical establishment frames + message frames + base64
  envelopes to the upstream TypeScript AGT SDK. **This is the
  in-CI cross-runtime proof** — the bash harnesses below are
  live-cluster proofs that complement it, not duplicate it,
  because AGT operates on opaque byte buffers and the wire format
  is the only thing that has to match between runtimes.
- `E2E (Kind)` — applies a `KarsSandbox` of `runtime.kind: Hermes`
  and asserts the controller reconciles it into a namespace +
  Deployment whose `agent` container image references the hermes
  runtime tag. This catches reconciler-side breakage (planner
  dispatch, image selection, env injection) without needing any
  external services.

## How to run these harnesses locally

### Prerequisites (all harnesses)

1. A reachable Kubernetes cluster (`kubectl` configured) with kars
   deployed via the helm chart in `deploy/helm/kars`.
2. The `agentmesh` namespace populated with `agentmesh-relay` and
   `agentmesh-registry` Deployments (the helm chart includes them).
3. The sandboxes referenced in the table above already applied as
   `KarsSandbox` CRs and reconciled (pods Ready).
4. An `InferencePolicy` bound to each sandbox that resolves to a
   real Foundry endpoint with valid Workload-Identity-federated
   RBAC. The controller writes the Entra federated credential, but
   the operator must one-time grant `Cognitive Services OpenAI User`
   on the Foundry account to the per-sandbox Entra App.

### Then

```bash
# Cross-runtime (Hermes ↔ OpenClaw), works on kind or AKS:
bash tests/e2e/interop/hermes_openclaw_bidi.sh

# AKS-only:
bash tests/e2e/interop/aks_full_suite.sh
bash tests/e2e/interop/aks_hermes_hermes_bidi.sh
```

All three are unattended (exit 0 = all green, 1 = any assertion
failed) and print colourised `✓ / ✗` lines as they go. They are
idempotent: they clean up port-forwards on exit and don't mutate
the cluster except for the harness's own probe traffic.

### Useful env overrides

`hermes_openclaw_bidi.sh`:

| Env | Default |
|---|---|
| `HERMES_NS`            | `kars-execbrief-hermes-multi` |
| `HERMES_DEPLOY`        | `execbrief-hermes-multi` |
| `OPENCLAW_NS`          | `kars-mesh-peer-openclaw` |
| `OPENCLAW_DEPLOY`      | `mesh-peer-openclaw` |
| `REGISTRY_NS`          | `agentmesh` |
| `HERMES_ROUTER_PORT`   | `29443` (local port for forwarding to the inference router) |
| `OPENCLAW_GATEWAY_PORT`| `29789` (local port for forwarding to the OpenClaw gateway) |

`aks_full_suite.sh` and `aks_hermes_hermes_bidi.sh`:

| Env | Default |
|---|---|
| `KARS_AKS_CONTEXT` | `kars-aks` |
| `NS_*`             | per-suite namespace overrides (see script source for the exact list) |

## When to extend these

- Adding a new runtime: copy `hermes_openclaw_bidi.sh` and adapt
  the namespace/pod selectors to your runtime's sandbox naming
  convention. The cross-runtime mesh proof itself (KNOCK + X3DH +
  Double Ratchet) is identical because AGT operates on opaque
  byte buffers — the runtime that produces the bytes doesn't
  matter to the wire format.
- Adding a new sandbox to the AKS matrix: extend `aks_full_suite.sh`
  in the same shape as the existing scenarios and add the namespace
  to the cleanup list at the top of the script.
