# Azure permissions required for `kars up`

`kars up` provisions a complete secure-by-default AKS runtime: cluster,
ACR, Key Vault, Log Analytics, Azure AI Foundry project, Workload Identity,
role assignments, network rules, Helm charts, and the `KarsSandbox` CRD.

End-to-end the flow takes **15–25 minutes**. If the caller lacks the right
RBAC, the flow fails halfway through with a cryptic `AuthorizationFailed`
error. This document enumerates exactly what's required so you can either
ask your subscription Owner for the right roles up front, or hand them this
page.

The CLI also ships a preflight check (run automatically at the start of
`kars up`) that queries your effective permissions and fails fast in
under 30 seconds if anything is missing. You can bypass it with
`--skip-preflight` if you know better.

---

## TL;DR — grant these two roles

At **subscription** scope, for the user (or service principal) running
`kars up`:

```bash
SUB=$(az account show --query id -o tsv)
USER=$(az account show --query user.name -o tsv)

az role assignment create --assignee "$USER" --role "Contributor"                 --scope "/subscriptions/$SUB"
az role assignment create --assignee "$USER" --role "User Access Administrator"   --scope "/subscriptions/$SUB"
```

Alternatively, `Owner` at subscription scope covers both, but it violates
least-privilege. If you're running a one-off bootstrap, `Owner` scoped to
the target **resource group** (after you create it manually) is also
acceptable.

---

## Why two roles?

| Role | Actions it grants | What `kars up` needs it for |
|------|-------------------|----------------------------------|
| **Contributor** | `*` except `Microsoft.Authorization/*/Write`, `*/Delete` | Create AKS, ACR, KV, Log Analytics, Foundry, VNet, Workload Identity, Bicep deployments |
| **User Access Administrator** | `Microsoft.Authorization/*` | `az aks update --attach-acr` (kubelet↔ACR role assignment), federated-credential creation, Workload Identity → Foundry role grants |

`Contributor` alone is **not enough** — AKS↔ACR attachment creates a role
assignment between the AKS kubelet identity and the ACR, and that requires
`Microsoft.Authorization/roleAssignments/write`.

---

## Full required actions matrix

These are the individual control-plane actions the preflight checks against.
Most are bundled inside `Contributor` + `User Access Administrator`; listed
here so you can build a **custom role** if you need tighter least-privilege.

| Action | Purpose |
|--------|---------|
| `Microsoft.Resources/subscriptions/resourceGroups/write` | Create the target resource group |
| `Microsoft.Resources/deployments/write` | Run the Bicep deployment |
| `Microsoft.ContainerService/managedClusters/write` | Provision AKS cluster |
| `Microsoft.ContainerService/managedClusters/listClusterUserCredential/action` | `az aks get-credentials` |
| `Microsoft.ContainerRegistry/registries/write` | Provision ACR |
| `Microsoft.ContainerRegistry/registries/importImage/action` | Import sandbox/controller/router images |
| `Microsoft.KeyVault/vaults/write` | Provision Key Vault for sandbox secrets |
| `Microsoft.ManagedIdentity/userAssignedIdentities/write` | Create Workload Identity |
| `Microsoft.ManagedIdentity/userAssignedIdentities/federatedIdentityCredentials/write` | Federate each sandbox ServiceAccount |
| `Microsoft.OperationalInsights/workspaces/write` | Provision Log Analytics |
| `Microsoft.Authorization/roleAssignments/write` | Attach ACR to AKS, grant Workload Identity scoped RBAC |
| `Microsoft.Network/virtualNetworks/write` | AKS VNet (if Bicep creates one) |
| `Microsoft.CognitiveServices/accounts/write` | Provision Azure AI Foundry project (skip if you pass `--foundry-endpoint`) |
| `Microsoft.Features/providers/features/register/action` | Register `EncryptionAtHost`, `KataVMIsolationPreview` |
| `Microsoft.Network/loadBalancers/write` | Required when `a2aGateway.enabled: true` in Helm values — provisions the A2A public ingress `LoadBalancer` service |

### Custom role JSON (copy-paste)

