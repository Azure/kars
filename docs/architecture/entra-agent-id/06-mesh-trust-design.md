# Phase 6 — Entra-signed AGT mesh trust (design + status)

> Status: **shipped** in Phase 6.b+6.c (commits `78606a8`, `b300526`,
> `4c0d466`). End-to-end verified on `kars-aks` with 9/9 e2e harness
> PASS under verified-tier registration.

When the operator sets `kars up --mesh-trust=entra`, kars replaces
the anonymous-tier mesh registration with **Entra-signed agent
identity tokens** so the AGT relay/registry:

1. Verify each mesh peer against Entra's published JWKS at WebSocket
   connect time
2. Pin the peer's identity to the agent identity `appId` (same
   principal kars uses for Foundry RBAC — one identity across data
   plane and mesh plane)
3. Score peers by tier (verified, blueprint-derived) rather than the
   current binary anonymous/not

Anonymous tier (`--mesh-trust=anonymous`, default) remains supported
for local dev, demos, and tenants where Entra provisioning is
unavailable. The trust threshold falls back to 0 and the SDK's X3DH
handshake is the only gate.

## What shipped

| Piece | Owner | Status |
|-------|-------|--------|
| (a) CRD field + auth chain shape | kars controller | ✅ `KarsAuthConfig.spec.meshAuthBackend` enum live |
| (b) Sandbox entrypoint mints token via shared sidecar | kars sandbox image | ✅ `inference-router /v1/mesh-token` + `entrypoint.sh` MESH_AUTH_BACKEND branch |
| (c) AGT relay/registry JWKS verification | Microsoft AGT | ✅ **Merged upstream as [microsoft/agent-governance-toolkit#2719](https://github.com/microsoft/agent-governance-toolkit/pull/2719)** — `agent-governance-python/agent-mesh/src/agentmesh/identity/entra_verifier.py` (330 LOC JWKS verifier), `POST /v1/registry/verify` endpoint, per-agent session counters + `completion_rate`, opt-in via `AGENTMESH_ENTRA_AUDIENCE` + `AGENTMESH_ENTRA_TENANT_ID`. Merged 2026-05-31, +2289/-560 across 14 files (commit `a8a96bf4`). |
| (d) CLI operator switch | kars CLI | ✅ Single `--mesh-trust=anonymous\|entra` flag on `kars up` |

## Per-sandbox identity lifecycle (Phase 5b)

When `KarsAuthConfig/default.meshAuthBackend=EntraAgentIdentity` and
a new `KarsSandbox` is created, the controller now (Phase 5b):

1. Creates a typed `microsoft.graph.agentIdentity` SP derived from
   the blueprint via Graph beta API
2. Assigns Foundry data-plane RBAC (`Cognitive Services User`) on the
   AI Services resource scoped to that SP
3. Creates a federated identity credential linking the SP to the
   sandbox's Kubernetes service account
4. Stamps `KarsSandbox.status.agentIdentity.appId` for kubectl
   visibility

Deletion cleans up in reverse: federated cred → RBAC assignment →
agent identity SP. Zero manual `az role assignment create` calls.

## Verification

- Each sandbox pod's `inference-router` logs show
  `Mesh token acquired via auth-sidecar after N attempt(s) — verified-tier registration`
  at startup when `MESH_AUTH_BACKEND=EntraAgentIdentity`.
- AGT relay logs show `WebSocket /ws connect verified appid=… tid=…`
  for each peer after the Entra patches land upstream.
- `kubectl get karssandbox <name> -o jsonpath='{.status.agentIdentity.appId}'`
  surfaces the per-sandbox typed agent identity.

## Historical design context

Original design before Phase 6.b/6.c shipped, kept for posterity:

### The full target flow

```text
┌──────────────────────────────┐
│ kars sandbox pod             │
│                              │
│  inference-router (UID 1001) │
│   • new /v1/mesh-token route │ ← entrypoint hits this
│   • internally calls         │
│     entra-auth-sidecar       │
│     ?AgentIdentity=<appId>   │
│   • returns Bearer token to  │
│     UID 1000 via 127.0.0.1   │
└─────────────┬────────────────┘
              │ Authorization: Bearer <agent identity token>
              │ aud = api://agentmesh (or per-cluster custom scope)
              │ tid = corp tenant
              │ appid = <per-sandbox agentIdentity>
              ▼
┌──────────────────────────────┐
│ AGT relay (in agentmesh ns)  │
│                              │
│  Fetch JWKS from             │
│  login.microsoftonline.com/  │
│  common/discovery/keys       │
│                              │
│  Verify signature + tid +    │
│  aud, then extract appid as  │
│  the peer DID.               │
│                              │
│  Trust score = mapping from  │
│  (appid → tier from CSAs)    │
│    AgentClassification +     │
│    DataSensitivity custom    │
│    security attributes ←→    │
│    score table (operator     │
│    configurable).            │
└──────────────────────────────┘
```

## Piece (a) — CRD scaffold (this PR)

`KarsAuthConfig.spec.meshAuthBackend`: enum, default `Anonymous`
preserves current behaviour. Set to `EntraAgentIdentity` once
pieces (b) and (c) are deployed.

- `Anonymous` (default) — current behaviour. Sandbox registers without
  a token; `AGT_TRUST_THRESHOLD` is forced to 0 (entrypoint already
  does this fail-open logic). No code-path change.
- `EntraAgentIdentity` — sandbox entrypoint MUST acquire an agent
  identity token via the shared sidecar and present it on every relay
  connection. Relay MUST verify against Entra JWKS.

The reconciler in `auth_config_reconciler.rs` reads this field to
decide whether to inject a `MESH_AUTH_BACKEND=EntraAgentIdentity` env
var on the sandbox (or just on the inference-router so the new
`/v1/mesh-token` route refuses requests when the backend is not
enabled).

## Piece (b) — entrypoint via sidecar (next PR)

The sandbox's `openclaw` container runs as UID 1000, which the
egress-guard `iptables` baseline blocks from making outbound TCP
**except to loopback** (the inference-router on 127.0.0.1:8443) and
DNS. The router is UID 1001 and can reach the kars-system Service
DNS (verified in Phase 7).

The clean path:
1. Add a new internal route on the inference-router:
   ```
   GET http://127.0.0.1:8443/v1/mesh-token
   Response: { "access_token": "<bearer>", "expires_in": 3600 }
   ```
2. Internally the router calls the shared sidecar with
   `?AgentIdentity=$PINNED_AGENT_IDENTITY_APP_ID` (env it already
   pins) targeting the AGT mesh audience.
3. `entrypoint.sh` exports the response as `AGT_OAUTH_TOKEN` and
   stops calling Entra directly.
4. The existing fail-open logic on
   `AGT_OAUTH_TOKEN`-empty stays as a safety net during the rollout.

Cost: ~80 LoC in the router (route + sidecar call + claim pin to
`AGT_RELAY_AUDIENCE`), ~30 LoC change in `entrypoint.sh` to replace
the curl-against-login.microsoftonline.com block.

## Piece (c) — relay JWKS verification (vendored upstream)

The AGT relay deployment at
`deploy/agentmesh-agt.yaml` runs the Microsoft
`agent-governance-toolkit` Python relay. Today it accepts unverified
connections.

For full enforcement we need either:
- An upstream AGT release that adds optional JWKS verification with an
  ENV switch, OR
- A vendored patch that pins our requirements:
  - Fetch + cache `https://login.microsoftonline.com/common/discovery/keys`
  - Verify signature, `tid` (against `KarsAuthConfig.spec.tenant.tenantId`),
    `aud` (against `KarsAuthConfig.spec.meshAuthAudience`, default
    `api://agentmesh`)
  - Extract `appid` as the registry DID
  - Set the peer's trust tier from a lookup table (CSA attributes ←→
    score)

