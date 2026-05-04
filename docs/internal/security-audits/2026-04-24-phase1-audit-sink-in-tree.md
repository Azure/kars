# Phase 1 — AuditSink in-tree impl on `Governance`

**Date:** 2026-04-24
**Branch:** `phase1/audit-sink-in-tree`
**Scope:** Wire the `providers::AuditSink` four-seam contract to a real implementation on the in-tree `Governance` and migrate one production call-site. Closes the §0.2 #8 pseudo-impl gap for the audit seam.

## What landed

1. **`impl AuditSink for Governance`** in `inference-router/src/providers/audit_impl.rs`.
   - `append(event) -> AuditReceipt` delegates to `self.audit.log(agent_id, action, decision)` (the hash-chained `agentmesh::AuditLogger`), wraps the returned `AuditEntry.hash` as `ReceiptId`, and surfaces `previous_hash` as `prev_hash_hex`.
   - `get(receipt_id)` scans the in-memory chain for a matching entry hash; returns `Ok(None)` on not-found (per trait contract).
2. **Idempotency cache.** The upstream `AuditLogger::log` appends on every call — it is *not* idempotent, but the trait contract requires idempotent `append` on identical `(timestamp_ms, principal, action, payload_digest_hex)` tuples. `Governance` now owns an `AuditDedup` LRU (capacity 256) in `providers::audit_impl`. First call through a given key writes + caches the receipt; duplicate keys return the cached receipt without touching the chain.
3. **Label preservation.** `AuditEvent.labels` and `payload_digest_hex` are folded into the legacy three-field `decision` string as `verdict|k=v|k=v|digest=hex`, so they enter the hash chain (and would desync the chain on tampering).
4. **`AppState.audit_sink: Arc<dyn AuditSink>`** populated by coercion from the existing `Arc<Governance>`. Same instance as `AppState.governance` — no duplicate state, no extra lock.
5. **Call-site migration.** `handoff.rs::handoff_init` now calls `audit_events::handoff_init(&state, ...)` which routes through the trait. One call-site is enough to prove the seam is live; the remaining 17 `state.governance.audit.log(...)` direct-field calls migrate incrementally.

## What *didn't* land

- **No AGT-SDK-native audit sink.** `AgtAuditSink` is a separate follow-up branch.
- **No persistent dedup.** The cache is process-local; a router restart resets it. This is acceptable — the retry-after-`Unreachable` scenario the trait idempotency contract is designed for happens within the same request, same process. A persistent dedup would be over-engineering for the current use case.
- **Not every call-site migrated.** Only `handoff:init` migrates in this PR to minimise blast radius (§4.3). Each of the remaining 17 sites is its own one-liner and will land as a sibling `audit_events::*` helper in follow-up PRs.

## Threat model delta

None. The trait-level `append` ends up in the same `agentmesh::AuditLogger` hash chain as before, with labels additionally folded into the decision string (same hash-chain protection). STRIDE/OWASP mappings are unchanged.

Idempotency semantics **improve** — duplicate tuples within the dedup window no longer inflate the hash chain. Before this PR, a retry by a caller (hypothetically) would have produced two indistinguishable entries; now it produces one.

## AuthN / AuthZ path

Unchanged. `AuditSink::append` does not gate authz; it only records.

## Secret / key custody

No new secrets. The dedup cache holds hashes and action strings only, no bodies.

## Egress surface delta

Zero. In-tree sink is fully local (`Mutex<Vec>`).

## Failure mode

The in-tree sink never returns `Unreachable` or `QueueFull` (both are `Mutex<Vec>` operations). The error arms exist in the trait for forward-compatibility with `AgtAuditSink` and remote backends.

The migrated `handoff_init` helper logs audit-append errors at `warn` level and continues; the hand-off token is already persisted and rejecting the request because the sink is unreachable would be a denial-of-service vector against the sink. This matches the legacy behaviour (the old `state.governance.audit.log(...)` call had no return value at all).

## Negative-test coverage

| Test | Property asserted |
|---|---|
| `append_returns_receipt_with_chain_hash` | receipt carries a non-empty entry hash + `ReceiptId == entry_hash` + `prev_hash_hex` is `None` for the genesis entry |
| `second_append_links_to_first` | second entry's `prev_hash_hex` equals first entry's `entry_hash_hex` (chain property) |
| `identical_event_is_deduplicated` | duplicate tuple returns same receipt AND chain length stays at 1 |
| `different_timestamp_bypasses_dedup` | `timestamp_ms` is part of the dedup key |
| `different_digest_bypasses_dedup` | `payload_digest_hex` is part of the dedup key |
| `labels_are_folded_into_decision` | labels + digest appear in the hash-chained `decision` string — they can't be tampered with undetected |
| `get_returns_event_when_hash_matches` | `get` finds appended entries |
| `get_returns_none_for_unknown_id` | `get` returns `None`, not an error, for unknown IDs |
| `trait_object_coercion_works` | `Arc<dyn AuditSink>` coercion works and `append`/`get` via the trait object round-trip correctly |
| `dedup_cache_evicts_oldest_over_capacity` | cache is bounded at `DEDUP_CAPACITY` — after filling + 5 extra, the oldest keys are gone (re-appending them writes, doesn't dedup) |
| `parse_iso8601_*` (4 tests) | timestamp round-trip for epoch/fractional/offset/garbage inputs |
| `event_to_legacy_args_*` (2 tests) | the `(agent_id, action, decision)` translation is stable and compact |

The `dedup_cache_evicts_oldest_over_capacity` test is the bug-class guard: it catches a regression where the cache grows unbounded (memory leak) or fails to evict (silent drop of new entries once full).

## Vendored / third-party dependency delta

None. `agentmesh::AuditLogger` was already a workspace dep; no new crates.

## Vendored-patch audit

N/A — no `vendor/` files touched.

## Sign-offs

### Capability author
**Copilot** — 2026-04-24.
I confirm:
- No TODO/FIXME/unimplemented!/todo!/panic! on any production path.
- No hand-rolled crypto or protocol framing (rule §0.2 #8). The sha256 hash chain is provided by the upstream `agentmesh::AuditLogger`; this PR does not add or replace any hashing.
- All 6 CI gates pass locally with `BASE_REF=origin/dev`.
- `cargo test --all` passes (366 tests, +16 from this PR).
- The dedup cache is bounded and the eviction path is explicitly tested.

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>

### Independent reviewer
**Pál Lakatos-Tóth** (`pallakatos@microsoft.com`) — 2026-04-24.
I confirm:
- The dedup cache cannot mask a real audit append: its key includes `timestamp_ms`, so legitimately-distinct events with the same `(principal, action, digest)` are still recorded.
- Labels and payload digest are folded into the chained `decision` — a tamper that rewrites labels would break the hash chain.
- `Arc<Governance> → Arc<dyn AuditSink>` coercion shares state with `Arc<dyn PolicyDecisionProvider>` and with `AppState.governance`. Reconciled: confirmed all three views are `Arc::clone` of one instance (see `routes/mod.rs` `AppState::new`).
- `handoff_init` failure is non-fatal and logged — matches the legacy `.audit.log(...)` behaviour (which also had no return value).
- Audit append is now deduplicated; this *reduces* the hash-chain size on retries, which is a property improvement, not a regression.

Signed-off-by: Pál Lakatos-Tóth <pallakatos@microsoft.com>
