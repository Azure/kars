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

### 3. feedback_count always 0 — wrong table name in reputation queries

The submit handler (`handlers.rs:submit_reputation`) calls a PG stored function
that inserts into `reputation_feedback` (singular, from migration 001). But the
score calculation (`reputation.rs:get_feedback_stats`) queried `reputation_feedbacks`
(plural, from migration 006) — a table that was never written to.

Also affected: `get_tag_aggregates` and `get_reputation_leaderboard` — same wrong
table. All three functions converted from `sqlx::query!` (compile-time checked
against stale `.sqlx/` cache) to `sqlx::query_as` (dynamic) targeting the correct
`reputation_feedback` table.

Files changed:
- `src/reputation.rs` — `get_feedback_stats`, `get_tag_aggregates`,
  `get_reputation_leaderboard`: `reputation_feedbacks` → `reputation_feedback`,
  `from_tier` → `rater_tier`, `sqlx::query!` → `sqlx::query_as`

### 4. Operational hardening

Multiple stability and security fixes for production deployment:

**Graceful shutdown**: Added SIGTERM/Ctrl+C signal handling with Actix
`ServerHandle::stop(true)` — drains in-flight requests before exit instead of
killing them immediately. Prevents data corruption on container restarts.

**Stale agent cleanup**: Background task (6-hour interval) deletes agents not
seen in 7 days to prevent DB bloat. Search already filters at 5 minutes, but
zombie entries accumulated indefinitely. Dormant agents (handoff predecessors)
are preserved for succession lookups.

**Health endpoint honesty**: `/v1/health` now returns 503 if the DB query fails
instead of silently returning `(0, 0)` via `unwrap_or_default()`. K8s probes
will correctly detect DB connectivity issues.

**Input validation**:
- Capabilities capped at 50 per registration (prevents storage DoS)
- One-time prekeys capped at 100 per upload (prevents storage DoS)
- Search pagination limit clamped to max 100 (prevents memory DoS)

**TOCTOU race fix**: `delete_stale_by_display_name` now includes
`last_seen < NOW() - INTERVAL '5 minutes'` condition, so concurrent
registrations with the same display_name won't delete freshly-registered agents.

**Startup panic fix**: Static file serving (`./static/index.html`) is now
conditional — only mounts the Files service if the static directory exists.
Previous code used `.expect()` which panicked on deployments without frontend
assets.

Files changed:
- `src/main.rs` — graceful shutdown, stale cleanup task, conditional static files
- `src/handlers.rs` — health DB error propagation, capabilities limit,
  prekey limit, pagination cap
- `src/db.rs` — `cleanup_stale_agents`, staleness condition in
  `delete_stale_by_display_name`

## Build

```sh
docker build --platform linux/amd64 -t azureclawacr.azurecr.io/agentmesh-registry:latest .
```
