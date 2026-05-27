# Entra Agent ID — End-to-end POC, verified

## What we proved

Real Entra Agent ID + sidecar token acquisition works end-to-end in the
Microsoft corp tenant (72f988bf-86f1-41af-91ab-2d7cd011db47).

| Primitive | ID |
|---|---|
| Agent identity blueprint (App + SP) | `9010cbe3-ee13-4cb6-aa5f-f892910804a0` |
| Agent identity (ServiceIdentity SP) | `a8e0eff0-1fe0-4b46-aba3-d7fa7a1c2ecd` |
| FIC on blueprint trusting MI | issuer=login.microsoftonline.com/<tenant>/v2.0 |
| Managed identity used as credential | `a5cc7e08-ee03-4eee-b034-5302b6b54547` |

## The token the sidecar returned

```
GET /AuthorizationHeaderUnauthenticated/Graph?AgentIdentity=a8e0eff0-...

→ Bearer <token> with claims:
    aud:             https://graph.microsoft.com
    oid:             a8e0eff0-1fe0-4b46-aba3-d7fa7a1c2ecd    ← agent identity
    sub:             a8e0eff0-1fe0-4b46-aba3-d7fa7a1c2ecd    ← agent identity
    appid:           a8e0eff0-1fe0-4b46-aba3-d7fa7a1c2ecd    ← agent identity
    app_displayname: kars-poc-agent-1                         ← legible name
    idtyp:           app
```

vs without the `?AgentIdentity=` parameter:
```
    appid:    9010cbe3-... (blueprint, opaque)
    oid:      5a9587be-... (blueprint SP, opaque)
```

**The actor-flow swap is transparent.** kars sandbox calls sidecar with its own
agent identity GUID; Foundry/Graph/KV see `kars-poc-agent-1` as the principal.

## Measured times

- Blueprint provision (Graph POST + SP POST): ~3s
- Agent identity provision (Graph POST): ~1s
- MI provision: 4.59s
- FIC on blueprint: 2.03s
- Sidecar pull (56.6 MB): 0.28s
- Sidecar cold start to /healthz: 1.27s solo / 3s in multi-container ACI
- Token acquisition through sidecar: **546ms** (cold, includes MSAL fetch from login.microsoftonline.com)
- Token acquisition warm (cached): would be sub-50ms

## Critical sidecar config (real production env vars)

```yaml
env:
  - AzureAd__TenantId: <tenant>
  - AzureAd__ClientId: <blueprint-app-id>      # not the agent identity
  - AzureAd__ClientCredentials__0__SourceType: SignedAssertionFromManagedIdentity
  - AzureAd__ClientCredentials__0__ManagedIdentityClientId: <umi-client-id>
  - DownstreamApis__Foundry__BaseUrl: https://<foundry>/
  - DownstreamApis__Foundry__Scopes__0: https://ai.azure.com/.default
  - DownstreamApis__Foundry__RequestAppToken: "true"
  - DownstreamApis__Graph__BaseUrl: https://graph.microsoft.com/v1.0/
  - DownstreamApis__Graph__Scopes__0: https://graph.microsoft.com/.default
  - DownstreamApis__Graph__RequestAppToken: "true"
```

## Sidecar endpoints

- `/healthz` — liveness (note: 'z', not '/health')
- `/AuthorizationHeader/{api}` — **requires inbound JWT** (Bearer from caller)
- `/AuthorizationHeaderUnauthenticated/{api}` — **anonymous, uses configured creds**
  — kars uses this since sidecar is localhost-only inside the pod
- `/DownstreamApi/{api}` — proxies a downstream call with token attached

## Per-sandbox provisioning shape

```
kars up sandbox-N:
  controller →  POST /serviceprincipals/Microsoft.Graph.AgentIdentity
                {
                  "displayName": "kars-sandbox-N",
                  "agentIdentityBlueprintId": "<tenant-wide blueprint>",
                  "sponsors@odata.bind": [...]
                }
              → store returned `appId` in pod env as AGENT_IDENTITY_APP_ID
              → pod starts sidecar + openclaw; openclaw calls
                localhost:8080/AuthorizationHeaderUnauthenticated/Foundry
                ?AgentIdentity=${AGENT_IDENTITY_APP_ID}

kars down sandbox-N:
  controller →  DELETE /serviceprincipals/<appId>
```

## Open items for production

| Item | Path |
|---|---|
| AKS OIDC issuer FIC allow-list (Pattern B) | IDAdmin ticket per cluster |
| Or use MI-mediated FIC (Pattern A) | works today, no admin ticket |
| Foundry RBAC on agent identities | use `az role assignment create` per agent at provision time |
| Tenant-side blueprint sponsor governance | already enforced by Entra |

## Verdict for kars migration

**Recommended.** The architecture works end-to-end today. Migration plan:
- Phase 0 (Setup): rewrite `kars mesh setup-trust` to create blueprint + FIC instead of api://agentmesh app reg
- Phase 1 (Controller): rewrite `controller/src/fedcred.rs` to create agent identities instead of MI fedcreds
- Phase 2 (Sandbox): add sidecar container to sandbox pod, remove entrypoint.sh token exchange (~80 LoC)
- Phase 3 (Router): adapt inference-router to call sidecar instead of doing the exchange itself

Net code: -679 / +300 ≈ -379 LoC. Plus governance gains (per-agent audit, blueprint sponsorship).
