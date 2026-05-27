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
  "Name": "Kars Deployer",
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
az role assignment create --assignee "$USER" --role "Kars Deployer" --scope "/subscriptions/$SUB"
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

Kars's inter-agent mesh (AGT) authenticates agents to the AgentMesh
relay using an Entra-issued token for the scope `api://agentmesh/.default`.
**Registering that scope requires a tenant administrator** (Global
Administrator or Application Administrator). Kars does **not** create
the app registration automatically.

If nobody has provisioned the `api://agentmesh` Entra application in your
tenant, sandboxes still come up — they fall back to the **AGT anonymous
tier**, which works for dev/test but not for production tenant-isolated
workloads.

The fastest fix is the Kars CLI helper, which is idempotent and
prints the tenant/client IDs when it's done:

```bash
# Requires Application Administrator or Global Admin
kars mesh setup-trust
```

If you'd rather run the underlying `az` calls directly:

```bash
# Requires Application Administrator or Global Admin
az ad app create --display-name "AgentMesh" --identifier-uris "api://agentmesh"
APP_ID=$(az ad app list --display-name AgentMesh --query "[0].appId" -o tsv)
az ad sp create --id "$APP_ID"
```

---

## Common failure modes

| Error | Cause | Fix |
|-------|-------|-----|
| `AuthorizationFailed` on AKS attach-ACR | Missing `Microsoft.Authorization/roleAssignments/write` | Grant `User Access Administrator` at sub scope |
| `FeatureNotRegistered` during Bicep | `EncryptionAtHost` not propagated yet | Wait 5–15 min after `az feature register`, then retry |
| `SubscriptionNotRegistered` for Microsoft.ContainerService | Locked-down sub blocks auto-registration | `az provider register -n Microsoft.ContainerService` |
| `ResourceQuotaExceeded` for VM cores | Regional vCPU quota | Request quota increase or use `--region` to pick a different region |
| Sandboxes come up but AGT messages fail auth | `api://agentmesh` Entra app not registered | Ask tenant admin (see above) — sandboxes fall back to anonymous tier |
| `AADSTS500011` in router logs | Same as above | Tenant admin registers `api://agentmesh` |

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
