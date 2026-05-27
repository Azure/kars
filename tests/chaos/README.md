# Chaos tier (Phase 2 S16)

Self-contained fault-injection test suite under `tests/chaos/`.

## When does it run?

| Trigger | What runs |
|---|---|
| PR CI — `Chaos Tier` job | `cargo test --package kars-chaos-tests --features chaos --tests` |
| Nightly perf — `.github/workflows/perf-nightly.yml` | `k6 run tests/k6/router_smoke.js` |
| PRs touching controller / router src — `Bench Regression` job | criterion benches with regression gate |
| Default `cargo test --all` | **does not** run chaos tier (feature-gated) |

The default suite stays fast for PR signal; the chaos tier is a
parallel job, not a serial dependency.

## Tier composition

22 chaos tests across four failure-mode categories:

| File | Cases | Failure mode |
|---|---|---|
| `tests/chaos/tests/k8s_api_flakes.rs` | 8 | K8s API: 500/503/429 storms, stale resourceVersion (410 GONE), truncated watch JSON, premature EOF, persistent 500, concurrent watchers |
| `tests/chaos/tests/foundry_storms.rs` | 6 | Foundry: 80/100 429 storm, 429 propagation (not 500), mid-stream 503 SSE close, slow-backend timeout, blocked-attempt metric, mixed storm convergence |
| `tests/chaos/tests/entra_rotation.rs` | 4 | WI / JWKS: token refresh mid-flight, single-flight invariant, JWKS rotation re-fetch, SA token file rotation |
| `tests/chaos/tests/agt_relay.rs` | 4 | Relay: WS upstream disconnect, handshake timeout (504-class), slow registry deadline, repeated churn (no task leak) |

## How to run locally

```bash
# Full chaos tier (~2-3s wall time — uses tokio time virtualization).
cargo test --package kars-chaos-tests --features chaos --tests

# Single file:
cargo test --features chaos -p kars-chaos-tests --test foundry_storms

# Single test:
cargo test --features chaos -p kars-chaos-tests \
    --test foundry_storms foundry_429_storm_respects_retry_after
```

## What each test asserts (invariant ↔ test map)

For every test the `///` doc comment names the production-code invariant
and where the same pattern lives in the production source. The shared
shape is: drive an axum-based mock against a reqwest client, inject the
fault on the mock side, assert the client-side reaction (no panic, no
infinite loop, eventual convergence, bounded calls).

| Invariant | Source file (production) | Chaos test |
|---|---|---|
| Re-watch on 5xx | `controller/src/reconciler/runtime.rs` | `k8s_watch_500_then_recovers`, `k8s_watch_503_recovers_without_panic` |
| Respect `Retry-After` | `inference-router/src/proxy.rs::send_with_retry` | `k8s_watch_429_respects_retry_after`, `foundry_429_storm_respects_retry_after` |
| 410 GONE → re-list | `controller/src/reconciler/runtime.rs` | `k8s_watch_410_gone_triggers_restart` |
| Truncated body → drop, no panic | `controller/src/policy_fetcher.rs` | `k8s_watch_truncated_json_does_not_panic` |
| Bounded retries (no DoS) | `inference-router/src/proxy.rs` | `k8s_watch_persistent_500_terminates`, `agt_relay_repeated_churn_no_task_leak` |
| 429 propagation (not 500) | `inference-router/src/proxy.rs` | `foundry_429_propagates_to_caller` |
| Mid-stream upstream close | `inference-router/src/routes/chat_completions.rs` | `foundry_503_midstream_sse_emits_clean_close` |
| Caller timeout trips | `inference-router/src/proxy.rs` | `foundry_slow_backend_caller_timeout_trips` |
| Blocked-attempt metric | `inference-router/src/egress_blocked.rs` | `foundry_blocked_metric_increments_per_rejection` |
| Token refresh mid-flight | `inference-router/src/auth.rs::get_token` | `entra_token_refresh_midflight_no_drop` |
| Single-flight refresh | `inference-router/src/auth.rs::get_token` | `entra_single_flight_one_network_call` |
| JWKS Kid rotation | A2A trust-store hot-reload | `entra_jwks_rotation_refetches_on_unknown_kid` |
| SA token file rotation | `inference-router/src/auth.rs::exchange_token` | `entra_sa_token_file_rotation_picks_up_new_value` |
| Reconcile not blocked by slow upstream | controller's per-call deadline | `agt_relay_slow_registry_does_not_block_reconcile` |
| 504 vs 502 distinction | router gateway error mapping | `agt_relay_handshake_timeout_returns_504_class` |

## How to add a new chaos case

1. Identify the **production invariant** you want to protect. If it is
   not already documented in the table above, add a row.
2. Add a `#[tokio::test(flavor = "current_thread", start_paused = true)]`
   function in the most relevant `tests/chaos/tests/*.rs` file.
3. Use the `harness::ChaosScript` to script upstream behaviour. Avoid
   real `tokio::time::sleep` for retry waits — paused time + the
   harness's `http_with_retry` keeps the case sub-second.
4. Assert the **specific** invariant (no panic / bounded calls /
   eventual convergence / propagation). Avoid loose "didn't error"
   assertions — they grow stale.
5. Update this table.

## Performance baselines

| Bench | File | Baseline | Regression gate |
|---|---|---|---|
| Reconciler decision | `controller/benches/reconciler_bench.rs` | `controller/benches/baselines.json` | median + 25 % |
| Proxy hot path | `inference-router/benches/proxy_bench.rs` | `inference-router/benches/baselines.json` | median + 25 %, p99 ≤ 5 ms |
| Router smoke (k6) | `tests/k6/router_smoke.js` | inline `thresholds` (p95 < 100 ms, err < 0.1 %) | nightly only |

Refresh baselines via `cargo bench --bench <name> -- --save-baseline dev`,
then copy the criterion-reported median into the corresponding
`baselines.json` file. Document the refresh in CHANGELOG.

## Anti-patterns

* **Do not** add tests that depend on real Azure / GitHub services.
  Every upstream is mocked.
* **Do not** use `tokio::time::sleep` to wait out a retry — use
  `start_paused = true` so virtual time advances instantly.
* **Do not** chase end-to-end controller / router integration in chaos
  tests. The chaos tier protects HTTP-layer invariants; e2e wiring is
  covered by `tests/e2e/`.
