# Per-sandbox identity (Entra Agent ID)

Every kars sandbox runs under its own **Microsoft Entra Agent ID**.
When the sandbox calls Foundry, Microsoft Graph, Key Vault, or any
Azure service, the calling principal is `kars-<cluster>-<sandbox>` —
not a cluster-wide shared identity. That means:

- **Audit logs name the sandbox.** `kubectl get pod` and Foundry's
  sign-in logs reference the same human-readable label.
- **RBAC is per-sandbox.** Grant `Cognitive Services User` to one
  agent without granting it to others. `kars policy grant` is the
  thin wrapper.
- **No long-lived secrets.** No client secrets, no API keys, no PFX
  files on disk. The entire chain is federated through Microsoft
  Entra; tokens are minted on demand and never persisted.
- **Sub-agents get their own identity automatically.** A parent agent
  that spawns a sub-agent (via `azureclaw spawn` or AGT mesh) yields a
  new `KarsSandbox` Custom Resource, which the controller reconciles
  into a new agent identity. Same reconcile path for every sandbox —
  there is no special-case code.

This guide covers the day-1 user flow. For the architecture and the
token-acquisition mechanics, see
[`docs/architecture/entra-agent-id/`](architecture/entra-agent-id/).

---

## Prerequisites

| Role | Where | Why |
|------|-------|-----|
| `Contributor` | Subscription scope | Create AKS, ACR, KV, Foundry, MI |
| `User Access Administrator` | Subscription scope | Assign Foundry RBAC to agent identities |
| **`Agent ID Developer`** | Entra directory (tenant) | Create the blueprint + per-sandbox agent identities |

The first two are the standard `kars up` baseline. The third is what
**unlocks per-sandbox identity**; without it `kars up` still succeeds
but the cluster runs in AGT anonymous tier. Activate `Agent ID
Developer` via PIM at <https://portal.azure.com> → Privileged Identity
Management → My roles → Microsoft Entra roles.

`kars up` runs a preflight check and warns clearly if the role is
missing — you don't have to remember.

---

## Day-1: deploy a fresh cluster

```bash
# Sign in once.
az login --tenant <your-tenant>

# Deploy.
kars up --name prod-agent --location swedencentral

# Microsoft-corp users (and any tenant that requires ServiceTree):
kars up --name prod-agent --location swedencentral --service-tree <guid>
# or
export KARS_SERVICE_TREE=<guid>
kars up --name prod-agent --location swedencentral
```

`kars up` is idempotent. Re-running on the same cluster is safe.

What happens for Entra Agent ID, transparently:

1. **Preflight.** Confirms you hold `Agent ID Developer` (or stronger).
2. **Blueprint.** If the tenant already has a `kars-blueprint`
   application, kars reuses it. Otherwise it creates one via Microsoft
   Graph and registers its service principal so it appears in the
   Entra Agents portal.
3. **Controller MI.** A user-assigned managed identity in your
   subscription, scoped to this cluster, gets created.
4. **Federation.** The controller MI is added as a federated identity
   credential on the blueprint. The federation issuer is
   `https://login.microsoftonline.com/<tenant>/v2.0` (universally
   allow-listed; no tenant-admin action required).
5. **Cluster anchor.** A `KarsAuthConfig/default` Custom Resource is
   written to the cluster. The controller materialises a sibling
   ConfigMap with the sidecar environment variables.

Subsequent steps proceed as before: bicep, helm, sandbox creation. By
the time `kars up` returns, your first sandbox is up and Foundry-side
audit logs already record the agent identity by name.

---

## Day-2: list, inspect, grant

### See the agent identity for a sandbox

```bash
kubectl get karssandbox prod-agent -o yaml | yq .status.agentIdentity
```

Outputs:

```yaml
appId:       a8e0eff0-1fe0-4b46-aba3-d7fa7a1c2ecd
objectId:    a8e0eff0-1fe0-4b46-aba3-d7fa7a1c2ecd
displayName: kars-prod-prod-agent
createdAt:   "2026-05-27T11:22:48Z"
```

### Grant Foundry access

```bash
# Built-in alias
kars policy grant prod-agent foundry-user

# Or raw ARM
az role assignment create \
  --assignee-object-id <agentIdentity.objectId> \
  --assignee-principal-type ServicePrincipal \
  --role "Cognitive Services User" \
  --scope <foundry-resource-id>
```

The role takes 10-30 minutes to fully propagate in Foundry's
data-plane RBAC cache. The first call after grant may still 403; the
second will succeed.

### Revoke

```bash
kars policy revoke prod-agent foundry-user
# or
az role assignment delete --assignee <agentIdentity.objectId> --scope <foundry-resource-id>
```

### Audit

Microsoft Entra portal → **Identity > Monitoring > Sign-in logs >
Service principal sign-ins**, filter by `kars-prod-prod-agent`. Every
Foundry / Graph call attributed to the agent appears here.

---

## Spawning sub-agents

When a kars agent spawns another (`azureclaw spawn` or AGT
`mesh_send`), the spawned sandbox is a separate `KarsSandbox` CR with
its own name. The controller reconciles it like any other: a new agent
identity is created, the sidecar is wired up, RBAC is assigned
independently.

