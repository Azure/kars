# kars Roadmap

> Living document. The project is at **`v0.1.0`** — see [`CHANGELOG.md`](../CHANGELOG.md) for what's shipped. This roadmap lists the themes we are evolving the platform towards. Versions and ordering may change as we learn from production deployments.

## What ships today (`v0.1.0`)

The current public surface — exercised by CI (Kind E2E + manual matrix) on every push to `main`:

- **`KarsSandbox` CRD** (`kars.azure.com/v1alpha1`) plus eight sibling CRDs covering inference policy, tool policy, A2A agents, MCP servers, memory, evaluation, egress approval, and trust topology.
- **Six first-class agent runtimes:** OpenClaw, OpenAI Agents (Python), Microsoft Agent Framework (Python), Anthropic Claude Agent SDK, LangGraph (Python + TypeScript), Pydantic-AI. Plus a documented **BYO runtime** path with strict-mode admission gating ([`operations/byo-strict.md`](operations/byo-strict.md)).
- **Inference router** with IMDS / Workload-Identity broker, content-safety floor, per-sandbox token budgets, the full Foundry data-plane API surface, MCP Streamable-HTTP + SSE compat, A2A transport.
- **E2E-encrypted inter-agent messaging** via AgentMesh (Signal Protocol — X3DH + Double Ratchet). The Signal session is owned end-to-end by the agent processes; the inference router only WebSocket-bridges opaque ciphertext.
- **Defense-in-depth sandbox:** read-only rootfs, UID-1000 + UID-1001 split, drop-ALL caps, custom seccomp (`kars-strict`), Landlock, iptables UID-based egress, optional Kata.
- **AGT integration:** `PolicyEngine`, `TrustManager`, `AuditLogger`, `RateLimiter`, `BehaviorMonitor` consumed via four provider traits (`MeshProvider`, `PolicyDecisionProvider`, `AuditSink`, `SigningProvider`).
- **Operator UX:** `kars up / add / dev / connect / handoff / mesh / policy learn / migrate / convert / claw attest` plus the operator TUI.
- **Supply chain:** cosign keyless OIDC signatures, SBOM (CycloneDX) per image, Trivy + Container Image Scan + Rust Supply-Chain Gate (cargo-deny) + RustSec advisory audit in CI.

## What we're working on next

Themes ordered roughly by what we expect to ship first. Nothing here is dated; landing depends on what production use surfaces as the next bottleneck.

### Trust topology, end to end

- **`TrustGraph` router-side mesh-admission gating.** Today the projected graph is mounted at sandbox creation and consumed by the agent's KNOCK handler. The next step is a coarser pre-handshake admission check in the router that refuses to bridge a WebSocket for an edge that is not in the graph — separate from KNOCK (which the router cannot decrypt) and complementary to it.
- **`TrustGraph` dynamic projection.** The controller watches mesh edges and patches the in-pod projection without a sandbox restart, so topology changes do not require a roll.

### Inference policy enforcement

- **Aggregate token budgets** in `InferencePolicy` — persisted counters across requests (per-hour / per-day windows) with `rejectOnExceed` enforced at the router. Today only `tokenBudget.perRequestTokens` is enforced; aggregate counters are accepted on the spec and surfaced in status but not yet metered.

### A2A gateway hardening

- **In-binary JWS verification.** `kars_a2a_core::verify_inbound_card` is library-complete and unit-tested; the gateway binary today consumes the verified-caller subject from the upstream Gateway-API mTLS handshake (`X-A2A-Agent-Subject`). The next step is an opt-in axum layer inside the gateway that calls the verifier directly, so the gateway can run in non-AGC topologies.

### Admission and supply chain

- **Cosign-on-admission gating.** A `ValidatingAdmissionPolicy` that rejects sandboxes whose images lack a cosign signature matching the configured identity / issuer. The read surface (signature attestation in status) is already shipped; the enforcement webhook is the missing piece.

### Egress allowlist authority flip

- **Make the signed OCI allowlist authoritative.** Today the inline `allowedEndpoints` field on `KarsSandbox` is the source of truth and the signed artifact (when present) is a parallel check. The plan is to make the signed artifact the only source of truth, with the inline field becoming a read-only convenience. See [`egress-proxy.md`](egress-proxy.md).

### More runtimes

- **CrewAI** as a first-class runtime adapter.
- **Microsoft Agent Framework (.NET)** — currently trimmed because `Microsoft.AgentGovernance` 3.x ships no `AgentMeshClient` for .NET. Returns when AGT lands inter-agent comms on that platform.
- **Native Strands / Google ADK adapters** once their tool-loop SDKs stabilise.

### Multi-cluster and DR

- **Multi-cluster federation** — federated mesh registry, cross-cluster `TrustGraph`, cross-cluster `kars handoff`.
- **AgentMesh registry / relay disaster recovery** — backup / restore path for relay state, regional failover playbook.
- **Per-cluster token budget** enforced at the router, in addition to per-tenant.

### Observability and certification

- **App Insights workbooks** shipped under `deploy/workbooks/` so the dashboards we use internally are reproducible.
- **Public certification** against the CNCF Kubernetes AI Conformance suite once the upstream certification programme is published.

### Backlog (no timeline)

- Confidential controller + router-mediated controller egress.
- Signed reconcile audit-chain *emission* (the read surface is already shipped).
- Public AAIF / CNCF Sandbox filing.

---

This roadmap is intentionally short. The current surface is large; we'd rather ship fewer themes at high quality than chase a wide wishlist.
