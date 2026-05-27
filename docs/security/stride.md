# STRIDE Threat Model — kars

> Companion to [`docs/security.md`](../security.md) (defense-in-depth layers). This document classifies the threats kars mitigates using STRIDE (Spoofing, Tampering, Repudiation, Information disclosure, Denial of service, Elevation of privilege) across the four primary trust boundaries.

## Trust boundaries

```
┌──────────────────────────────────────────────────────────────────┐
│  T1  External user / channel ↔ Inference Router (north edge)     │
├──────────────────────────────────────────────────────────────────┤
│  T2  Sandbox Agent (UID 1000) ↔ Inference Router (UID 1001)      │
├──────────────────────────────────────────────────────────────────┤
│  T3  Inference Router ↔ Azure / Foundry (south edge)             │
├──────────────────────────────────────────────────────────────────┤
│  T4  Sandbox A ↔ Sandbox B via AgentMesh (east-west)             │
└──────────────────────────────────────────────────────────────────┘
```

## STRIDE × boundary matrix

### T1 — North edge

| Category | Threat | Mitigation |
|---|---|---|
| **S** | Attacker forges channel webhook | Per-channel HMAC verification + Azure Front Door TLS |
| **T** | Tampered request body | Schema validation + size cap + content-type allowlist |
| **R** | Inbound action not auditable | Every request hashed into AGT receipt chain |
| **I** | API key in URL leaks via logs | Request URI redacted before logging; tokens accepted only via headers |
| **D** | Channel flood | Per-tenant token budget + global rate-limiter (500 rps, 50 rps/agent) |
| **E** | Cross-tenant request | Tenant header bound at the gateway; sandbox namespace isolated |

### T2 — Agent ↔ Router

| Category | Threat | Mitigation |
|---|---|---|
| **S** | Agent impersonates the router | Loopback-only socket (`127.0.0.1:8443`); UID-1001 listener; egress-guard iptables blocks UID 1000 from any other socket |
| **T** | Agent injects forged auth header | Router authenticates via IMDS / Workload Identity; agent never holds tokens |
| **R** | Tool calls not attributable | AGT policy receipt + per-tool span emitted to App Insights |
| **I** | Agent reads cluster metadata (IMDS) | UID-1000 iptables block on `169.254.169.254` except via router |
| **D** | Agent spam-saturates router | Per-agent rate limit + circuit-breaker on tool failures |
| **E** | Agent escapes UID-1000 jail | seccomp `kars-strict` (28 blocked syscalls), Landlock RO mount, drop-ALL caps, read-only rootfs |

### T3 — Router ↔ Azure

| Category | Threat | Mitigation |
|---|---|---|
| **S** | Spoofed Azure endpoint via DNS hijack | Router uses pinned host names + system trust roots; egress proxy allowlist |
| **T** | MITM of model traffic | mTLS to Foundry, public CA pinning where supported |
| **R** | Foundry call not linked to agent | Trace ID propagated end-to-end → App Insights |
| **I** | Token leak via response body | Router strips `Authorization` from outbound response; SDK tests cover it |
| **D** | Router stampede on retry | Bounded retry + circuit breaker per upstream |
| **E** | Stolen IMDS token used outside cluster | Tokens are workload-identity scoped + short-lived (≤ 1 hour) |

### T4 — East-west (AgentMesh)

| Category | Threat | Mitigation |
|---|---|---|
| **S** | Peer claims someone else's identity | Ed25519 signed prekey bundle; KNOCK signature check; trust score gating |
| **T** | Tampered ciphertext | Double Ratchet AEAD (XSalsa20-Poly1305) |
| **R** | Encrypted message not auditable | Outer envelope (sender, recipient, mesh, ts, receipt) emitted to AGT — content stays sealed |
| **I** | Relay reads message content | Relay sees only encrypted blobs; it has no session keys |
| **D** | Relay flooded by malicious peer | Per-identity rate limit at the relay + AGT global limit |
| **E** | Compromised peer escalates trust | Score clamped ±200 per signal; transitive paths capped; signed updates only |

---

## Residual risk

Items accepted today with explicit user-visible markers in source/docs:

- **Cosign-on-admission** — image signatures are produced and verifiable; admission does not yet *require* a signature on `KarsSandbox` reconciliation. Mitigation: cluster admins gate image pulls at the registry (ACR signed-content policy).
- **Static TrustGraph projection** — captured at sandbox creation; live edge changes require a sandbox roll. Mitigation: operators can roll affected sandboxes.
- **Per-cluster token budget** — only per-tenant budgets exist. A noisy tenant cannot starve the cluster but could starve its own peers. Mitigation: namespace ResourceQuota.

These gaps are tracked in [`docs/roadmap.md`](../roadmap.md).

---

## Re-evaluation cadence

This model is reviewed:

1. On every PR that introduces a new route / provider / external surface (gated by `ci/security-audit-required.sh`).
2. On every AGT Rust SDK release (`ci/vendored-patch-audit.sh`).
3. Quarterly, regardless of change volume.
