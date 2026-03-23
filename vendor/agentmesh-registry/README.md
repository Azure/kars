# Vendored AgentMesh Registry (patched)

Upstream: https://github.com/amitayks/agentmesh/tree/main/registry (v0.3.0)

## Patch: raw timestamp signature verification for update endpoints

Same root cause as the relay fix. The `verify_update_signature` function (used
for status updates, capability updates, and **prekey uploads**) re-serialized
timestamps via chrono's `to_rfc3339()`, producing `+00:00` instead of the SDK's
`Z` suffix. This caused all prekey uploads to fail with 401, preventing X3DH
key exchange and E2E encrypted messaging.

Files changed:
- `src/auth.rs` — `verify_update_signature`: accepts `&str`, verifies raw bytes
- `src/models.rs` — All request model `timestamp` fields: `DateTime<Utc>` → `String`
- `src/handlers.rs` — Passes `&req.timestamp` (string ref) to auth

## Build

```sh
docker build --platform linux/amd64 -t azureclawacr.azurecr.io/agentmesh-registry:v0.3.1-rawts .
```