```jsonc
{
  "Name": "kars Deployer",
  "Description": "Minimal role to run 'kars up' end-to-end",
  "IsCustom": true,
  "Actions": [
    "Microsoft.Resources/subscriptions/resourceGroups/write",
    "Microsoft.Resources/subscriptions/resourceGroups/read",
    "Microsoft.Resources/deployments/*",
    "Microsoft.ContainerService/managedClusters/*",
    "Microsoft.ContainerRegistry/registries/*",
    "Microsoft.KeyVault/vaults/*",
    "Microsoft.ManagedIdentity/userAssignedIdentities/*",
    "Microsoft.OperationalInsights/workspaces/*",
    "Microsoft.Insights/*",
    "Microsoft.Authorization/roleAssignments/write",
    "Microsoft.Authorization/roleAssignments/read",
    "Microsoft.Authorization/roleAssignments/delete",
    "Microsoft.Authorization/roleDefinitions/read",
    "Microsoft.Network/virtualNetworks/*",
    "Microsoft.Network/networkSecurityGroups/*",
    "Microsoft.Network/privateEndpoints/*",
    "Microsoft.CognitiveServices/accounts/*",
    "Microsoft.Features/providers/features/register/action",
    "Microsoft.Features/providers/features/read",
    "Microsoft.Features/features/read"
  ],
  "NotActions": [],
  "AssignableScopes": ["/subscriptions/<YOUR_SUB_ID>"]
}
```

Assign with:

```bash
az role definition create --role-definition ./kars-deployer.json
az role assignment create --assignee "$USER" --role "kars Deployer" --scope "/subscriptions/$SUB"
```

> **Note:** When `a2aGateway.enabled: true` in Helm values, cert-manager (≥ 1.14)
> must be installed in the cluster before deploying — the chart creates a
> `Certificate` CR for TLS provisioning. cert-manager is not deployed by
> `kars up`; install it independently with
> `helm install cert-manager jetstack/cert-manager --set installCRDs=true`.

---

## Resource providers

The preflight checks these are `Registered`. If any are `NotRegistered`, it
prints the exact `az provider register` command. Most subscriptions
auto-register on first use; locked-down subs block that.

- `Microsoft.ContainerService`
- `Microsoft.ContainerRegistry`
- `Microsoft.KeyVault`
- `Microsoft.ManagedIdentity`
- `Microsoft.OperationalInsights`
- `Microsoft.Insights`
- `Microsoft.Network`
- `Microsoft.Compute`
- `Microsoft.Authorization`
- `Microsoft.CognitiveServices` *(only if provisioning a new Foundry)*

Bulk-register:

```bash
for ns in Microsoft.ContainerService Microsoft.ContainerRegistry Microsoft.KeyVault \
          Microsoft.ManagedIdentity Microsoft.OperationalInsights Microsoft.Insights \
          Microsoft.Network Microsoft.Compute Microsoft.Authorization Microsoft.CognitiveServices; do
  az provider register -n "$ns"
done
```

---

## Preview features

| Feature | Required when |
|---------|---------------|
| `Microsoft.Compute/EncryptionAtHost` | Always — `clawpool` VMs use encryption-at-host |
| `Microsoft.ContainerService/KataVMIsolationPreview` | Only with `--isolation confidential` |

`kars up` attempts to register these automatically. **Feature
propagation takes 5–15 minutes** — if your tenant hasn't registered them
before, the preflight will print a warning and you'll want to register
them ahead of time:

```bash
az feature register --namespace Microsoft.Compute         --name EncryptionAtHost
az feature register --namespace Microsoft.ContainerService --name KataVMIsolationPreview   # only for confidential
```

Track progress with `az feature show`. Once both show `Registered`, re-run
the provider refresh:

```bash
az provider register -n Microsoft.Compute
az provider register -n Microsoft.ContainerService
```

---

## Tenant-level (Entra ID) considerations

kars per-sandbox identity uses **Microsoft Entra Agent ID** (GA, May 2026).
Each kars sandbox calls Foundry / Graph / Key Vault as its own Entra
agent identity (`kars-<cluster>-<sandbox>`), and Foundry's audit logs
attribute every call by name. This replaces the legacy
`api://agentmesh` app-registration pattern.

### What the operator needs

The user running `kars up` must hold one of the following Entra
directory roles at tenant scope:

| Role | Sufficiency |
|------|-------------|
| **Agent ID Developer** | ✅ Recommended — narrowest scope that works |
| Agent ID Administrator | ✅ Stronger than required |
| Privileged Role Administrator | ✅ Stronger than required |
| Global Administrator | ✅ Stronger than required |

Most Microsoft-corporate users self-elevate via PIM at
<https://portal.azure.com> → Privileged Identity Management → My
roles. External tenants typically grant the role through Entra
portal → Identity → Roles & admins.

### What `kars up` does automatically

When you run `kars up` for the first time on a tenant, the CLI:

1. Detects whether a `KarsAuthConfig/default` resource already exists
   in the cluster (idempotent — skipped on re-runs).
2. Creates the **agent identity blueprint** via Microsoft Graph
   (`POST /v1.0/applications/` with
   `@odata.type=#Microsoft.Graph.AgentIdentityBlueprint`).
3. Creates the blueprint service principal so it appears in the Entra
   Agents portal (<https://entra.microsoft.com> → Agents → Agent
   identities) and agent identities can be derived from it.
4. Creates a per-cluster **controller managed identity** in your
   subscription.
