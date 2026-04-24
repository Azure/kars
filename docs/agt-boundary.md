# AGT Boundary — what AzureClaw consumes vs. what AzureClaw builds

**Status:** internal draft, shared with AGT team for confirmation.
**Companion:** `docs/implementation-plan.md` §1 ("AGT boundary (operational,
not rhetorical)") is the canonical version; this file is the standalone
reference that non-plan readers need.

AGT ships the governance engine. AzureClaw is the AKS operator and data plane
that feeds AGT and surfaces AGT decisions as K8s primitives. Any overlap is a
bug to resolve, not a feature to negotiate.

---

## 1. Responsibility split

### AGT owns

- **Policy evaluation** — `PolicyEngine.decide(request) -> verdict`. Policy
  profile schema is AGT's. We emit profiles from CRDs; we do not redefine the
  schema.
- **Signal Protocol primitives** — X3DH key exchange, Double Ratchet, prekey
  lifecycle, session state machine.
- **Audit Merkle chain** — `AuditLogger.append(event) -> ReceiptId`. Storage,
  retention SLA, signing of tree roots, queryability API.
- **Trust scoring** — `TrustManager`. Per-peer trust scores, transitive
  evaluation, decay functions, negative-signal ingestion.
- **Behavior anomaly detection** — `BehaviorMonitor`. Baseline capture,
  deviation detection, Shadow-MCP behavioral signals.
- **Rate-limit token bucket** — per-identity, per-tool, per-mesh counters.
  We configure caps; AGT enforces.
- **Signing keys** — HSM / HW-backed key custody, key rotation, signing
  primitives for A2A cards and AP2 transfers.
- **A2A 1.2 Signed Agent Card signing** — when AGT ships this primitive. If
  not, we implement via our `SigningProvider` and document the gap.
- **AP2 policy grammar** — if AGT defines; otherwise we define an internal
  schema designed to be portable to a future AGT definition.

### AzureClaw owns

- **K8s operator** — controller, CRDs, admission policies, reconcilers.
- **Router data plane** — L7 proxy, IMDS/Workload-Identity auth, Foundry
  calls, MCP transport (Streamable HTTP + SSE compat), A2A transport,
  OpenAI-SDK sandbox-provider adapter, channel plugins.
- **Sandbox image** — Dockerfile layout, seccomp profiles, Landlock policy,
  egress-guard iptables, UID layout, init containers.
- **CLI (`azureclaw`)** — including `operator` TUI, `up`, `add`, `push`,
  `dev`, `handoff`, `offload`, `policy learn`, `migrate`, `convert`,
  `claw attest`.
- **Confidential compute integration** — Kata + SEV-SNP runtime class,
  attestation document handling.
- **K8s AI Conformance** — internal certification harness; public certification
  deferred to post-OSS.
- **`sigs/agent-sandbox` compat mode** — translator / overlay / vendored
  reconciler for the upstream schema. Opt-in; default stays Native.

---

## 2. Provider contracts (Phase 0)

Four provider traits, each with three impls (`Vendored*`, `Agt*`, `Null*`).
`Null*` is test-only and blocked in prod by admission (see
`docs/implementation-plan.md` §6 item 6).

| Trait | Current impls | Role |
|---|---|---|
| `MeshProvider` | `VendoredAgentMesh` · `Agt` (pending AGT AgentMesh delivery) | E2E session establishment + message send/receive |
| `PolicyDecisionProvider` | `Vendored` · `AgtRustSdk` · `Null` | Allow/Deny/Approval/RateLimit evaluation |
| `AuditSink` | `Vendored` · `AgtRustSdk` · `Null` | Append-only audit events → receipt id |
| `SigningProvider` | `Vendored` · `AgtRustSdk` · `Null` | Sign (key_ref, payload) and verify |

Providers are selected **per tenant** via feature flag. The vendored path is
permanent alternate architecture, not migration staging — it is never
scheduled for deletion.

## 3. Outage semantics

Every provider call passes through an outage-mode layer:

| Mode | Behaviour on provider outage | Default |
|---|---|---|
| `Strict` | Fail-closed. Refuse the operation. | **Prod (default)** |
| `CachedRead` | Use last-known decision for read-only paths; fail-close on mutation. | opt-in, regulated tenants |
| `DegradedDev` | Allow, mark event `degraded=true`, emit loud metric. | `azureclaw dev` |

Per-tenant override via `ClawSandbox.spec.agt.outageMode`.

## 4. What we never build

- Signal/X3DH/Double-Ratchet primitives (principle §0.2 #8: no custom crypto).
- A standalone audit chain (we emit into AGT's).
- A trust-score computation engine.
- A rate-limit enforcement bucket outside AGT.
- Key custody or HSM integration outside AGT.

## 5. What we always build

- The K8s-primitive surface (CRDs) for anything AGT exposes.
- The router data-plane enforcement point for every AGT decision.
- The sandbox isolation substrate AGT assumes.

## 6. Working mode with AGT

- This file is shared with the AGT team for confirmation (Phase 0
  deliverable).
- Disagreements resolved by: (a) AGT's ownership wins if they commit to
  shipping; (b) AzureClaw picks it up temporarily if they cannot commit;
  (c) scope is documented here and in the relevant security-audit docs.
- AGT Rust SDK releases trigger `ci/vendored-patch-audit.sh` re-runs (see
  `docs/agt-vendored-patch-audit.md`).