This piece is upstream-coordination work — should be proposed to
the Microsoft AGT team rather than vendored, per kars convention.

## Why this PR's scaffold is still useful

Landing the CRD field now means:
- Operators can pin their KarsAuthConfig today even though the
  enforcement isn't live, so when (b)+(c) ship the migration is just
  `--set entraAgentIdentity` rather than a CRD upgrade.
- The default `Anonymous` value is 100 % backward compatible — no
  existing cluster behaviour changes.
- The reconciler treating an unknown future value as "force anonymous"
  means the controller running an older binary against a CR with the
  new field is graceful.

## Test plan (when (b) + (c) land)

1. KAC patched with `meshAuthBackend: EntraAgentIdentity` →
   reconciler injects MESH_AUTH_BACKEND env on the router.
2. Sandbox pod boot: `entrypoint.sh` calls
   `http://127.0.0.1:8443/v1/mesh-token` → 200 with a valid JWT.
3. Decoded token: `aud=api://agentmesh`,
   `appid=$PINNED_AGENT_IDENTITY_APP_ID`, `tid` matches.
4. WebSocket to AGT relay: connection upgrades successfully with
   `Authorization: Bearer <token>`.
5. Registry log: peer registered with `did:agentmesh:<appid>`,
   NOT `did:agentmesh:anonymous`.
6. Force a token with the wrong tid → relay refuses the WebSocket
   upgrade with 401.
7. Restart sandbox, sidecar restart, sandbox restart — token cache
   flushes cleanly, no peer-identity flicker.
