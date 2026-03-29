# Vendored AgentMesh Registry (patched)

Upstream: https://github.com/amitayks/agentmesh/tree/main/registry (v0.3.0)

## Patches

### 1. Raw timestamp signature verification (original)

Same root cause as the relay fix. The `verify_update_signature` function (used
for status updates, capability updates, and **prekey uploads**) re-serialized
timestamps via chrono's `to_rfc3339()`, producing `+00:00` instead of the SDK's
`Z` suffix. This caused all prekey uploads to fail with 401.

Files changed:
- `src/auth.rs` — `verify_update_signature`: accepts `&str`, verifies raw bytes
- `src/models.rs` — All request model `timestamp` fields: `DateTime<Utc>` → `String`
- `src/handlers.rs` — Passes `&req.timestamp` (string ref) to auth

### 2. Ghost agent cleanup + heartbeat + search freshness

Sub-agent containers generate new AMIDs on each restart but register under the
same `display_name`. Without cleanup, the registry accumulates ghost entries and
discovery returns stale/dead agents.

Fixes:
- **Replace-on-register**: registration deletes old agents with the same
  `display_name` before inserting (prevents ghost accumulation)
- **Heartbeat endpoint**: `POST /v1/registry/heartbeat` with `{ "amid": "..." }`
  updates `last_seen` and sets `status = online` (no signature required)
- **Initial status**: agents register as `Online` instead of `Offline`
- **Search freshness**: search only returns agents seen in the last 5 minutes
  (`last_seen > NOW() - INTERVAL '5 minutes'`), ordered by `last_seen DESC`

Files changed:
- `src/handlers.rs` — ghost cleanup in `register_agent`, new `heartbeat` handler + route
- `src/db.rs` — `delete_stale_by_display_name`, `heartbeat_agent`, search freshness filter

## Build

```sh
docker build --platform linux/amd64 -t azureclawacr.azurecr.io/agentmesh-registry:v0.3.1-rawts .
```
