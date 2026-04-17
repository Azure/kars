# Vendored AgentMesh Relay (patched)

Upstream: https://github.com/amitayks/agentmesh/tree/main/relay (v0.3.0)

## Patch 1: raw timestamp signature verification

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

## Patch 2: session-aware connection management (ghost connection fix)

**Problem**: When the same AMID reconnects (e.g., after a network drop), the
upstream relay overwrites the connection entry in `DashMap` but leaves the old
handler tasks running. After 25s, the old handler's ping task times out and calls
`unregister(amid)` — which removes the *new* connection's entry. The agent
becomes invisible ("ghost"): relay thinks it's offline, messages get stored
instead of delivered.

**Root cause**: `unregister()` accepts only the AMID string, with no way to
distinguish which connection initiated the cleanup.

**Fix**:
1. Added `ConnectionEntry` struct wrapping `session_id: Uuid` + `sender` channel
2. `register()` now supersedes old connections: removes old entry (dropping the
   sender, which causes old handler tasks to detect channel closure and exit)
3. `unregister()` now requires `session_id` parameter — only removes the entry if
   the session matches, preventing stale handlers from removing newer connections
4. `send_to()` now returns `SendResult` enum (`Delivered | Offline | ChannelBroken`)
   instead of `bool`. On `ChannelBroken`, it proactively cleans up the stale entry
   so the caller can fall through to store-forward

Files changed:
- `src/types.rs` — Added `SendResult` enum
- `src/connection.rs` — Rewrote `ConnectionManager` with session-aware tracking,
  updated all callers to pass `session_id` to `unregister()` and handle `SendResult`

## Patch 3: HTTP health endpoint (K8s probe fix)

**Problem**: The relay only speaks WebSocket on port 8765. K8s `tcpSocket`
readiness probes connect and immediately close without completing the WebSocket
handshake, generating "Handshake not finished" warnings every 10 seconds per
probe interval. This clutters logs and wastes resources.

**Fix**: Added a separate HTTP health endpoint on port 8766 (configurable via
`HEALTH_PORT` env var). Returns JSON with connection count and store-forward
stats. K8s probes should use `httpGet` on port 8766 instead of `tcpSocket` on 8765.

Files changed:
- `src/main.rs` — Added `serve_health()` function on separate TCP listener
- `Dockerfile` — Expose port 8766, set `HEALTH_PORT` env var

## Patch 4: explicit close reason error codes

**Problem**: When the relay supersedes an old connection (same AMID reconnects)
or times out a ping, it drops the WebSocket without telling the client *why*.
Clients then auto-reconnect and get superseded again, creating a reconnect storm.

**Fix**: Added two new `ErrorCode` variants — `SessionReplaced` and `PingTimeout`
— and the relay now sends an `{"type":"error","code":"SESSION_REPLACED",...}`
frame on the old sender before dropping it (similarly for ping timeouts). The
mesh-plugin client treats `SESSION_REPLACED` as an intentional close and stops
auto-reconnecting, eliminating the fight between two sessions.

Files changed:
- `src/types.rs` — Added `SessionReplaced` and `PingTimeout` variants to `ErrorCode`
- `src/connection.rs` — `register()` supersede path sends `SessionReplaced` before
  drop; ping watchdog sends `PingTimeout` before unregister

## Build

```sh
docker build --platform linux/amd64 -t azureclawacr.azurecr.io/agentmesh-relay:latest .
```
