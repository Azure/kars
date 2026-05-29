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
  that spawns a sub-agent (via `kars_spawn` or AGT mesh) yields a
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
**unlocks per-sandbox identity** when you opt in with
`--mesh-trust=entra`. Without the flag, `kars up` runs in anonymous
mode (shared cluster Workload Identity for Foundry, anonymous AGT
mesh tier) and the role check is skipped. With the flag and without
the role, `kars up` fails preflight with a clear error. Activate
`Agent ID Developer` via PIM at <https://portal.azure.com> →
Privileged Identity Management → My roles → Microsoft Entra roles.

`kars up --mesh-trust=entra` runs a preflight check and warns
clearly if the role is missing — you don't have to remember.

---

## Day-1: deploy a fresh cluster

```bash
# Sign in once.
az login --tenant <your-tenant>

# Deploy with per-sandbox Entra Agent ID (opt in via --mesh-trust=entra).
kars up --name prod-agent --location swedencentral --mesh-trust=entra

# Anonymous mode (default) — shared cluster MI, no Entra prerequisites.
kars up --name prod-agent --location swedencentral

# Microsoft-corp users (and any tenant that requires ServiceTree):
kars up --name prod-agent --location swedencentral --mesh-trust=entra --service-tree <guid>
# or
export KARS_SERVICE_TREE=<guid>
kars up --name prod-agent --location swedencentral --mesh-trust=entra
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

When a kars agent spawns another (`kars_spawn` or AGT
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

### `az cli ca block` — "Could not enumerate directory roles (AADSTS530084)" {#az-cli-ca-block}

The Azure CLI token cache is being blocked by Conditional Access
token-binding policy when calling Microsoft Graph. This is the most
common failure mode in Microsoft-corporate (and similarly-policed
enterprise) tenants — the CLI's first-party app needs explicit
Graph-scope consent for each session.

**The kars CLI auto-handles two common variants:**

| Code | Meaning | What kars CLI does |
|---|---|---|
| `AADSTS530084` | Token-binding policy on the az CLI's Graph token | Auto-runs `az login --use-device-code --scope https://graph.microsoft.com//.default` and retries |
| `AADSTS65001` / `AADSTS65002` | Missing first-party Graph consent | Same auto-retry |

If both the interactive and device-code flows fail (typically with
`AADSTS530033` — "device must be Intune-managed"), kars falls back
to the **Bicep ARM path** which uses a different auth surface
(`Microsoft.Graph` extension via the ARM deployment principal).
The Bicep path produces a functional but **untyped** blueprint
(tag-based detection only — visible under "App registrations" but
not under the "Agents" portal page).

### When even Bicep cannot produce the typed blueprint

If you need the blueprint to appear under
**Entra portal → Identity → Agents → Agent identity blueprints**
(and your terminal cannot reach Microsoft Graph at all), the
workaround is:

1. Let `kars up` / `kars mesh setup-trust` provision via Bicep —
   you get a working blueprint + controller MI + KarsAuthConfig CR.
2. Open https://developer.microsoft.com/en-us/graph/graph-explorer
   in a browser (Graph Explorer uses a different first-party app
   `de8bc8b5-...` which is typically not subject to the same CA
   token-binding policy as the Azure CLI).
3. Convert the existing untyped App to a typed `AgentIdentityBlueprint`
   in place. **Include `sponsors` and `owners`** — empirically the
   Entra Agents portal filter requires both before the blueprint
   becomes visible under the Agents page (a minimal `@odata.type`-only
   PATCH does the type upgrade but the entry stays hidden):

   ```http
   PATCH https://graph.microsoft.com/beta/applications/<blueprintObjectId>
   Content-Type: application/json

   {
     "@odata.type": "Microsoft.Graph.AgentIdentityBlueprint",
     "sponsors@odata.bind": [
       "https://graph.microsoft.com/v1.0/users/<YOUR_USER_OID>"
     ],
     "owners@odata.bind": [
       "https://graph.microsoft.com/v1.0/users/<YOUR_USER_OID>"
     ]
   }
   ```

   - `blueprintObjectId` is the value from
     `kubectl get karsauthconfig default -o jsonpath='{.spec.agentId.blueprintObjectId}'`
   - `<YOUR_USER_OID>` is your Entra user objectId — find it at
     Entra admin center → Users → (your account) → Object ID
   - Note the `@odata.type` value has **no `#` prefix** in the request
     body; Graph returns it with the `#` and lowercase `microsoft.graph.`
     prefix on subsequent GETs (both forms are accepted on input)
   - The `@odata.bind` URLs must use **`/v1.0/users/`** specifically
     even though the parent PATCH is to `/beta/applications/` — Graph
     rejects `/beta/users/` as a bind target

   The `kars mesh setup-trust --mode bicep` driver prints this exact
   body (with your OID auto-filled when discoverable) at the end of
   the Bicep flow — copy-paste it directly from the CLI output.

   If Graph rejects the in-place PATCH, you must delete + recreate the
   blueprint as typed. **Important:** a fresh Graph Explorer
   `POST /applications` creates **only the app** — neither the SP
   nor the FIC are auto-created (Bicep does all three; the Graph
   Explorer fallback does step 1 only). Full recovery sequence:

   ```http
   # 1. Create the typed blueprint app
   POST https://graph.microsoft.com/beta/applications
   Content-Type: application/json

   {
     "@odata.type": "Microsoft.Graph.AgentIdentityBlueprint",
     "displayName": "kars-blueprint",
     "sponsors@odata.bind": [
       "https://graph.microsoft.com/v1.0/users/<YOUR_USER_OID>"
     ],
     "owners@odata.bind": [
       "https://graph.microsoft.com/v1.0/users/<YOUR_USER_OID>"
     ]
   }
   # Response includes `id` (the new objectId) and `appId` — save both.
   ```

   ```http
   # 2. Create the SP for the new app (required for RBAC + portal listing)
   POST https://graph.microsoft.com/v1.0/servicePrincipals
   Content-Type: application/json

   { "appId": "<NEW_APP_ID_FROM_STEP_1>" }
   ```

   ```http
   # 3. Recreate the MI-as-FIC on the new app
   POST https://graph.microsoft.com/v1.0/applications/<NEW_OBJECT_ID>/federatedIdentityCredentials
   Content-Type: application/json

   {
     "name": "kars-controller-mi-fic",
     "issuer": "https://login.microsoftonline.com/<TENANT_ID>/v2.0",
     "subject": "<MI_PRINCIPAL_ID>",
     "audiences": ["api://AzureADTokenExchange"]
   }
   ```

   Tenant ID + MI principalId come from `kubectl get karsauthconfig
   default -o jsonpath='{.spec.tenant.tenantId}{"\n"}{.spec.controller.managedIdentityPrincipalId}{"\n"}'`.

   ```bash
   # 4. Re-point the cluster CR at the new blueprint
   kubectl patch karsauthconfig default --type=merge \
     -p '{"spec":{"agentId":{"blueprintClientId":"<NEW>","blueprintObjectId":"<NEW>"}}}'

   # 5. Optional: delete the old orphan app (cascades to its SP)
   #    DELETE https://graph.microsoft.com/v1.0/applications/<OLD_OBJECT_ID>
   ```

4. Long-term: get the workstation Intune-enrolled (`aka.ms/intune`).
   After enrollment the `AADSTS530033` block clears and the
   imperative kars CLI path Just Works without browser hops.

The kars **runtime** does not depend on the typed-vs-untyped
distinction — controller, sidecar, RBAC chain all key off `appId`.
The typed form is purely for portal categorisation (and for tenant
admins who scope blueprint governance via the Agents page).

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
