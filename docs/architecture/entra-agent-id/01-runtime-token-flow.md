# Entra Agent ID — Runtime Token Flow

This document captures the architecture that was validated end-to-end on
real AKS in the Microsoft tenant during the POC phase. It is the
specification the kars controller, sidecar, and inference-router must
implement.

## What gets authenticated

```
sandbox openclaw code
       │
       ▼
inference-router (UID 1001, trusted)
       │ HTTP localhost:8080 with AgentIdentity pinned via env var
       ▼
auth-sidecar (UID 1002, Microsoft Entra SDK)
       │ IMDS at 169.254.169.254
       │ → MI assertion (audience = api://AzureADTokenExchange)
       │
       │ POST /oauth2/v2.0/token (client_credentials + jwt-bearer)
       │ → blueprint token (appid = blueprint, role = AgentIdentity.CreateAsManager)
       │
       │ MSAL internal actor flow with AgentIdentity claim
       │ → agent identity token (oid = agent, idtyp = app)
       ▼
Foundry / Graph / KV / Storage
   (RBAC scoped to the agent identity)
```

## What's NOT used

- **AKS Workload Identity** for blueprint authentication — this path hits
  Entra anti-loop `AADSTS700231` ("Token obtained using a federated
  identity credential may not be used as a federated identity credential")
- **Client secrets** on the blueprint — banned by Microsoft tenant
  policy 538f1913
- **Self-signed certificates** on the blueprint — same policy caps
  lifetime to zero
- **Direct AKS OIDC FIC** on the blueprint — AKS issuer URLs are not in
  the tenant FIC allow-list

## What is used

- **AKS node-pool user-assigned managed identity** (assigned to VMSS via
  `az vmss identity assign`). Tokens from this MI are issued by IMDS
  directly from the Azure fabric, NOT FIC-derived → no anti-loop.
- **MI-as-FIC on the blueprint**: federated credential with
  `issuer = https://login.microsoftonline.com/<tid>/v2.0`,
  `subject = <controllerMI principalId>`,
  `audience = api://AzureADTokenExchange`. This is the universally
  allow-listed issuer pattern.
- **Microsoft Entra SDK sidecar**:
  `mcr.microsoft.com/entra-sdk/auth-sidecar:1.0.0-azurelinux3.0-distroless`.
  Open source, .NET 9, listens on port 8080.

## Required sidecar configuration

```yaml
env:
  - name: AzureAd__TenantId
    value: <tenant-id>
  - name: AzureAd__ClientId
    value: <blueprint-app-id>      # NOT the MI
  - name: AzureAd__Instance
    value: https://login.microsoftonline.com/
  - name: AzureAd__ClientCredentials__0__SourceType
    value: SignedAssertionFromManagedIdentity
  - name: AzureAd__ClientCredentials__0__ManagedIdentityClientId
    value: <controller-mi-client-id>
  - name: DownstreamApis__Foundry__BaseUrl
    value: https://<account>.cognitiveservices.azure.com/
  - name: DownstreamApis__Foundry__Scopes__0
    value: https://ai.azure.com/.default
  - name: DownstreamApis__Foundry__RequestAppToken
    value: "true"
  - name: DownstreamApis__Graph__BaseUrl
    value: https://graph.microsoft.com/v1.0/
  - name: DownstreamApis__Graph__Scopes__0
    value: https://graph.microsoft.com/.default
  - name: DownstreamApis__Graph__RequestAppToken
    value: "true"
```

## How tokens are acquired at runtime

The inference-router calls the sidecar's anonymous endpoint with
the agent identity pinned in the URL:

```
GET http://localhost:8080/AuthorizationHeaderUnauthenticated/Foundry
    ?AgentIdentity=<agent-identity-app-id>
```

Sidecar internals (we don't implement these; they're in MSAL):

1. Read configured credential source → call IMDS to get MI token
2. POST to `/oauth2/v2.0/token` with the MI token as `client_assertion`
   → receive blueprint token (audience varies based on downstream API)
3. Trigger MSAL actor flow with `AgentIdentity` claim → receive token
   whose `oid` is the agent identity
4. Return `Bearer <token>` in JSON body

Response shape:

```json
{
  "authorizationHeader": "Bearer eyJ..."
}
```

## Token claims (proven, captured from real Foundry call)

```json
{
  "aud":             "https://ai.azure.com",
  "iss":             "https://sts.windows.net/<tenant>/",
  "appid":           "<agent-identity-app-id>",
  "oid":             "<agent-identity-object-id>",
  "sub":             "<agent-identity-object-id>",
  "tid":             "<tenant>",
  "app_displayname": "kars-poc-agent-1",
  "idtyp":           "app",
  "xms_par_app_azp": "<blueprint-app-id>"   ← actor-flow attestation
}
```

The `xms_par_app_azp` claim is Microsoft's machine-verifiable assertion
that this token was minted via the blueprint's actor flow.

## Required Graph API operations (controller side)

### Create an agent identity

```http
POST https://graph.microsoft.com/beta/servicePrincipals/Microsoft.Graph.AgentIdentity
OData-Version: 4.0
Content-Type: application/json
Authorization: Bearer <blueprint-token>

{
  "displayName": "kars-<cluster>-<sandbox>",
  "agentIdentityBlueprintId": "<blueprint-app-id>",
  "sponsors@odata.bind": [
    "https://graph.microsoft.com/v1.0/users/<owner-oid>"
  ]
}
```

Response (subset of fields we care about):

```json
{
  "@odata.context": "https://graph.microsoft.com/beta/$metadata#servicePrincipals/microsoft.graph.agentIdentity/$entity",
  "id":                     "<object-id>",
  "appId":                  "<app-id>",
  "displayName":            "kars-<cluster>-<sandbox>",
  "servicePrincipalType":   "ServiceIdentity",
  "agentIdentityBlueprintId": "<blueprint-app-id>",
  "createdDateTime":        "2026-05-27T11:22:48Z"
}
```

### Delete an agent identity

```http
DELETE https://graph.microsoft.com/beta/serviceprincipals/<object-id>
OData-Version: 4.0
Authorization: Bearer <blueprint-token>
```

### Create a blueprint (one-time per tenant)

The blueprint is an `Application` with a special `@odata.type`:

```http
POST https://graph.microsoft.com/v1.0/applications/
OData-Version: 4.0
Content-Type: application/json

{
  "@odata.type": "#Microsoft.Graph.AgentIdentityBlueprint",
  "displayName": "kars-<cluster>-blueprint",
  "serviceManagementReference": "<service-tree-guid>",   // required in Microsoft tenant
  "sponsors@odata.bind": [
    "https://graph.microsoft.com/v1.0/users/<creator-oid>"
  ],
  "owners@odata.bind": [
    "https://graph.microsoft.com/v1.0/users/<creator-oid>"
  ]
}
```

A separate SP must then be created for the blueprint app:

```http
POST https://graph.microsoft.com/v1.0/servicePrincipals
{
  "appId": "<blueprint-app-id>"
}
```

Without the SP, the blueprint isn't visible in the Entra Agents portal
page and agent identities can't be derived from it.

### Add MI-as-FIC to the blueprint

```http
POST https://graph.microsoft.com/v1.0/applications/<blueprint-id>/federatedIdentityCredentials
{
  "name": "kars-controller-mi",
  "issuer": "https://login.microsoftonline.com/<tenant>/v2.0",
  "subject": "<controller-mi-principal-id>",
  "audiences": ["api://AzureADTokenExchange"]
}
```

## Required Azure operations (controller side)

### Assign controller MI to AKS node pool VMSS

```bash
az vmss identity assign -g <node-rg> -n <vmss-name> \
  --identities <controller-mi-rid>
```

This makes the MI's IMDS token available to all pods running on that
node pool. The kars sandbox pod's sidecar uses
`SignedAssertionFromManagedIdentity` source type which reads from IMDS.

### Verify role assignments on the agent identity (per-sandbox RBAC)

```bash
az role assignment create \
  --assignee-object-id <agent-identity-object-id> \
  --assignee-principal-type ServicePrincipal \
  --role "Cognitive Services User" \
  --scope <foundry-rid>
```

## Network restrictions enforced by egress-guard init container

```
UID 1000 (openclaw, agent code):
  ❌ Cannot reach 127.0.0.1:8080 (sidecar)
  ❌ Cannot reach 169.254.169.254 (IMDS)
  ✅ Can reach 127.0.0.1:8443 (router)

UID 1001 (inference-router):
  ✅ Can reach 127.0.0.1:8080 (sidecar)
  ❌ Cannot reach 169.254.169.254 (IMDS — sidecar handles this)
  ✅ Can reach external Foundry/Graph (via egress allowlist)

UID 1002 (sidecar):
  ✅ Can reach 169.254.169.254 (IMDS)
  ✅ Can reach login.microsoftonline.com
  ✅ Listens on 127.0.0.1:8080 (pod-internal only)
```

## Failure modes

| Condition | Behavior |
|---|---|
| KarsAuthConfig CR absent | Pod starts in anonymous tier (no sidecar, no MI access) |
| Graph unreachable when reconciling new sandbox | Reconcile fails with backoff; pod NOT created; condition `AgentIdentityReady=False` |
| Sidecar fails to acquire blueprint token | Sidecar returns 401/500 to router; router fails the Foundry request with structured error |
| Agent identity has no RBAC on requested resource | Foundry returns 401 with agent identity oid in error message; router surfaces to caller |
| Controller MI loses access (rare) | Status condition `ControllerMIReachable=False`; alerts; pods on existing tokens keep working until expiry |

## Limits and quotas

| Resource | Limit | Mitigation |
|---|---|---|
| Federated credentials per Application | 20 | One per cluster's controller MI. With 20-cluster headroom, plenty. |
| Agent identities per blueprint | (TBD — verify in spike) | Likely thousands; monitor + alert |
| Graph API rate limit | 130 req/sec sustained | Backoff in controller; idempotent create |

## Performance (measured during POC)

| Stage | Time |
|---|---|
| Sidecar cold start to /healthz | ~2s |
| IMDS → MI token (first call) | <100ms |
| MI token → blueprint token | ~500ms |
| Blueprint → agent identity token (actor flow) | ~50ms |
| Subsequent /AuthorizationHeader calls (warm cache) | <50ms |
| Token TTL | 1h (full lifecycle covered) |
