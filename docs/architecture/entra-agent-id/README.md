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
| [05-security-alignment.md](05-security-alignment.md) | Phase 5 — custom security attributes, CA baseline, scale-out invariant |
| [06-mesh-trust-design.md](06-mesh-trust-design.md) | Mesh trust model — Entra JWT verification on the AGT relay/registry |

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
| 5b | `4c0d466` | Controller-driven per-agent ARM RBAC assignment (auto Foundry binding) |
| 6.b | `78606a8` | AGT mesh registry verify endpoint (Entra JWT → pubkey fallback) |
| 6.c | `b300526` | Single `--mesh-trust=anonymous\|entra` operator switch on `kars up` |
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

## Operator surface (Phase 6.c)

The whole Entra Agent ID stack — blueprint, per-sandbox SPs, RBAC,
federated credentials, KAC, plus AGT mesh JWT verification — is
gated by **one** CLI flag on `kars up`:

```bash
# Anonymous tier (default) — zero Entra prerequisites
kars up --name myagent --mesh-trust=anonymous

# Entra tier — full provisioning (greenfield supported)
kars up --name myagent --mesh-trust=entra
```

When `--mesh-trust=entra`:

1. `kars mesh setup-trust` runs as part of `up` and provisions the
   tenant-wide blueprint + custom security attributes + conditional
   access baseline (if not already present)
2. The controller picks up `KarsAuthConfig/default` and, for every
   `KarsSandbox`, mints a per-sandbox typed agent identity SP +
   federated credential + Foundry RBAC scoped to that SP
3. AGT mesh relay + registry get patched with
   `AGENTMESH_ENTRA_AUDIENCE` + `AGENTMESH_ENTRA_TENANT_ID` so they
   verify peer JWTs against Entra's JWKS

Anonymous mode skips all of the above — sandboxes share the cluster's
workload identity for Foundry, and the AGT mesh runs in trust score 0
(everyone-accepts-everyone). Good for local dev, demos, and tenants
where the operator can't (or won't) provision Entra resources.

## Open follow-ups

- **Phase 1 LOC budget** (next): split `agent_identity.rs` (1511 LOC),
  `agent_id_provisioning.rs` (859 LOC), `auth_config_reconciler.rs`
  (835 LOC), `sidecar_client.rs` (1468 LOC), and
  `cli/src/commands/mesh/agent_id_setup.ts` (1106 LOC) into focused
  modules. Tracked via `// ci:loc-ok` markers in each file.
- **Multi-tenant CLI**: `kars mesh setup-trust` currently runs against
  the operator's signed-in tenant. The `tenantId` plumbing is in
  place (CRD + sidecar + router) but the CLI accepts it as a no-op
  today.

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