For the parent agent, **no special handling is required**. There is
no shared identity between parent and child — they are two separate
Entra principals from Foundry's perspective. This means:

- Sub-agent permissions can be **narrower** than the parent's. The
  parent can spawn a "search" sub-agent that only has read access to
  one Foundry deployment.
- Sub-agent audit trails are **separable**. Foundry sign-in logs let
  you filter for `kars-*-search-*` and see exactly what the
  search-class agents did.
- Sub-agent cleanup is **automatic**. When the parent's CR is
  deleted, sub-agents created from it are typically deleted in the
  same reconcile pass; their agent identities are then reaped by the
  controller's finalizer.

The `kars handoff` command (see [`handoff.md`](handoff.md)) uses the
same machinery — handoff'd sandboxes receive distinct agent
identities and their permission set is migrated separately.

---

## When does the agent identity get created?

**Lazily, on the first reconcile of the `KarsSandbox` CR.** Not at
`kars up` invocation, not at any global "warm pool" of pre-minted
identities.

```text
User runs: kars up --name prod-agent
   ↓
kubectl apply -f KarsSandbox CR (controller takes over from here)
   ↓
Reconciler reads spec.meshAuth.mode (Auto -> Agent ID since KarsAuthConfig is ready)
   ↓
Reconciler reads status.agentIdentity (empty on first reconcile)
   ↓
agent_identity.create_agent_identity() ->
   IMDS controller MI token (audience = api://AzureADTokenExchange)
   -> Entra /token exchange (jwt-bearer, audience = Graph)
   -> blueprint Graph token
   -> POST /beta/servicePrincipals/Microsoft.Graph.AgentIdentity
   -> new agent identity service principal
   ↓
Reconciler writes status.agentIdentity
   ↓
Reconciler renders pod spec with the auth-sidecar container
   ↓
Pod starts. Sidecar mints downstream tokens for that agent identity on demand.
```

Token TTLs:

- **Blueprint token** (Graph): 1h, refreshed by MSAL on next sidecar
  call when within 5 min of expiry.
- **Per-agent-identity token** (downstream): 1h, refreshed the same
  way.

The sandbox code never sees this. Every call out hits the sidecar,
which always returns a valid Bearer header. There is **no refresh
thread** to manage.

---

## Tearing it down

```bash
kubectl delete karssandbox prod-agent
# or
kars destroy prod-agent
```

The controller's finalizer deletes the agent identity service
principal via Microsoft Graph **before** the K8s CR is fully removed.
Any RBAC assignments you made via `kars policy grant` need to be
removed manually if you want a fully clean Entra tenant (the
finalizer does not delete role assignments — that is by design, so a
`kars destroy` accident cannot revoke unrelated grants).

To wipe the entire blueprint (and all derived agent identities that
share it):

```bash
# Caution: irreversible. Removes the blueprint application, its SP,
# and all `KarsSandbox` agent identities derived from it across every
# cluster using this tenant trust anchor.
kars mesh setup-trust --uninstall
```

---

## Troubleshooting

### "Entra Agent ID setup skipped" warning during `kars up`

The signed-in user lacks the `Agent ID Developer` role. The cluster
continues in anonymous tier. Activate the role through PIM and re-run
`kars up` — the auth provisioning step is idempotent and only runs if
`KarsAuthConfig/default` does not already exist.

### Foundry call returns 401 with the agent identity in the error

The role assignment hasn't propagated yet. Wait 30 seconds and retry.
If it persists past 30 minutes, check:

```bash
# Confirm the role is actually assigned at the right scope:
az role assignment list \
  --assignee <agentIdentity.objectId> \
  --scope <foundry-resource-id> \
  -o table
```

### `kubectl get karsauthconfig` returns NotFound

The controller hasn't installed the CRD yet. This usually means
`kars up` aborted before the Helm phase. Run `kars up` again — it
will resume from the last completed phase.

### `kars up` says "Sandbox does not yet have an agent identity"

The reconciler hasn't run yet. Wait ~30 seconds; the status is
populated on the first reconcile. If it stays empty for more than
2 minutes, check controller logs:

```bash
kubectl logs -n kars-system deploy/kars-controller | grep agent_identity
```

Most likely causes:

- The controller MI is not assigned to the AKS node pool VMSS.
  Run `az vmss identity show -g <node-rg> -n <vmss-name>` and ensure
  the controller MI is listed.
- The blueprint's federated identity credential is missing or has
  the wrong subject. Confirm
  `az identity show -g <ridg> -n kars-<cluster>-controller-mi --query principalId`
  matches the FIC subject on the blueprint.

---

## See also

- [`docs/permissions.md`](permissions.md) — full permission matrix
  and tenant-level prerequisites
- [`docs/architecture/entra-agent-id/01-runtime-token-flow.md`](architecture/entra-agent-id/01-runtime-token-flow.md)
  — the runtime token-acquisition flow
- [`docs/architecture/entra-agent-id/README.md`](architecture/entra-agent-id/README.md)
  — POC findings and design rationale
