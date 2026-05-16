# AGT Boundary — what AzureClaw consumes vs. what AzureClaw builds

> Defines the operational seam between [Microsoft AGT](https://github.com/microsoft/agent-governance-toolkit) and AzureClaw: what AzureClaw imports, what it builds in-tree, and the four provider contracts that keep them aligned.

AGT ships the governance engine. AzureClaw is the AKS operator and data plane that feeds AGT and surfaces AGT decisions as Kubernetes primitives. Any overlap is treated as a bug to resolve, not a feature to negotiate.

---

## 1. Responsibility split

### AGT owns

- **Policy evaluation** — `PolicyEngine.decide(request) -> verdict`. Policy-profile schema is AGT's. We emit profiles from CRDs; we do not redefine the schema.
- **Signal Protocol primitives** — X3DH key exchange, Double Ratchet, prekey lifecycle, session state machine.
- **Audit Merkle chain** — `AuditLogger.append(event) -> ReceiptId`. Storage, retention SLA, signing of tree roots, queryability API.
- **Trust scoring** — `TrustManager`. Per-peer trust scores, transitive evaluation, decay functions, negative-signal ingestion.
- **Behavior anomaly detection** — `BehaviorMonitor`. Baseline capture, deviation detection, Shadow-MCP behavioral signals.
- **Rate-limit token bucket** — per-identity, per-tool, per-mesh counters. We configure caps; AGT enforces.
- **Signing keys** — HSM / HW-backed key custody, key rotation, signing primitives for A2A cards and AP2 transfers.
- **A2A 1.0.0 Signed Agent Card signing** — when AGT ships this primitive. Until then we implement via the `SigningProvider` seam and document the gap.
- **AP2 policy grammar** — if AGT defines it. Until then AzureClaw defines a private schema designed to be portable to a future AGT definition.

### AzureClaw owns

- **Kubernetes operator** — controller, CRDs, admission policies, reconcilers.
- **Router data plane** — L7 proxy, IMDS / Workload-Identity auth, Foundry calls, MCP transport (Streamable HTTP + SSE compat), A2A transport, OpenAI-SDK sandbox-provider adapter, channel plugins.
- **Sandbox image** — Dockerfile layout, seccomp profiles, Landlock policy, egress-guard iptables, UID layout, init containers.
- **CLI (`azureclaw`)** — including `operator` TUI, `up`, `add`, `push`, `dev`, `handoff`, `offload`, `policy learn`, `migrate`, `convert`, `claw attest`.
- **Confidential-compute integration** — Kata + SEV-SNP runtime class, attestation-document handling.
- **`sigs/agent-sandbox` compatibility mode** — translator / overlay / vendored reconciler for the upstream schema. Opt-in; default stays Native.

---

## 2. Provider contracts

AzureClaw exposes four provider traits. Two of them (`PolicyDecisionProvider`, `AuditSink`, `SigningProvider`) ship a vendored implementation alongside the AGT-Rust-SDK path; one (`MeshProvider`) consumes the upstream `@agentmesh/sdk` directly.

| Trait | Current implementations | Role |
|---|---|---|
| `MeshProvider` | upstream `@agentmesh/sdk` (Rust crate `agentmesh = "3.1.0"`) | E2E session establishment + message send/receive |
| `PolicyDecisionProvider` | `Vendored` · `AgtRustSdk` · `Null` | Allow / Deny / Approval / RateLimit evaluation |
| `AuditSink` | `Vendored` · `AgtRustSdk` · `Null` | Append-only audit events → receipt id |
| `SigningProvider` | `Vendored` · `AgtRustSdk` · `Null` | Sign `(key_ref, payload)` and verify |

`Null*` is test-only and blocked in production by admission policy. The Rust-side AgentMesh vendor was retired — `Cargo.toml` now depends on the published `agentmesh = "3.1.0"` crate. A small dist-only patch set against `@agentmesh/sdk` (TypeScript plugin layer) survives in `vendor/agentmesh-sdk/`; patches are itemised in `vendor/agentmesh-sdk/README.md`.

The remaining vendored paths (`PolicyDecisionProvider`, `AuditSink`, `SigningProvider`) are a **permanent alternate architecture**, not migration staging — they are never scheduled for deletion.

## 3. Outage semantics

Every provider call passes through an outage-mode layer:

| Mode | Behaviour on provider outage | Default |
|---|---|---|
| `Strict` | Fail-closed. Refuse the operation. | **Production (default)** |
| `CachedRead` | Use last-known decision for read-only paths; fail-close on mutation. | Opt-in, regulated tenants |
| `DegradedDev` | Allow, mark event `degraded=true`, emit loud metric. | `azureclaw dev` |

Per-tenant override via `ClawSandbox.spec.agt.outageMode`.

## 4. What AzureClaw never builds

- Signal / X3DH / Double-Ratchet primitives (no custom crypto).
- A standalone audit chain (we emit into AGT's).
- A trust-score computation engine.
- A rate-limit enforcement bucket outside AGT.
- Key custody or HSM integration outside AGT.

## 5. What AzureClaw always builds

- The Kubernetes-primitive surface (CRDs) for anything AGT exposes.
- The router data-plane enforcement point for every AGT decision.
- The sandbox isolation substrate AGT assumes.

## 6. Working mode with AGT

- This file is shared with the AGT team for confirmation as a living seam document.
- Disagreements are resolved by: (a) AGT's ownership wins if they commit to shipping; (b) AzureClaw picks it up temporarily if they cannot commit; (c) the scope is documented here and in the relevant security-audit doc.
- AGT Rust SDK releases trigger a manual audit pass against `vendor/agentmesh-sdk/README.md` to verify each listed patch is either upstreamed or still needed.
