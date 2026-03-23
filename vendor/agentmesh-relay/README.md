# Vendored AgentMesh Relay (patched)

Upstream: https://github.com/amitayks/agentmesh/tree/main/relay (v0.3.0)

## Patch: raw timestamp signature verification

The upstream relay deserializes the `timestamp` field in the `Connect` message
into a `chrono::DateTime<Utc>`, then re-serializes it via `to_rfc3339()` for
Ed25519 signature verification. Chrono's `to_rfc3339()` outputs `+00:00` instead
of `Z` and may alter fractional-second precision, so the bytes being verified
never match what the SDK signed with JS `Date.toISOString()`.

**Fix**: Keep the timestamp as a raw `String` in the `Connect` variant. Verify
the signature against the exact bytes the client sent. Parse into `DateTime`
only for the replay-protection time-window check.

Files changed:
- `src/types.rs` — `Connect.timestamp`: `DateTime<Utc>` → `String`
- `src/auth.rs` — `verify_connection_signature`: accepts `&str`, verifies raw
- `src/connection.rs` — passes `&timestamp` (string ref) to auth

## Build

```sh
docker build --platform linux/amd64 -t azureclawacr.azurecr.io/agentmesh-relay:v0.3.1-rawts .
```
