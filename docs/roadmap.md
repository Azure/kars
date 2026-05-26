# AzureClaw Roadmap

> Living document. Updated each release. The project is currently **`v0.1.0` — pre-1.0 development**; the v1.0.0 target below describes the runtime substrate we are tracking towards as the first stable release. Everything beyond v1.0.0 is intent, not commitment.

## v1.0.0 — General Availability of the runtime substrate (target)

Capabilities feature-complete on `main` and exercised by CI; pending a formal `v1.0.0` tag once the public CRD surface is frozen:

- ClawSandbox CRD `v1alpha1` (frozen for v1) — see [`docs/architecture/crd-versioning.md`](architecture/crd-versioning.md).
- Six first-class agent runtimes: OpenClaw, OpenAI Agents (Python), Microsoft Agent Framework (Python), Anthropic Claude Agent SDK, LangGraph (Python + TypeScript), Pydantic-AI.
- BYO runtime path with strict-mode admission gating ([`docs/operations/byo-strict.md`](operations/byo-strict.md)).
- Inference router with IMDS / Workload-Identity broker, content-safety floor, per-sandbox token budgets, 18 Foundry API groups, MCP Streamable-HTTP + SSE compat, A2A transport.
- E2E-encrypted inter-agent messaging via AgentMesh (Signal Protocol — X3DH + Double Ratchet). The Signal session is owned end-to-end by the agent processes; the inference router only WebSocket-bridges opaque ciphertext and never holds session keys, so KNOCK accept/deny is — and stays — an agent-side decision. The TrustGraph CRD ships in v1alpha1 reconciler-only mode (projection mounted at sandbox creation); **router-side mesh-admission gating** against the projected graph (refuse to bridge a WS for an edge not in the graph — separate from KNOCK, which the router cannot see) is on the v1.1 roadmap.
- Defense-in-depth sandbox: read-only rootfs, UID-1000 + UID-1001 split, drop-ALL caps, custom seccomp (`azureclaw-strict`), Landlock, iptables UID-based egress, optional Kata.
- AGT integration: `PolicyEngine`, `TrustManager`, `AuditLogger`, `RateLimiter`, `BehaviorMonitor` consumed via four provider traits (`MeshProvider`, `PolicyDecisionProvider`, `AuditSink`, `SigningProvider`).
- Operator TUI + `azureclaw up / add / dev / connect / handoff / mesh / policy learn / migrate / convert / claw attest`.
- Supply-chain: cosign keyless OIDC signatures, SBOM (CycloneDX) per image, Trivy + Container Image Scan + Rust Supply-Chain Gate (cargo-deny) + RustSec advisory audit in CI.

Documented v1.0 gaps (tracked in source as `// v1.1:` comments):

- Cosign-on-admission gating (CRD-side admission webhook) — read surface is shipped; enforcement is post-v1.
- TrustGraph live updates — projection is mounted at sandbox creation; topology changes require sandbox re-roll until v1.1.
- Microsoft Agent Framework **.NET** — `Microsoft.AgentGovernance` 3.3.0 ships no `AgentMeshClient`. Adapter trimmed; will return when AGT lands inter-agent comms for .NET.
- A2A gateway in-binary JWS verification — `azureclaw_a2a_core::verify_inbound_card` is library-complete & tested; the gateway binary today consumes the verified-caller subject from the upstream Gateway-API mTLS handshake (`X-A2A-Agent-Subject` header). Wiring the verifier as an opt-in axum layer inside the gateway is a v1.1 task; see [`docs/architecture/a2a-gateway.md`](architecture/a2a-gateway.md).

## v1.1 — Topology + signed CRD upgrades

Targets:

- **CRD `v1alpha2` + conversion webhook** — see [`docs/architecture/crd-versioning.md`](architecture/crd-versioning.md). Converts in both directions; existing `v1alpha1` objects continue to round-trip.
- **TrustGraph dynamic projection + router-side mesh-admission gating** — controller watches mesh edges and patches the in-pod projection without a sandbox restart, **and** the router reads the projected graph at the WS-bridge entrypoint to refuse forwarding for edges that aren't in the graph (admission-time mesh ACL). This is **not** KNOCK gating — KNOCK lives inside the Signal session the agent owns and the router cannot decrypt — it's a coarser pre-handshake admission check that complements the agent-side KNOCK + trust-score check.
- **InferencePolicy aggregate budgets** — persisted token counters across requests (per-hour / per-day windows) with `rejectOnExceed` enforced at the router. Today only `tokenBudget.perRequestTokens` is enforced; aggregate counters are accepted on the spec and surfaced in status but not yet metered.
- **Cosign-on-admission** — ValidatingAdmissionPolicy that rejects sandboxes whose images lack a cosign signature matching configured identity / issuer.
- **A2A gateway in-binary JWS verifier** — opt-in axum layer in `azureclaw-a2a-gateway` that calls `azureclaw_a2a_core::verify_inbound_card`, removes reliance on upstream Gateway-API mTLS for the trust decision, and lets the gateway run in non-AGC topologies.
- **CrewAI runtime adapter** — first-class.
- **MAF .NET adapter** — re-introduce when AGT ships `AgentMeshClient` for .NET.
- **Observability dashboards** — App Insights workbooks shipped under `deploy/workbooks/`.

## v1.2 — Multi-cluster + DR

Targets (subject to revisit):

- **Multi-cluster federation** — federated mesh registry, cross-cluster TrustGraph, cross-cluster `azureclaw handoff`.
- **AgentMesh registry/relay disaster recovery** — backup / restore path for relay state, regional failover playbook.
- **Per-cluster token budget** — global token budget enforced at the router, in addition to per-tenant.
- **Public certification harness** — Kubernetes AI Conformance suite once the upstream certification programme is published.

## Backlog (no timeline)

- Confidential controller + router-mediated controller egress.
- Native Strands / Google ADK runtime adapters when their tool-loop SDKs stabilise.
- Public AAIF / CNCF Sandbox filing.
- Signed reconcile audit-chain *emission* (the read surface is already shipped).

---

This roadmap is intentionally short. AzureClaw's v1.0 surface is large; we'd rather ship fewer v1.1 capabilities at high quality than chase a wide v1.1 wishlist.
