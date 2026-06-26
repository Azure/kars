# AGT Boundary — what kars consumes vs. what kars builds

> Defines the operational seam between [Microsoft AGT](https://github.com/microsoft/agent-governance-toolkit) and kars: what kars imports, what it builds in-tree, and the four provider contracts that keep them aligned.

AGT ships the governance engine. kars is the AKS operator and data plane that feeds AGT and surfaces AGT decisions as Kubernetes primitives. Any overlap is treated as a bug to resolve, not a feature to negotiate.

---

## 1. Responsibility split

### AGT owns

- **Policy evaluation** — `PolicyEngine.decide(request) -> verdict`. Policy-profile schema is AGT's. We emit profiles from CRDs; we do not redefine the schema.
- **Signal Protocol primitives** — X3DH key exchange, Double Ratchet, prekey lifecycle, session state machine.
- **Mesh-relay + mesh-registry** — including **opt-in Entra-signed JWT verification on the connect frame** (audience + tenant + issuer enforcement), **per-agent session counters** + `completion_rate` reputation surface, and the `POST /v1/registry/verify` endpoint. Upstreamed by kars in [microsoft/agent-governance-toolkit#2719](https://github.com/microsoft/agent-governance-toolkit/pull/2719); kars now consumes this as a regular AGT dependency rather than a vendored fork.
- **Audit chain** — `AuditLogger.append(event) -> ReceiptId`. Storage, retention SLA, queryability API. The runtime uses AGT's linear SHA-256 hash chain today; Merkle anchoring and signed roots are a planned extension (the library exists in `inference-router/src/audit/merkle.rs` but is not yet wired into the live pipeline).
- **Trust scoring** — `TrustManager`. Per-peer trust scores, transitive evaluation, decay functions, negative-signal ingestion.
- **Behavior anomaly detection** — `BehaviorMonitor`. Baseline capture, deviation detection, Shadow-MCP behavioral signals.
- **Rate-limit token bucket** — per-identity, per-tool, per-mesh counters. We configure caps; AGT enforces.
- **Signing keys** — HSM / HW-backed key custody, key rotation, signing primitives for A2A cards and AP2 transfers.
- **AP2 policy grammar** — if AGT defines it. Until then kars defines a private schema designed to be portable to a future AGT definition.

### kars owns

- **Kubernetes operator** — controller, CRDs, admission policies, reconcilers.
- **Router data plane** — L7 proxy, IMDS / Workload-Identity auth, Foundry calls, MCP transport (Streamable HTTP + SSE compat), A2A transport, OpenAI-SDK sandbox-provider adapter, channel plugins.
- **Sandbox image** — Dockerfile layout, seccomp profiles, Landlock policy, egress-guard iptables, UID layout, init containers.
- **CLI (`kars`)** — including `operator` TUI, `up`, `upgrade`, `add`, `push`, `dev`, `handoff`, `policy`, `migrate`, `convert`, `attest`, `sre`, `mesh`, `pair`.
- **Confidential-compute integration** — Kata + SEV-SNP runtime class, attestation-document handling.

---

## 2. Provider contracts

kars exposes four provider traits. Three of them (`PolicyDecisionProvider`, `AuditSink`, `SigningProvider`) ship a vendored implementation alongside the AGT-Rust-SDK path. The fourth (`MeshProvider`) is **plugin-side only**: the router is a transport proxy for mesh traffic, holds no Signal keys, and intentionally provides **no** Rust mesh implementation — E2E encryption happens in the agent (see [The mesh](../architecture.md#the-mesh)).

| Trait | Where it's implemented | Role |
|---|---|---|
| `MeshProvider` | **Agent-side only** — OpenClaw via the AGT TypeScript SDK, Hermes via the AGT Python mesh client. The router has **no** mesh `impl` (it proxies opaque ciphertext). | E2E session establishment + message send/receive |
| `PolicyDecisionProvider` | `Vendored` · `AgtRustSdk` · `Null` | Allow / Deny / Approval / RateLimit evaluation |
| `AuditSink` | `Vendored` · `AgtRustSdk` · `Null` | Append-only audit events → receipt id |
| `SigningProvider` | `Vendored` · `AgtRustSdk` · `Null` | Sign `(key_ref, payload)` and verify |

`Null*` is test-only and blocked in production by admission policy. The router **does** link the [`agentmesh`](https://crates.io/crates/agentmesh) Rust crate (`agentmesh = "4.0.0"`, the crates.io floor, temporarily redirected to a pinned `microsoft/agent-governance-toolkit` revision via `[patch.crates-io]` while two pre-release fixes are in review) — but only for the shared **governance** primitives (`AuditLogger`, `PolicyEngine`, `TrustManager`), never for mesh crypto. On the TypeScript side, the OpenClaw plugin bundles `@microsoft/agent-governance-sdk` at sandbox-image build time (currently from the same pinned AGT build, switching to the published npm release once those fixes land); the Python runtimes use the AGT Python mesh client (`kars-agt-mesh` / `a2a_agentmesh`). There is **no in-tree fork** of either SDK — every client is an upstream AGT build, and the historical `vendor/agentmesh-*` overlay has been retired.

The remaining vendored paths (`PolicyDecisionProvider`, `AuditSink`, `SigningProvider`) are a **permanent alternate architecture**, not migration staging — they are never scheduled for deletion.

## 3. Outage semantics

Every provider call passes through an outage-mode layer:

| Mode | Behaviour on provider outage | Default |
|---|---|---|
| `Strict` | Fail-closed. Refuse the operation. | **Production (default)** |
| `CachedRead` | Use last-known decision for read-only paths; fail-close on mutation. | Opt-in, regulated tenants |
| `DegradedDev` | Allow, mark event `degraded=true`, emit loud metric. | `kars dev` |

Per-tenant override via `KarsSandbox.spec.agt.outageMode`.

## 4. What kars never builds

- Signal / X3DH / Double-Ratchet primitives (no custom crypto).
- A standalone audit chain (we emit into AGT's).
- A trust-score computation engine.
- A rate-limit enforcement bucket outside AGT.
- Key custody or HSM integration outside AGT.

## 5. What kars always builds

- The Kubernetes-primitive surface (CRDs) for anything AGT exposes.
- The router data-plane enforcement point for every AGT decision.
- The sandbox isolation substrate AGT assumes.

## 6. Working mode with AGT

- This file is shared with the AGT team for confirmation as a living seam document.
- Disagreements are resolved by: (a) AGT's ownership wins if they commit to shipping; (b) kars picks it up temporarily if they cannot commit; (c) the scope is documented here and in the relevant security-audit doc.
- AGT Rust SDK releases trigger a manual audit pass to verify behaviour against our `MeshProvider` integration tests.
