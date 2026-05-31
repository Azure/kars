# Feature maturity & enforcement status

kars is `v0.1.0`. Most of the control plane is enforced at runtime today, but some
capabilities are reconciled-but-not-yet-gated, ship as a library that is not yet wired
into the request path, or are still on the [roadmap](roadmap.md). This page is the
**single, honest source of truth** for where each capability sits, so reviewers do not
have to reconstruct it from scattered caveats.

It consolidates the per-layer caveats in **[security.md](security.md)** and the themes in
**[roadmap.md](roadmap.md)**. Where a row says "enforced," it means the runtime fails
closed — not that the field merely exists on a CRD.

## How to read the status column

| Status | Meaning |
|---|---|
| ✅ **Enforced** | The runtime (router, controller, init container, or kernel) actively gates on this today and fails closed. Exercised by CI and/or the [exec-brief walkthrough](use-cases/exec-brief-walkthrough.md). |
| 🟡 **Reconciler-only** | The CRD is validated, reconciled, and its data projected into the cluster, but the corresponding **runtime** enforcement point is not yet wired. The spec is accepted and surfaced in status; it does not yet block. |
| 🔵 **Library-only** | The enforcement logic exists as tested code (a crate function or module) but is not yet called from the live request path. Reachable by embedders; not on by default. |
| ⚪ **Roadmap** | Planned, not built. Tracked in [roadmap.md](roadmap.md). |

> The distinction between 🟡 and ⚪ matters for threat modelling: a 🟡 capability has a
> defined spec and a known wiring gap; a ⚪ capability has neither yet.

## Identity & credential isolation

