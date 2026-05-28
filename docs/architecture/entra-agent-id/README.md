# Entra Agent ID — Architecture Index

> kars per-sandbox Entra Agent ID with **shared auth-sidecar** architecture.
> Status: ✅ verified live in Microsoft corp tenant on `kars-aks` (2026-05-28).

This directory is the canonical reference for how kars provisions and uses
per-sandbox [Microsoft Entra Agent Identities][entra-agent-id]. Documents are
numbered by deployment order and scope.

[entra-agent-id]: https://learn.microsoft.com/en-us/entra/agent-id/

## Contents

| Doc | Scope |
|-----|-------|
| [01-runtime-token-flow.md](01-runtime-token-flow.md) | Runtime auth flow — sidecar → blueprint → agent token → Foundry |
| [02-aci-token-flow.md](02-aci-token-flow.md) | Alternative ACI-based token flow (reference / not deployed) |
| [03-original-findings.md](03-original-findings.md) | Initial POC findings + design constraints |
| [05-security-alignment.md](05-security-alignment.md) | Phase 5 — custom security attributes, CA baseline, scale-out invariant |
| [00-poc-archive.md](00-poc-archive.md) | Archived original POC README |

## TL;DR architecture

```
                            ┌─────────────────────┐
                            │ Microsoft Entra ID  │
                            │  - blueprint app    │
                            │  - per-sandbox      │
                            │    agent identities │
                            │    (typed SPs)      │
                            └──────────┬──────────┘
                                       │
                       ┌───────────────┼────────────────────┐
                       │ (Pattern A: IMDS-MI bridge)        │
                       │ (Pattern B: WI federated subject)  │
                       │                                    │
            ┌──────────▼─────────────┐         ┌───────────▼───────────┐
            │ shared entra-auth-     │         │ Foundry data plane    │
            │ sidecar (Deployment)   │         │ (Azure RBAC per       │
            │ in kars-system, x2 HA  │         │  agent identity SP)   │
            └──────────┬─────────────┘         └───────────▲───────────┘
                       │ HTTP                              │
                       │ /AuthorizationHeaderUnauthenticated/Foundry
                       │ ?AgentIdentity=<sandbox appId>    │
                       │                                   │
            ┌──────────▼─────────────────────────────────────────────────┐
            │ kars sandbox pod                                            │
            │  ┌────────────────────┐    ┌──────────────────────────┐    │
            │  │ openclaw agent     │───▶│ inference-router         │    │
            │  │  (UID 1000)        │ http (UID 1001)               │    │
            │  │  pinned via env →  │    │ • fail-closed sidecar    │    │
            │  └────────────────────┘    │   mode (no WI/IMDS/API)  │    │
            │                            │ • pins tid, principal,   │    │
            │   iptables egress-guard:   │   aud, exp on every      │    │
            │   UID 1000 → blocked from  │   sidecar response       │    │
            │   IMDS + sidecar           │ • forwards to Foundry    │    │
            │                            └──────────┬───────────────┘    │
            └───────────────────────────────────────┼────────────────────┘
                                                    │ TCP 5000
                                                    ▼
                                              NetworkPolicy gate
                                              (kars-system ns, port 5000)
```

## Pattern selection

| Tenant capability | Pattern | Sidecar credential source |
|-------------------|---------|---------------------------|
| Tenant allows AKS OIDC as FIC issuer | B (WorkloadIdentity) | `SignedAssertionFilePath` against projected SA token |
| Tenant rejects AKS OIDC (Microsoft corp, restricted) | A (ManagedIdentityImds) | `SignedAssertionFromManagedIdentity` via IMDS |

The kars CLI `kars mesh setup-trust` auto-detects which pattern works in your
tenant and provisions accordingly. Pattern A is the conservative default;
Pattern B requires no per-cluster controller MI.

## Phase ledger

| Phase | Commit | Description |
|-------|--------|-------------|
| 0 | `a124f0c` | Branch surgery — drop per-pod injection, keep shared model |
| 1 | `27d5495` | Shared sidecar Helm chart (Deployment, Service, NetworkPolicy, SA) |
| 2 | `7c77ec8` | Controller egress rule + NP label fix |
| 3 | `b021610` | Router sidecar_client + 4-claim pinning (tid, principal, aud, exp) |
| 4 | `405e331` | CLI + Bicep dual-pattern auto-detect |
| 5 | `8e8e811` | Custom security attributes + scale-out invariant + CA baseline |
| 7 | `8cfb05d` | Live deploy + multi-agent exec-brief demo verified on kars-aks |

## Key files

| Layer | File |
|-------|------|
| Helm — sidecar | `deploy/helm/kars/templates/auth-sidecar-{deployment,service,networkpolicy,serviceaccount}.yaml` |
| Helm — controller | `deploy/helm/kars/values.yaml` (`entraSidecar:` block) |
| Bicep | `deploy/bicep/agent-id-trust.bicep` |
| Bicep standalone | `deploy/bicep/standalone/foundry-rbac.bicep`, `custom-security-attributes.sh`, `conditional-access-baseline.sh` |
| Controller — CRD | `controller/src/auth_config.rs` (`KarsAuthConfig`) |
| Controller — provisioning | `controller/src/agent_id_provisioning.rs`, `controller/src/agent_identity.rs` |
| Controller — reconciler | `controller/src/auth_config_reconciler.rs` |
| Router | `inference-router/src/sidecar_client.rs`, `inference-router/src/auth.rs` |
| CLI | `cli/src/commands/mesh/agent_id_setup.ts`, `cli/src/commands/mesh/agent_id_setup_bicep.ts` |
| CLI | `cli/src/commands/up/sandbox_bringup.ts` (Foundry RBAC inline Bicep) |

## Open follow-ups

- **Phase 5b** (next PR): controller-driven per-agent ARM RBAC assignment.
  Eliminates the manual `az role assignment create` step operators run today
  for each new sandbox.
- **Phase 6** (separate PR): mesh trust — use Entra-signed JWTs (instead of
  anonymous tier) for AGT trust scoring.

## Live validation snapshot (2026-05-28)

Verified end-to-end on `kars-aks` cluster (Microsoft corp tenant `72f988bf-...`):

- 5 typed `microsoft.graph.agentIdentity` SPs derived from one typed
  `microsoft.graph.agentIdentityBlueprint`, each with its own `appId`, sponsors,
  and Foundry RBAC.
- Real Foundry tokens minted via shared sidecar — JWT decode confirms all four
  claim pins match.
- Multi-agent exec-brief demo: parent + 3 sub-agents booted, exchanged AGT-mesh
  messages, transferred files via E2E encrypted relay, all under their own
  agent identities. 65+ successful Foundry 200s, 0 PermissionDenied,
  0 NetworkPolicy denials.
