# Phase 6 — Entra-signed AGT mesh trust (design)

> Status: **scaffolded** in this PR (CRD field + doc); full enforcement
> tracked as the next milestone.

Today the kars sandbox AGT-mesh peer registers as **anonymous tier**
(`AGT_OAUTH_TOKEN` empty, `AGT_TRUST_THRESHOLD=0`). Peer KNOCKs are
gated only by the SDK's X3DH handshake, not by any verified identity.
Trust scores are meaningless ("everyone is 0").

This document captures the design for **Goal 1** of the agent-id work:
replace the anonymous-tier registration with **Entra-signed agent
identity tokens** so the AGT relay/registry can:

1. Verify each mesh peer against Entra's published JWKS.
2. Pin the peer's identity to the agent identity `appId` (already used
   for Foundry RBAC — same principal across the data plane).
3. Score peers by tier (verified, blueprint-derived) rather than the
   current binary anonymous/not.

## Why this PR doesn't ship it yet

Three independent moving parts must land together for the chain to be
enforceable end-to-end. This PR delivers piece (a) only; (b) and (c)
are tracked as follow-up work.

| Piece | Owner | This PR | Next |
|-------|-------|---------|------|
| (a) CRD field + auth chain shape | kars controller | ✅ `KarsAuthConfig.spec.meshAuthBackend` enum scaffolded | — |
| (b) Sandbox entrypoint mints token via shared sidecar | kars sandbox image | ❌ | required |
| (c) AGT relay/registry JWKS verification | Microsoft AGT | ❌ | required |

## The full target flow

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