| Capability | Status | Enforcement point |
|---|---|---|
| No Azure credentials reachable by the agent (Workload Identity / IMDS broker) | ✅ Enforced | Router exchanges the projected SA token; UID 1000 is iptables-blocked from IMDS. [security.md Layer 5](security.md#layer-5--network-segmentation) |
| Per-scope token caching & refresh | ✅ Enforced | `inference-router/src/auth.rs` |
| Per-sandbox Entra Agent ID (typed `microsoft.graph.agentIdentity`) | ✅ Enforced | `--mesh-trust=entra`; [agent-identity.md](agent-identity.md) |

## Network & egress

| Capability | Status | Enforcement point |
|---|---|---|
| iptables UID-based egress guard (agent → localhost + DNS only) | ✅ Enforced | `init: egress-guard` container. [security.md Layer 5](security.md#layer-5--network-segmentation) |
| Kubernetes NetworkPolicy default-deny egress | ✅ Enforced | Controller reconciles from `KarsSandbox` spec |
| Router L7 allowlist on every outbound CONNECT | ✅ Enforced | Inline `allowedEndpoints` is the source of truth today |
| `EgressApproval` time-boxed exceptions | ✅ Enforced | Router consults active `EgressApproval` CRs per request |
| Auto-refreshing malicious-domain blocklist (OISD + URLhaus) | ✅ Enforced | [egress-proxy.md](egress-proxy.md) |
| Signed-OCI allowlist as the **authoritative** source (authority flip) | ⚪ Roadmap | Today the signed artifact is a parallel advisory check; making it the only source of truth is on the [roadmap](roadmap.md#egress-allowlist-authority-flip) |

## Kernel & container hardening

| Capability | Status | Enforcement point |
|---|---|---|
| Read-only rootfs, drop-ALL caps, non-root, no-privilege-escalation | ✅ Enforced | Pod securityContext on every sandbox. [security.md Layer 3](security.md#layer-3--container-hardening) |
| Custom `kars-strict` seccomp profile (blocks `mount`, `ptrace`, `bpf`, `unshare`, …) | ✅ Enforced | DaemonSet-installed; `deploy/helm/kars/files/kars-strict.json` |
| Kata + AMD SEV-SNP confidential isolation (opt-in) | ✅ Enforced | `spec.isolation: confidential`. [security.md Layer 2](security.md#layer-2--pod-isolation-optional-vm) |

## Inference safety

| Capability | Status | Enforcement point |
|---|---|---|
| Content Safety / Prompt Shield (Foundry providers) | ✅ Enforced | Server-side `Microsoft.DefaultV2`; router parses `prompt_filter_results` into AGT `BehaviorMonitor` flags. [security.md Layer 6](security.md#layer-6--inference-safety) |
| Content Safety on GitHub Copilot / GitHub Models paths (provider-side) | ✅ Enforced (provider-side, opaque to router) | Both providers enforce Microsoft Responsible AI content filtering server-side. The router itself does not see per-request `prompt_filter_results` annotations, so AGT `BehaviorMonitor` cannot tune severity or surface category flags on those paths — see [security.md → What we do *not* defend against](security.md#what-we-do-not-defend-against) for the router-side gap. The underlying content filter is on. |
| Per-request token cap (`tokenBudget.perRequestTokens`) | ✅ Enforced | Router, HTTP 429 on overrun |
| Per-tenant daily / monthly UTC token counters | ✅ Enforced | On-disk persistence in the router |
| `InferencePolicy` **aggregate** token budgets (per-hour / per-day windows, `rejectOnExceed`) | 🟡 Reconciler-only | Accepted on spec and surfaced in status; not yet metered at the router. [roadmap](roadmap.md#inference-policy-enforcement) |

## Governance (AGT, in-router)

| Capability | Status | Enforcement point |
|---|---|---|
| `PolicyEngine` — hot-reloaded YAML rules gating exec / fetch / spawn / mesh send | ✅ Enforced | `inference-router/src/governance/mod.rs` |
| `RateLimiter` — global + per-agent token bucket | ✅ Enforced | Same hot path; 429 + audit on overrun |
| `BehaviorMonitor` — burst / failure / denial detection | ✅ Enforced | Emits alerts (does not block) |
| `TrustManager` — Ed25519 identities, 0–1000 score, 5 tiers | ✅ Enforced | Consulted at mesh session establishment (not per-request) |

## Mesh & trust topology

| Capability | Status | Enforcement point |
|---|---|---|
| E2E-encrypted inter-agent messaging (Signal: X3DH + Double Ratchet) | ✅ Enforced | Agent-owned session; router bridges ciphertext only. [security.md Layer 8](security.md#layer-8--end-to-end-encrypted-mesh) |
| KNOCK-gated session establishment against `AGT_TRUST_THRESHOLD` | ✅ Enforced | Plugin-side KNOCK handler |
| Verified-tier registration via `api://agentmesh` Entra token | ✅ Enforced | `kars mesh setup-trust`; anonymous-tier fail-open by design |
| `TrustGraph` **router-side** mesh-admission gating (pre-handshake edge check) | 🟡 Reconciler-only | Graph is projected into the pod and consumed by the agent's KNOCK handler; a coarser router-side admission check is on the [roadmap](roadmap.md#trust-topology-end-to-end) |
| `TrustGraph` dynamic projection (no sandbox restart on topology change) | ⚪ Roadmap | [roadmap](roadmap.md#trust-topology-end-to-end) |

## A2A gateway

| Capability | Status | Enforcement point |
|---|---|---|
| Verified-caller subject from upstream Gateway-API mTLS (`X-A2A-Agent-Subject`) | ✅ Enforced | Gateway binary today. [architecture/a2a-gateway.md](architecture/a2a-gateway.md) |
| In-binary `AgentCard` JWS verification | 🔵 Library-only | `kars_a2a_core::verify_inbound_card` is library-complete and unit-tested; wiring it as an axum layer for non-AGC topologies is on the [roadmap](roadmap.md#a2a-gateway-hardening) |

## Audit & attestation

| Capability | Status | Enforcement point |
|---|---|---|
| SHA-256 hash-chained audit log (tamper **detection**) | ✅ Enforced | `AuditLogger`; any edit breaks the chain on replay |
| Audit-chain head **signing** (non-repudiation) | ⚪ Roadmap | Detection ships today; cryptographic head-signing is on the [roadmap](roadmap.md#backlog-no-timeline) |
| `kars attest sign` / `attest verify` full flow | ⚪ Roadmap | CLI is scaffolded; the full attestation flow is on the [roadmap](roadmap.md) |
| Signed reconcile audit-chain **emission** | ⚪ Roadmap | Read surface ships; emission is backlog |

## Admission & supply chain

| Capability | Status | Enforcement point |
|---|---|---|
| cosign keyless OIDC image signatures + CycloneDX SBOM per image | ✅ Enforced | CI (`image-sign-sbom.yml`). [security.md Layer 9](security.md#layer-9--engineering-controls-ci-gates) |
| Trivy + Container Image Scan + `cargo-deny` + RustSec audit | ✅ Enforced | CI on every PR |
| BYO runtime strict-mode admission gating | ✅ Enforced | [operations/byo-strict.md](operations/byo-strict.md) |
| Cosign-on-admission `ValidatingAdmissionPolicy` (reject unsigned sandbox images) | ⚪ Roadmap | Signature attestation in status ships; the enforcement webhook is the gap. [roadmap](roadmap.md#admission-and-supply-chain) |

## Runtimes

| Capability | Status | Notes |
|---|---|---|
| OpenClaw, OpenAI Agents (Py), Microsoft Agent Framework (Py), Anthropic Claude Agent SDK, LangGraph (Py + TS), Pydantic-AI | ✅ Enforced | Seven first-class adapters (LangGraph ships Python + TypeScript) + BYO. [runtimes.md](runtimes.md) |
| CrewAI, Microsoft Agent Framework (.NET), Strands / Google ADK | ⚪ Roadmap | [roadmap](roadmap.md#more-runtimes) — .NET returns when AGT ships `AgentMeshClient` for .NET |

## Multi-cluster & DR

| Capability | Status | Notes |
|---|---|---|
| Multi-cluster federation (federated registry, cross-cluster `TrustGraph` / `handoff`) | ⚪ Roadmap | [roadmap](roadmap.md#multi-cluster-and-dr) |
| AgentMesh registry / relay disaster recovery | ⚪ Roadmap | Backup / restore + regional failover playbook |
| Per-cluster token budget (in addition to per-tenant) | ⚪ Roadmap | [roadmap](roadmap.md#multi-cluster-and-dr) |

## Certification

| Capability | Status | Notes |
|---|---|---|
| CNCF Kubernetes AI Conformance | ⚪ Roadmap | Once the upstream certification programme is published. [roadmap](roadmap.md#observability-and-certification) |
| Public AAIF / CNCF Sandbox filing | ⚪ Roadmap | Backlog |

---

## See also

- **[Security model](security.md)** — what each layer enforces and where the seams are.
- **[Control mapping](compliance.md)** — kars controls mapped to NIST SP 800-53 and CIS Kubernetes families.
- **[Roadmap](roadmap.md)** — the themes behind every ⚪ row above.
- **[Security validation](security-validation.md)** — what CI verifies, cross-platform.
- **[Exec-brief walkthrough](use-cases/exec-brief-walkthrough.md#per-layer-proof)** — live per-layer proof commands.
