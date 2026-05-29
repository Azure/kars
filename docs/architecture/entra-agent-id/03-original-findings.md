# Entra Agent ID GA — POC findings (2026-05-27)

## TL;DR

The GA Entra Agent ID model is **architecturally cleaner** than our current
`api://agentmesh` precursor pattern AND **fits kars perfectly**:

- The SDK ships as a **sidecar container** (`mcr.microsoft.com/entra-sdk/auth-sidecar:1.0.0-azurelinux3.0-distroless`)
- It runs in the **same pod** as the agent — same pattern kars already uses for the inference-router + egress-guard initContainer
- It exposes a simple HTTP API (`/AuthorizationHeader/{serviceName}`, `/Validate`, `/health`)
- The sandbox calls it via `curl http://localhost:5000/...` — no bash token-exchange dance

## Measured numbers (this machine, M3 Mac arm64, image runs under emulation)

| Phase | Wall-clock |
|---|---|
| `docker pull` (cached) | 0.28s |
| `docker run` | 0.14s |
| `/health` ready (cold start) | **1.27s** |
| Token call latency | 0.02s |
| Memory steady-state | **82 MB** |
| Image size | 56.6 MB |
| Required sidecar resources (chart manifest) | 128Mi/100m → 256Mi/250m |

## API surface confirmed against the live container

- `GET /health` — 200 (used for K8s readiness/liveness probes)
- `GET /AuthorizationHeader/Graph?AgentIdentity=<id>` — 401 without creds (matches docs)
- `GET /AuthorizationHeader/{serviceName}?AgentIdentity=<id>&AgentUserId=<oid>` — autonomous-user-account flow
- `POST /Validate` (with user token) — 401 without creds (used for OBO flow)

## What we keep, what we delete

| File | Status | Why |
|---|---|---|
| `controller/src/fedcred.rs` (363 L) | **Replace** | Becomes `POST /beta/serviceprincipals/Microsoft.Graph.AgentIdentity` (Graph API call) |
| `controller/src/fedcred_reaper.rs` (236 L) | **Replace** | `DELETE /beta/serviceprincipals/<agent-id>` |
| `cli/src/commands/mesh/setup-trust.ts` (235 L) | **Repurpose** | Provisions the *agent identity blueprint* (not the app reg) — one-time, tenant admin |
| `sandbox-images/openclaw/entrypoint.sh:158-235` (~80 L token-exchange bash) | **Delete** | Sidecar handles it |
| `deploy/helm/kars/values.yaml entraAuth.enabled` (kill switch) | **Delete** | No longer needed; sidecar fails-fast on missing config |
| `AGT_SKIP_ENTRA` env var + controller injection | **Delete** | Same |

## What we add

- 4th container in the sandbox pod: `entra-sdk` (sidecar)
- One `KarsAgentIdentity` CRD per logical agent role, declaring:
  - `agentIdentityBlueprintId` (the tenant-wide blueprint ID, provisioned via `kars mesh setup-trust`)
  - `owners` (Entra user OIDs or M365 groups)
  - `sponsors` (Entra user OIDs or M365 groups)
- Controller reconciler that on `KarsSandbox` create:
  1. `POST /beta/serviceprincipals/Microsoft.Graph.AgentIdentity` with the blueprint ID → gets back the per-sandbox agent identity client ID
  2. Writes the client ID into the sandbox pod's env vars (`AzureAd__ClientId`)
- Inference-router consumes tokens via local HTTP to the sidecar

## Permission model shift

| Today | With GA |
|---|---|
| Cluster MI with federated credential to each sandbox SA | Cluster MI is the blueprint credential; per-sandbox identities inherit from blueprint |
| `api://agentmesh` audience (custom, requires app reg) | Native `agentIdentity` resource type — Microsoft.Graph handles all token exchange |
| Tier 1 verified via `AGT_OAUTH_TOKEN` smuggled through env | Per-call token request from sidecar — no env-var smuggling |

## Blockers / opens

1. **Need agent identity blueprint provisioned in tenant** — one-time admin action.
   The `kars mesh setup-trust` command would be rewritten to do this via
   Graph API instead of `az ad app create`.

2. **The image is amd64-only as of 1.0.0 GA.** Emulation works on kind (M3 Mac),
   measured at +0.2s startup overhead vs amd64 native. Acceptable for dev;
   prod is amd64 already.

3. **`/AuthorizationHeader/{serviceName}` requires service-name pre-registration**
   in the sidecar config map. We need a service name for the AGT registry
   (currently called via `api://agentmesh/.default`). The new SDK config
   schema covers this — see `AzureAd__DownstreamApi__<name>__*` env vars.

4. **OBO flow opens** — for `kars connect --as <user>` scenarios where the
   sandbox acts on a real user's behalf. The GA SDK has `/Validate` for this
   but it's a phase-2 integration.

## Migration plan (concrete)

### Phase 1 — sidecar wired into one sandbox profile (~3 days)

- Add `entra-sdk` container to controller's pod-template emission for sandboxes
  with `KarsAgentIdentity` bound
- Helm value `azure.entraAgentIdSdk.enabled` (default off) gates whether
  the controller adds the sidecar
- Strip `entrypoint.sh:158-235` when `KARS_USE_SIDECAR=1` is set
- Inference-router calls sidecar instead of using `AGT_OAUTH_TOKEN`

### Phase 2 — `KarsAgentIdentity` CRD + Graph reconciler (~1 week)

- New CRD `kars.azure.com/v1alpha1.KarsAgentIdentity`
- Controller calls Graph `POST /beta/serviceprincipals/Microsoft.Graph.AgentIdentity`
- Output the per-sandbox client ID as a CR status field, mounted into sandbox env

### Phase 3 — `kars mesh setup-trust` rewrite (~2 days)

- Replace `az ad app create --identifier-uris api://agentmesh` with
  Graph `POST /beta/identityGovernance/agentIdentityBlueprints`
- New required tenant-admin step: assign sponsors via the wizard or CLI

### Phase 4 — retire `api://agentmesh` (~2 days)

- Delete `fedcred.rs` + `fedcred_reaper.rs` (599 L total)
- Delete `entrypoint.sh:158-235` (~80 L)
- Delete `KARS_DISABLE_ENTRA_AUTH` / `AGT_SKIP_ENTRA` env vars
- Document the migration path in `docs/upgrade-to-entra-agent-id.md`

Total: ~2 weeks for the four phases.

## Recommendation

**Adopt the sidecar.** It's a strictly cleaner architecture, the GA API is the
right primitive for what we're trying to model, and the performance is fine
(1.27s startup, 82 MB, 0.02s per token call). The migration deletes more
code than it adds (599+80 = 679 LoC out, vs ~300 LoC in for the CRD reconciler
+ Helm wiring).

The blocker is **tenant-side blueprint provisioning** — a one-time admin
action that `kars mesh setup-trust` will automate.
