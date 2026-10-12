<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Per-sandbox identity (Entra Agent ID)

Kars supports a **Microsoft Entra Agent ID per sandbox** when the
reviewed identity configuration is active and provisioning succeeds.
This is not the default fresh-install identity model. In Agent ID mode,
the inference router pins the sandbox's identity and requests tokens from
the shared Entra SDK auth-sidecar in `kars-system`; the sidecar is not a
container in each sandbox Pod.

- **Audit attribution is per principal.** Verify the sandbox's recorded
  identity against actual downstream sign-in logs; configuration alone
  does not establish successful authentication.
- **RBAC is per-sandbox.** Grants can target one agent without granting
  access to others. `kars policy grant` wraps role assignment operations.
- **Federated credentials avoid client secrets.** Tokens are acquired on
  demand; federation, consent, token exchange and downstream authorization
  must all work for calls to succeed.
- **Sub-agents use the same reconcile path.** A separately created
  `KarsSandbox` can receive its own identity under the active configuration;
  creation, grants and cleanup are not guaranteed by `kars up` completing.

**Qualification boundary:** the current Helm identity activation has local
render coverage, not installed lifecycle or token-exchange qualification.
Mesh template checks and healthy Pods do not prove accepted Entra tokens,
verified membership, or the Core + Bridge beta journey. Follow the
[canonical Core Helm identity instructions](../deploy/helm/kars/README.md#helm-owned-entra-identity-configuration).
For token-acquisition mechanics, see
[`docs/architecture/entra-agent-id/`](architecture/entra-agent-id/).

---

## Prerequisites

| Role | Where | Why |
|------|-------|-----|
| `Contributor` | Subscription scope | Create AKS, ACR, KV, Foundry, MI |
| `User Access Administrator` | Subscription scope | Assign Foundry RBAC to agent identities |
| **`Agent ID Developer`** | Entra directory (tenant) | Create the blueprint + per-sandbox agent identities |

The first two are the Azure provisioning/RBAC baseline. Tenant identity
provisioning is a separate administrator-reviewed operation; having an
`Agent ID Developer` role does not itself establish federation, Graph consent
or downstream grants. Where applicable, activate the role through PIM at
<https://portal.azure.com> → Privileged Identity Management → My roles →
Microsoft Entra roles.

Neither ordinary `kars up` nor `kars up --upgrade` provisions tenant or
Blueprint prerequisites, nor raw-applies `KarsAuthConfig`. `--mesh-trust=entra`
requires an existing, reviewed **same-Core-Helm-release** activation, not just
a directory role.
The default `--mesh-trust=anonymous` allows a fresh anonymous mesh but
**preserves installed Entra trust**; it is not a downgrade request or an
error fallback.

---

## Day-1: establish Core, then activate identity

1. **Establish Core with `entraSidecar.enabled=false`.** A fresh
   `kars up --name prod-agent --region swedencentral` uses the default
   anonymous mesh and the cluster's shared Workload Identity path, not
   per-sandbox Agent ID. Requesting `--mesh-trust=entra` before an existing
   Core activation is present is refused.
2. **Review existing tenant prerequisites.** Reuse the intended Entra
   Agent ID Blueprint, its typed BlueprintPrincipal, and controller-associated
   shared SDK sidecar architecture. Do not replace the trust anchor to work
   around a CLI error. The chart does not provision managed identities,
   Blueprint federation, Graph consent or per-agent downstream grants.
3. **Prepare complete reviewed Core and identity values.** Verify the live
   `KarsAuthConfig` CRD is established and matches the supported schema.
   Set `entraSidecar.enabled=true`, canonical `entraSidecar.authConfig`, and
   `schemaHook.enabled=false`. Choose an explicit sidecar credential mode:
   - `ManagedIdentityImds`: the configured managed identity must be available
     on sidecar nodes, with reachable IMDS and reviewed Blueprint federation.
   - `WorkloadIdentity`: Blueprint federation must match the cluster OIDC
     issuer, audience and subject
     `system:serviceaccount:kars-system:entra-auth-sidecar`; the webhook and
     token projection must work.

   Sidecar Workload Identity is **independent** of the controller's
   `azure.workloadIdentity` setting and its identity-provisioning credential
   path. The controller provisioner still needs its managed identity and
   MI-to-Blueprint Graph exchange; sidecar Workload Identity does not replace
   those prerequisites.
4. **Activate in the same Core release.** Follow the
   [canonical activation and ownership procedure](../deploy/helm/kars/README.md#explicit-activation-in-the-same-core-release),
   using complete reviewed values rather than raw-applying or patching a
   Helm-owned `KarsAuthConfig/default`. Existing ownership/spec-hash conflicts
   require explicit review, not forced adoption. For managed Entra mesh, also
   review `agentMesh.enabled=true`, `meshAuthBackend=EntraAgentIdentity` and the
   [resource audience, tenant, v2-token and per-agent grant requirements](../deploy/helm/kars/README.md#managed-agentmesh-entra-verifier).
5. **Require the activated mode on subsequent `up` runs:**

   ```bash
   # Only after reviewed Entra activation in the existing Core release:
   kars up --name prod-agent --region swedencentral --mesh-trust=entra
   # Or use the existing cached deployment target:
   kars up --upgrade --mesh-trust=entra
   ```

   Both paths preserve installed values and compare the release's literal
   audience/tenant pair with **both** live registry and relay Pod templates,
   checking readiness. They do not patch verifier settings after startup or
   prove accepted tokens. `--mesh-trust=entra` with `--global-registry` is
   refused because `up` cannot configure or attest external trust.

`--service-tree` and `--expose-registry` are **temporarily refused by `up`**,
not permanently removed onboarding capabilities. Handle tenant prerequisites
and DNS/TLS/ingress ownership through reviewed procedures. `KARS_SERVICE_TREE`
does not turn current `up` into a Blueprint provisioning command.

### What Workload Identity verification establishes

Ordinary and fast `up` share `cli/src/lib/workload-identity.ts`. Against the
explicitly selected subscription, it verifies the intended ARM user-assigned
managed identity and principal, ensures controller/sandbox federated identity
credentials with the exact issuer, service-account subject and sole audience
`api://AzureADTokenExchange`, and ensures **Managed Identity Contributor on
that same identity's resource scope** for its principal. Missing entries are
created; conflicts and malformed inventories fail rather than being silently
accepted. Credential and role readback, plus a final principal check, are
required for success.

Ordinary `up` runs this check before creating the sandbox CR. Fast upgrade
checks the current sandbox inventory after Helm and workload restarts when
federation is configured; a failed ARM check does not undo those changes.
Partial or malformed cached federation configuration is refused before
connecting. An actual fast upgrade also requires a cached subscription and
will not use the ambient Azure account. If it is missing, run ordinary
`kars up` to select and save the intended
subscription. These checks establish ARM configuration only: **not token
exchange, propagation, Foundry access, or an Agent ID lifecycle**.

### Separate legacy `mesh setup-trust` provisioning

`kars mesh setup-trust` remains a separate imperative command. Its default
`--mode agent-id` uses the Graph provisioning helper, `--mode bicep` uses the
ARM/Graph-extension path, and `--mode legacy` creates the deprecated
`api://agentmesh` resource app. These are not current `up`/Helm activation
steps or hidden prerequisites. The Agent ID/Bicep paths can raw-apply an
AuthConfig and skip work when one exists; existence is not qualification or
permission to overwrite a Helm-owned singleton. Preserve and review an
existing Blueprint/configuration before any migration.

In the Graph Agent ID helper, an existing `kars-blueprint` application is
reused, or one is created if absent. Its typed
`agentIdentityBlueprintPrincipal` is ensured through the
[Graph BlueprintPrincipal API](https://learn.microsoft.com/en-us/graph/api/agentidentityblueprintprincipal-post?view=graph-rest-beta).
The CLI checks the principal's object ID, matching blueprint appId,
and derived type before provisioning credentials or writing
`KarsAuthConfig`. Both documented type annotations (with or without the
leading `#`) are accepted. An existing principal with omitted type metadata
requires a successful [typed read](https://learn.microsoft.com/en-us/graph/api/agentidentityblueprintprincipal-get?view=graph-rest-beta)
of that same object. Ordinary service
principals, mismatched IDs, ambiguous or incomplete lookups, and malformed
responses stop setup rather than being adopted or replaced. This type
check does not grant permissions or establish a usable Graph credential.

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

Role assignments may take time to propagate to Foundry's data plane.
A subsequent call is not guaranteed to succeed: confirm the token's
principal, tenant and audience, the assigned role and scope, and any
service-specific identity requirements before attributing an error solely
to propagation.

### Revoke

```bash
kars policy revoke prod-agent foundry-user
# or
az role assignment delete --assignee <agentIdentity.objectId> --scope <foundry-resource-id>
```

### Audit

Microsoft Entra portal → **Identity > Monitoring > Sign-in logs >
Service principal sign-ins**: inspect the recorded agent principal, for
example `kars-prod-prod-agent`, and correlate with downstream service logs.
Token caching means a sign-in record is not a record of every API call.

---

## Spawning sub-agents

When a spawn operation creates a separate `KarsSandbox` CR, the controller
uses the same identity reconcile path as for any other sandbox. An AGT
`mesh_send` message alone is not a sandbox spawn. With ready Agent ID
configuration and successful provisioning, the child receives a distinct
principal; its router uses the existing shared sidecar and its grants must
be authorized independently.

After both identities and their downstream access have been verified:

- Sub-agent permissions can be **narrower** than the parent's. The
  parent can spawn a "search" sub-agent that only has read access to
  one Foundry deployment.
- Sub-agent audit trails can be **separated by principal**. Correlate
  the recorded identity with Entra sign-in and downstream service logs;
  sign-in logs alone do not show every action.

Child sandbox deletion and identity cleanup still need verification; do not
assume deleting a parent removes every child or its grants in the same
reconcile pass. See [Tearing it down](#tearing-it-down).

The `kars handoff` command (see [CLI reference → `kars handoff`](cli-reference.md#kars-handoff))
does not itself prove destination Agent ID provisioning or permission
migration. Verify the destination configuration, principal and grants.

---

## When does the agent identity get created?

**During sandbox reconciliation**, once the effective mesh-auth mode
selects Agent ID and the AuthConfig is ready. `kars up` invocation alone
neither creates the identity nor establishes those conditions.

The controller checks the recorded `status.agentIdentity` and can recover
an existing identity rather than always creating one. When creation is
needed, its provisioner uses the controller managed identity's IMDS token
and the MI-to-Blueprint Graph exchange to create the derived principal,
then records it in sandbox status. That path depends on external identity
availability, federation and permissions.

The controller pins `PINNED_AGENT_IDENTITY_APP_ID`, `EXPECTED_TENANT_ID`
and `AUTH_SIDECAR_URL` in the inference router. The URL points to the
**shared** `entra-auth-sidecar.kars-system.svc:5000` service, not a
per-sandbox auth-sidecar container. The shared SDK sidecar acquires tokens
for the requested agent identity on demand; success still depends on the
selected sidecar credential mode and downstream grants.

Token caching:

- **Controller blueprint token** (Graph): cached by the controller's
  provisioning client across sandbox reconciles, normally for 50 minutes
  for a one-hour token. This is separate from the SDK sidecar's cache.
- **Per-agent-identity tokens** (downstream): acquired and refreshed on
  demand by the shared SDK sidecar. The router also bounds its cache by
  the token's expiry and the sidecar's reported lifetime.

After a reconcile observes a ready `KarsAuthConfig` with a changed tenant,
authority host, or controller managed-identity client ID, the controller
replaces that blueprint's provisioning client and its Graph token cache.
The cluster UID is also part of this comparison. Subsequent acquisitions
use the new configuration without restarting the controller; in-flight
reconciles can finish with their existing client. Unchanged identity
configuration retains the shared cache. This does not recreate existing
agent identities or change the SDK sidecar's exchange/refresh flow.

The sandbox runtime uses the inference router rather than managing these
Azure credentials. In Agent ID mode, router token acquisition uses the
shared sidecar exclusively: an acquisition failure is returned as an error,
not retried under a shared Workload Identity, IMDS identity or API key.

---

## Tearing it down

```bash
kubectl delete karssandbox prod-agent
# or
kars destroy prod-agent
```

The controller's finalizer **attempts** to delete the recorded agent
principal's ARM role assignments at configured `foundryRbac` scopes and
then the Graph principal. This is best-effort: failures are logged and do
not block finalizer removal. Missing AuthConfig or identity status can also
prevent cleanup. Check for remaining principals and assignments, including
manual grants outside configured scopes; CR deletion is not proof of
revocation or complete cleanup.

`kars mesh setup-trust` has no `--uninstall` option. Preserve a shared
Blueprint while other clusters or agents depend on it. Decommissioning needs
an explicit review of dependants, grants and federations. Helm also retains
`KarsAuthConfig/default` and versioned environment ConfigMaps; disabling or
uninstalling the sidecar does not revoke identities or stop controller
identity processing. See the
[Core retention and recovery guidance](../deploy/helm/kars/README.md#ownership-rollout-retention-and-recovery).

---

## Troubleshooting

### `kars up --mesh-trust=entra` refuses activation

Current `up` does not skip failed Agent ID setup and continue anonymously.
It requires the reviewed, already-deployed Core Helm identity configuration
and matching mesh verifier templates. Follow the canonical activation or
explicit release recovery procedure; a raw AuthConfig, role activation or
rerun alone does not satisfy it. The default `anonymous` requirement also
preserves installed Entra rather than concealing its verification failures.

### `az cli ca block` — "Could not enumerate directory roles (AADSTS530084)" {#az-cli-ca-block}

This troubleshooting applies to the **separate imperative
`kars mesh setup-trust` path**, not current `up`/Helm activation. Conditional
Access or Graph consent can prevent the Azure CLI from obtaining a usable
Graph credential. Follow the tenant's approved authentication/consent policy.

| Code | Meaning | Legacy provisioning behavior |
|---|---|---|
| `AADSTS530084` | Conditional Access block on the Azure CLI Graph token | `--mode agent-id` attempts the Bicep ARM/Graph-extension path; it does not launch device-code login |
| `AADSTS65001` / `AADSTS65002` | Missing Graph consent | The error is surfaced; no automatic consent/login retry |

`--mode bicep` explicitly selects that alternate provisioning path. It is
not proof that tenant policy permits the operation or that a usable typed
BlueprintPrincipal, token exchange and grants have been established. Its
application can be untyped/tag-based; portal visibility is not an identity
readiness check.

### When even Bicep cannot produce the typed blueprint

For an existing untyped application from the **legacy Bicep provisioning
path**, the helper prints an in-place Graph Explorer conversion reference.
This is not a current `up` activation step or permission to replace a shared
Blueprint. With tenant-administrator review:

1. Identify the existing Blueprint and its consumers. Bicep completion or
   AuthConfig presence does not prove the typed principal or runtime works.
2. Use https://developer.microsoft.com/en-us/graph/graph-explorer only if
   permitted by tenant policy. Its separate first-party application does not
   guarantee exemption from Conditional Access or consent requirements.
3. Review the printed PATCH for the existing application. The legacy helper
   includes `sponsors` and `owners` for portal visibility; changing the app's
   type is separate from verifying its typed BlueprintPrincipal:

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

   If Graph rejects the PATCH, stop and review the existing tenant objects
   with the administrator. Do not delete/recreate the shared Blueprint or
   raw-patch a Helm-owned AuthConfig as a routine recovery step. Application
   creation alone does not create or verify its typed principal, federation,
   consent or downstream grants.

4. Resolve device-compliance and consent requirements through the tenant's
   approved process. Re-check the existing BlueprintPrincipal and credential
   chain independently; neither portal visibility nor device enrollment
   guarantees runtime authentication. Any Core identity-value changes belong
   in the reviewed same-release Helm procedure.

The Graph helper's typed BlueprintPrincipal check is not merely portal
categorisation. The legacy Bicep application conversion reference does not
replace that check or establish accepted per-agent tokens.

### Foundry call returns 401 with the agent identity in the error

Do not assume a 401 is RBAC propagation. Inspect the token-exchange error,
tenant, audience, expiry and pinned agent identity; then check downstream
roles and scopes. A 403 can also reflect missing grants rather than a delay.
For example:

```bash
# Confirm the role is actually assigned at the right scope:
az role assignment list \
  --assignee <agentIdentity.objectId> \
  --scope <foundry-resource-id> \
  -o table
```

### `kubectl get karsauthconfig` returns NotFound

Distinguish a missing CRD from a missing `KarsAuthConfig/default` instance.
Establish the supported CRD through the Core installation/schema procedure
before identity activation. An anonymous installation need not have an
AuthConfig instance; ordinary `up` is not an identity-activation step. Do not
raw-apply an instance to bypass the same-release Helm activation/ownership
checks.

### Sandbox does not yet have an agent identity

Check the sandbox's effective mesh-auth mode, AuthConfig readiness and
controller logs. Empty status can indicate that provisioning is disabled,
not ready or failing; waiting a fixed interval does not guarantee creation:

```bash
kubectl logs -n kars-system deploy/kars-controller | grep agent_identity
```

Check these distinct prerequisites:

- The configured controller MI is assigned to the relevant AKS node-pool
  VMSS and controller IMDS access is permitted. Inspect the configured MI,
  rather than assuming a generated identity name.
- The controller MI principal matches the Blueprint federation subject,
  with the intended issuer/audience and Graph consent.
- The typed BlueprintPrincipal exists and matches the intended application.
- The shared sidecar's selected credential mode has its own prerequisites
  satisfied. Sidecar Workload Identity does not prove controller Graph
  provisioning, and successful ARM federation readback does not prove either
  token path.

---

## See also

- [`docs/permissions.md`](permissions.md) — full permission matrix
  and tenant-level prerequisites
- [`docs/architecture/entra-agent-id/01-runtime-token-flow.md`](architecture/entra-agent-id/01-runtime-token-flow.md)
  — the runtime token-acquisition flow
- [`docs/architecture/entra-agent-id/README.md`](architecture/entra-agent-id/README.md)
  — POC findings and design rationale
