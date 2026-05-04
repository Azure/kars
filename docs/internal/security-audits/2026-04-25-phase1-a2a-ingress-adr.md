# Security Audit: `phase1/a2a-ingress-adr`

**Capability:** documentation-only PR. Adds ADR 0001 codifying the
A2A 1.0 ingress architecture: single shared `azureclaw-a2a-gateway`
component as the only public TLS endpoint, router never directly
exposed, surgical per-sandbox opt-in via `ClawSandbox.spec.a2a`,
multi-layer defense in depth (Cilium L7 → gateway → cluster-internal
mTLS → router L7), router-internal module isolation, time-bounded
revocable exposure with controller-driven NetworkPolicy lifecycle.

**Type:** docs only. No code change. No new attack surface.

## 1. Summary

New files:

- `docs/adr/README.md` — ADR index.
- `docs/adr/0001-a2a-ingress-front-edge.md` — full ADR with eight
  decision blocks (D1 single gateway; D2 router never publicly
  exposed; D3 Cilium L7 defense in depth; D4 router-internal
  module isolation; D5 sidecar process isolation deferred; D6
  surgical opt-in / revocation; D7 outbound unchanged; D8 AgentCard
  custody).
- Update internal Phase 1 plan §7 entry 2 with the gateway
  posture, opt-in surface, and pod layout reminder.

No code is added or modified. The merged scaffold
(`phase1/a2a-1.0.0-scaffold`) data model is referenced but unchanged.

## 2. Threat model delta

This PR is documentation-only and adds **no attack surface**. Its
purpose is to constrain future PRs. The constraints captured here
that subsequent PRs MUST honour:

- Router never has a public-internet ingress.
- A2A inbound exposure is per-sandbox opt-in via CRD field, default
  off, time-bounded (max 30 days), revocable within one reconcile
  loop.
- Three independent gates between the internet and Rust parser code
  in the router (Cilium L7 ingress → gateway → router-side L7).
- Caller pinning by JWS thumbprint, not subject alone.
- Skill allow-list at gateway, re-checked at router.
- Gateway routing table is controller-owned ConfigMap; gateway has
  read-only RBAC.
- Module-level isolation: A2A handler module structurally cannot
  import IMDS / Foundry credential types (CI-enforced).
- Sidecar process isolation deferred but forward-compatible.

## 3. OWASP mapping

- **OWASP API1:2023 — Broken Object Level Authorization:** D6 caller
  pinning by thumbprint and per-sandbox routing table prevent
  authorised callers from reaching unintended sandboxes.
- **OWASP API4:2023 — Unrestricted Resource Consumption:** rate
  limits, body caps, session-max-seconds, expiresAt all bound the
  blast radius of any single caller.
- **OWASP API8:2023 — Security Misconfiguration:** controller-owned
  routing ConfigMap + admission validators on `spec.a2a` make
  unsafe configurations structurally impossible to apply.
- **OWASP LLM06 — Excessive Agency:** advertisedSkills allow-list is
  the explicit grant; ToolPolicy still gates each call inside.

## 4. AuthN / AuthZ path

Defined in the ADR (D1, D6). This PR does not implement any new
AuthN/AuthZ — it specifies the contract that subsequent code PRs
must honour.

## 5. Crypto inventory

No new crypto. Subsequent PRs will use:

- AgentCard JWS verification: `SigningProvider` trait (existing).
- Cluster-internal mTLS: Workload Identity certs (existing).
- Gateway public TLS: cert-manager (existing).

## 6. Backend equivalence

No behavioural delta. Documentation only.

## 7. Compliance / dev-only branching

Per implementation-plan §0.2 #11, this branch merges to `dev` only.

## 8. Sign-off

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