5. Adds a federated identity credential on the blueprint that trusts
   the controller MI's IMDS-issued token (issuer
   `https://login.microsoftonline.com/<tenant>/v2.0`). This is the
   anti-loop-safe credential path — see
   [`docs/architecture/entra-agent-id/`](./architecture/entra-agent-id/).
6. Writes the `KarsAuthConfig/default` Custom Resource. The
   controller's auth-config reconciler materialises a sibling
   ConfigMap with the sidecar environment variables every kars
   sandbox pod consumes.

After that, every `KarsSandbox` CR — whether created by `kars up`,
`kars handoff`, or sub-agent spawning — automatically gets its own
agent identity provisioned by the controller during reconcile.

### Microsoft-corporate (and similarly-policed) tenants

The Microsoft corporate tenant policy `538f1913-…` requires a
`serviceManagementReference` GUID on every new Entra application.
Pass it through `--service-tree`:

```bash
kars up --service-tree 1c826d4f-22b0-4c67-b755-778a05d7ffc9
# or via env var:
export KARS_SERVICE_TREE=1c826d4f-22b0-4c67-b755-778a05d7ffc9
kars up
```

Non-Microsoft tenants leave it empty — the field is omitted from
the Graph create call when unset.

### Anonymous-tier fallback

If the user running `kars up` does **not** hold the Agent ID
Developer role, the auto-provisioning step fails (non-fatal) and the
cluster comes up in the **AGT anonymous tier**: sandboxes start
successfully but cannot call Foundry / Graph as a named principal.
Once the role is granted, run `kars mesh setup-trust` (or `kars up`
again on the same cluster — both are idempotent) to complete
provisioning.

### Provisioning a blueprint manually (rare)

If you want to provision the blueprint outside of `kars up` (e.g.
in your IT-managed tenant pipeline), this is the minimal Graph
sequence — see
[`docs/architecture/entra-agent-id/01-runtime-token-flow.md`](./architecture/entra-agent-id/01-runtime-token-flow.md)
for the full chain:

```bash
# Requires Agent ID Developer (or stronger)
az rest --method POST \
  --url "https://graph.microsoft.com/beta/applications/" \
  --headers OData-Version=4.0 \
  --body '{
    "@odata.type": "#Microsoft.Graph.AgentIdentityBlueprint",
    "displayName": "kars-blueprint",
    "sponsors@odata.bind": ["https://graph.microsoft.com/beta/users/<your-oid>"],
    "owners@odata.bind":   ["https://graph.microsoft.com/beta/users/<your-oid>"]
  }'
# Then create the SP so it shows up in the Entra Agents portal:
APP_ID=...  # appId from the previous response
az rest --method POST \
  --url "https://graph.microsoft.com/beta/servicePrincipals" \
  --body "{\"appId\": \"$APP_ID\"}"
```

---

## Common failure modes

| Error | Cause | Fix |
|-------|-------|-----|
| `AuthorizationFailed` on AKS attach-ACR | Missing `Microsoft.Authorization/roleAssignments/write` | Grant `User Access Administrator` at sub scope |
| `FeatureNotRegistered` during Bicep | `EncryptionAtHost` not propagated yet | Wait 5–15 min after `az feature register`, then retry |
| `SubscriptionNotRegistered` for Microsoft.ContainerService | Locked-down sub blocks auto-registration | `az provider register -n Microsoft.ContainerService` |
| `ResourceQuotaExceeded` for VM cores | Regional vCPU quota | Request quota increase or use `--region` to pick a different region |
| `kars up` warns "Entra Agent ID setup skipped" | Signed-in user lacks Agent ID Developer role | Activate the role via PIM, then re-run `kars up` |
| `CredentialInvalidLifetimeAsPerAppPolicy` during blueprint create | Microsoft-corporate tenant policy blocks Application credential creation | This indicates a misconfigured tenant policy; only FIC-based credentials work — `kars up` uses MI-as-FIC which is allow-listed by default |
| `InvalidFederatedIdentityCredentialValue` on blueprint FIC | Tenant blocks the OIDC issuer | The issuer used by kars is `login.microsoftonline.com/<tenant>/v2.0`, which is universally allow-listed. Report this as a kars bug. |
| Sandbox tokens have `appid` matching the controller MI rather than the agent identity | Reconciler did not yet inject the sidecar | Pod-spec sidecar injection lands in the follow-up PR after the auth foundation PR. Update kars and re-create the sandbox. |

---

## Running the preflight manually

```bash
# The preflight runs automatically at the start of `kars up`. To run
# just the checks without deploying, pair it with --dry-run:
kars up --dry-run

# To bypass (e.g. cross-tenant guests where the permissions API returns
# misleading results, or CI systems using a federated identity where the
# effective-permissions API doesn't reflect reality):
kars up --skip-preflight
```
