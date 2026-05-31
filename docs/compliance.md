# Control mapping

This page maps kars's **shipped, enforced** controls (the ✅ rows in
**[maturity.md](maturity.md)**) to two widely used control catalogues:

- **NIST SP 800-53 Rev 5** control families and representative controls.
- **CIS Kubernetes Benchmark** sections.

> **This is a self-assessment aid, not a certification or attestation.** kars has not
> been formally audited against these catalogues. The mapping is informative: it helps a
> security reviewer locate where a kars control satisfies (or contributes to) a familiar
> requirement. Reconciler-only, library-only, and roadmap items from
> [maturity.md](maturity.md) are **excluded** here — only controls the runtime enforces
> today are mapped. Always verify enforcement on your own cluster using the
> [exec-brief walkthrough](use-cases/exec-brief-walkthrough.md#per-layer-proof).

## Access control (NIST **AC**)

| kars control | NIST 800-53 | CIS Kubernetes | Where |
|---|---|---|---|
| Per-sandbox Entra Agent ID; no shared identity | AC-2, AC-3, AC-6 | 5.1.x (RBAC) | [agent-identity.md](agent-identity.md) |
| Least-privilege Azure RBAC on the kubelet identity (scoped roles) | AC-6, AC-6(1) | 5.1.1, 5.1.3 | [security.md → Identity & access](security.md#identity--access) |
| Agent cannot reach IMDS (UID 1000 blocked) | AC-3, AC-4 | 5.2.x | [security.md Layer 5](security.md#layer-5--network-segmentation) |
| Per-namespace tenant isolation, no shared state | AC-4, SC-7 | 5.3.2 | [multi-tenant.md](multi-tenant.md) |

## System & communications protection (NIST **SC**)

| kars control | NIST 800-53 | CIS Kubernetes | Where |
|---|---|---|---|
| No standing credentials; Workload Identity / IMDS broker | SC-12, SC-28, IA-5 | 5.4.1 | [security.md → Identity & access](security.md#identity--access) |
| iptables UID egress guard (agent → localhost + DNS only) | SC-7, SC-7(5) | 5.3.2 | [security.md Layer 5](security.md#layer-5--network-segmentation) |
| NetworkPolicy default-deny egress | SC-7, SC-7(5) | 5.3.2 | Controller-reconciled |
| Router L7 allowlist on every CONNECT + malicious-domain blocklist | SC-7, SC-7(8), SI-3 | — | [egress-proxy.md](egress-proxy.md) |
| E2E-encrypted inter-agent messaging (Signal, forward secrecy) | SC-8, SC-8(1), SC-12 | — | [security.md Layer 8](security.md#layer-8--end-to-end-encrypted-mesh) |
| A2A caller authentication via Gateway-API mTLS | SC-8, IA-3, IA-9 | — | [architecture/a2a-gateway.md](architecture/a2a-gateway.md) |
| Kata + SEV-SNP confidential isolation (opt-in) | SC-7(21), SC-39 | — | [security.md Layer 2](security.md#layer-2--pod-isolation-optional-vm) |

## Audit & accountability (NIST **AU**)

| kars control | NIST 800-53 | CIS Kubernetes | Where |
|---|---|---|---|
| SHA-256 hash-chained audit log (tamper detection) | AU-9, AU-10, AU-2 | — | [security.md Layer 7](security.md#layer-7--behavioural-governance-agt) |
| Per-request governance decisions audited (deny reasons, trust deltas) | AU-2, AU-3, AU-12 | — | `AuditLogger` |
| Prometheus metrics on every external call | AU-6, SI-4 | — | [security.md Layer 6](security.md#layer-6--inference-safety) |

> Audit-chain head *signing* (non-repudiation, NIST AU-10) is **roadmap** — the property
> enforced today is detection, not non-repudiation. See [maturity.md → Audit & attestation](maturity.md#audit--attestation).

## System & information integrity (NIST **SI**)

| kars control | NIST 800-53 | CIS Kubernetes | Where |
|---|---|---|---|
| Content Safety / Prompt Shield (Foundry providers) | SI-3, SI-4, SI-10 | — | [security.md Layer 6](security.md#layer-6--inference-safety) |
| BehaviorMonitor — burst / failure / denial anomaly detection | SI-4, SI-4(2) | — | [security.md Layer 7](security.md#layer-7--behavioural-governance-agt) |
| RateLimiter — global + per-agent token bucket | SC-5, SI-10 | — | `inference-router/src/governance/mod.rs` |
| Per-request + per-tenant token budgets | SC-6, SC-5 | — | Router |

> Content Safety is **not** enforced on GitHub Copilot / GitHub Models provider paths —
> see [security.md → What we do not defend against](security.md#what-we-do-not-defend-against).

## Configuration management (NIST **CM**)

| kars control | NIST 800-53 | CIS Kubernetes | Where |
|---|---|---|---|
| Read-only rootfs, drop-ALL caps, non-root, no priv-esc | CM-6, CM-7 | 5.2.x (Pod Security) | [security.md Layer 3](security.md#layer-3--container-hardening) |
| `kars-strict` seccomp profile (blocks `mount`, `ptrace`, `bpf`, …) | CM-7, SI-3 | 5.2.x | [security.md Layer 4](security.md#layer-4--kernel-confinement-seccomp) |
| Pod Security Standards labels (`enforce`/`audit`/`warn`) | CM-6, CM-7 | 5.2.1 | [security.md → Pod Security Standards](security.md#pod-security-standards) |
| Declarative, signed CRDs as the configuration surface | CM-2, CM-3, CM-5 | — | [api/crd-reference.md](api/crd-reference.md) |

## Supply chain risk management (NIST **SR**)

| kars control | NIST 800-53 | CIS Kubernetes | Where |
|---|---|---|---|
| cosign keyless OIDC image signatures | SR-4, SR-11, SA-12 | — | [security.md Layer 9](security.md#layer-9--engineering-controls-ci-gates) |
| CycloneDX SBOM per image | SR-3, SR-4 | — | CI `image-sign-sbom.yml` |
| Trivy + Container Image Scan + `cargo-deny` + RustSec audit | SR-3, RA-5 | 5.5.1 | CI on every PR |
| Pinned Actions (SHA), pinned base images, custom-crypto gate | SR-3, SR-11 | — | [security.md Layer 9](security.md#layer-9--engineering-controls-ci-gates) |

> Cosign-on-admission enforcement (rejecting unsigned sandbox images at admission, NIST
> SR-4(3)) is **roadmap** — the signature read surface ships today; the
> `ValidatingAdmissionPolicy` is the gap. See [maturity.md → Admission & supply chain](maturity.md#admission--supply-chain).

---

## How to use this page

1. Find the requirement you care about by its NIST family or CIS section.
2. Follow the **Where** link to the design doc that explains the control.
3. Confirm it is ✅ **Enforced** in [maturity.md](maturity.md) — anything not listed here is
   reconciler-only, library-only, or roadmap and must not be relied on for compliance.
4. Prove it on your cluster with the [exec-brief walkthrough](use-cases/exec-brief-walkthrough.md#per-layer-proof).

## See also

- **[Maturity & enforcement status](maturity.md)** — the ✅ / 🟡 / 🔵 / ⚪ source of truth.
- **[Security model](security.md)** — the layered control plane in full.
- **[STRIDE](security/stride.md)** — the threat model these controls answer.
- **[Security validation](security-validation.md)** — what CI verifies, cross-platform.
