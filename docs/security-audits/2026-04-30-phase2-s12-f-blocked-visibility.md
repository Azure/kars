# Phase 2 — S12.f — Router blocked-attempt visibility

**Slice:** S12.f
**Branch:** `phase2-s12-f-blocked-visibility`
**Base:** `dev`

## Plan.md citation

> **S12.f — router blocked-attempt visibility (independent).** In enforce
> mode, capture blocked domain attempts to `/egress/learned/blocked`
> (separate ring buffer from learned-while-allowed). Bounded, deduped,
> rate-limited per source, hostname-only.
> — `~/.copilot/session-state/.../plan.md` line 483

## Existing implementation surveyed (§0.2 #8)

Before writing any code:

- **`inference-router/src/routes/egress.rs`** — current
  `GET /egress/learned` handler reads `state.blocklist.get_learned_domains()`
  and returns a JSON envelope `{ "learn_mode", "count", "domains" }`. RBAC
  is enforced one level up in `main.rs` where `egress_routes()` is merged
  into a `protected` `Router` that requires
  `Authorization: Bearer <admin-token>` via `admin_auth_middleware`. The
  new endpoint inherits that gate by being mounted in the same
  `egress_routes()` builder — no new auth path.
- **`inference-router/src/blocklist.rs`** — the existing learn-mode
  buffer for *allowed* observations (`learned_domains: HashSet<String>`)
  is intentionally **not refactored** in this slice. The blocked-attempt
  buffer is a sibling so observability of blocks does not pollute the
  allowed-observation buffer.
- **`inference-router/src/forward_proxy.rs`** — the deny branches in the
  router today are: (1) `handle_connect` → `blocklist.check_egress` Err;
  (2) `handle_connect` → `resolve_and_validate` Err (DNS rebinding);
  (3) `handle_http` → `blocklist.check_egress` Err; (4) `handle_http` →
  `resolve_and_validate` Err; (5) `handle_tls_redirect` → ECH detected;
  (6) `handle_tls_redirect` → `blocklist.check_egress` Err on the SNI;
  (7) `handle_tls_redirect` → `resolve_and_validate` Err. Each is the
  exact emission point for `BlockedBuffer::record(...)`. The "no SNI"
  branch is intentionally **not** recorded — there is no host string to
  store and the buffer rejects empty hosts by design.
- **`inference-router/src/routes/mod.rs::AppState`** — central shared
  state, cloned by axum and the forward proxy. The new buffer is added
  here as `Arc<BlockedBuffer>` and a clone is threaded into
  `forward_proxy::start` from `main.rs`.
- **Test scaffolding** — `tests/agt_governance_integration.rs` already
  demonstrates the canonical axum `oneshot` pattern; the new
  `tests/egress_blocked_endpoint.rs` mirrors it.

## Why blocked needs its own buffer

The existing `/egress/learned` buffer captures *observations of egress
that the policy allowed* (learn mode → discovery). The S12.f buffer
captures *attempts the policy denied in enforce mode*. These are two
different operational concerns:

- Mixing them would force operators to filter on a "was this allowed?"
  flag in every UI/CLI consumer of `/egress/learned`.
- Allowed observations naturally trend toward a stable allowlist;
  blocked observations are unbounded by definition (an attacker can
  generate arbitrary new domains). Different sizing, different
  rate-limiting, different threat model.
- Blocked entries carry a `source_sandbox` because attribution of
  blocked attempts matters for incident response; allowed observations
  in learn mode are already scoped to the sandbox.

## Threat surfaces introduced

- **One new HTTP endpoint:** `GET /egress/learned/blocked`. Returns a
  JSON envelope of hostname-only entries.
- **Auth:** mounted inside the existing `egress_routes()` `Router`, which
  is merged into the admin-token-protected `protected` router in
  `main.rs`. The endpoint inherits the same `Authorization: Bearer`
  gate that already protects `/egress/learned`, `/egress/allowlist`,
  etc. **No new auth path.**

## Threat surfaces NOT introduced

- **No payload disclosure.** Only `(host, port, source_sandbox,
  first_seen_unix, last_seen_unix, count)` is stored. Paths, query
  strings, request bodies, and headers are never observed by the
  buffer.
- **No PII leakage via IP literals.** IPv4/IPv6 literal hosts are
  rejected at `record()`-time; only DNS hostnames are stored. Bracketed
  IPv6 literals (`[::1]`) are also rejected.
- **No log-flooding amplification.** The per-source sliding-window
  rate limit caps ring-buffer churn at 100 new keys / 60 s (defaults).
  Existing entries continue to bump their aggregate `count` even when
  the rate limiter suppresses fresh writes — telemetry is preserved
  without unbounded memory growth.
- **No unbounded memory.** Capacity 1024 with FIFO eviction.
- **No cross-sandbox leakage of attribution data.** Blocked entries
  include `source_sandbox` only; in production the router serves a
  single sandbox and the field is the local `SANDBOX_NAME` env var.

## Test coverage

**Unit tests (`inference-router/src/egress_blocked.rs` `mod tests`):**
- `record_first_observation_returns_recorded`
- `record_duplicate_increments_count_and_dedupes`
- `record_distinct_hosts_creates_distinct_entries`
- `record_distinct_sandboxes_creates_distinct_entries`
- `rate_limit_drops_high_frequency_writes_from_one_source`
- `rate_limit_does_not_affect_other_sources`
- `rate_limit_window_resets_after_elapsed` (uses injected `Instant` via
  `record_at`, no `tokio::time` dependency)
- `capacity_evicts_oldest_fifo`
- `snapshot_returns_in_recency_order` — documented contract: **newest
  first** (reverse insertion order)
- `hostname_normalization_lowercases`
- `hostname_normalization_strips_trailing_dot`
- `record_rejects_ip_literal_host` (covers v4, v6, bracketed-v6)
- `record_rejects_empty_host` (covers empty, whitespace, `.`)
- `empty_source_sandbox_falls_back_to_unknown`
- `clear_resets_buffer`

**Integration tests (`inference-router/tests/egress_blocked_endpoint.rs`):**
- `endpoint_returns_empty_when_no_blocks`
- `endpoint_returns_recorded_blocks` (asserts dedup count, recency
  ordering, JSON wire shape including `source_sandbox`)

**Test count delta:** +15 unit (lib went 608 → 623) + 2 integration
binary = **+17 total**. Workspace `cargo test --all` green.

## Sign-offs

- `cargo fmt --all` — clean.
- `cargo clippy --all-targets -- -D warnings` (workspace) — clean.
- `cargo test --package azureclaw-inference-router` — 623 lib + all
  integration tests pass.
- `cargo test --all` — workspace green.
- No CRD / controller code touched.
- No new auth path.
- Hostname-only contract enforced at the type boundary (`record`
  rejects IP literals + empty strings).


Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
