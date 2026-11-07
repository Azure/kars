<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Chaos tier — operations guide

The chaos tier is a permanent CI surface that protects
controller and router behaviour under failure-mode duress. It is **not**
load testing for marketing numbers — it is reliability-invariant
coverage for the failure shapes operators actually see in production.

## Tier composition

| Category | File | What it injects |
|---|---|---|
| K8s API flakes | `tests/chaos/tests/k8s_api_flakes.rs` | 500/503 storms, 429 + Retry-After, 410 GONE, truncated JSON, premature EOF, persistent 500, concurrent watchers |
| Foundry storms | `tests/chaos/tests/foundry_storms.rs` | 80/100 429 storm, 429 propagation (not 500), mid-stream 503 SSE close, slow-backend client timeout, blocked-attempt metric, mixed-storm convergence |
| Entra rotation | `tests/chaos/tests/entra_rotation.rs` | Token refresh mid-flight, single-flight invariant, JWKS Kid rotation, SA token file rotation |
| AGT relay | `tests/chaos/tests/agt_relay.rs` | WS upstream disconnect, handshake timeout (504-class), slow registry deadline, repeated churn (no task leak) |

See [`tests/chaos/README.md`](../../tests/chaos/README.md) for the authoritative
test inventory and the precise invariant ↔ test map.

## When does the tier run?

| Trigger | Job | What runs |
|---|---|---|
| PR / push to `dev` / `main` | `Chaos Tier` (CI) | `cargo test --package kars-chaos-tests --features chaos --tests --no-fail-fast` |
| PR / push to `dev` / `main` | `Bench Regression` (CI) | Criterion benches; fail if candidate > same-run baseline + 25 % |
| Nightly (04:00 UTC) | `perf-nightly.yml` | k6 smoke against the router (50 VUs / 30 s) |
| Default `cargo test --all` | (none) | Chaos tests are feature-gated; do not run |

PR signal stays fast because the chaos tier compiles only when the
`chaos` feature is enabled, and it runs in a parallel job. The
invocation is scoped to the `kars-chaos-tests` crate so it does
not re-execute the full ~1800-test workspace suite that `rust-build`
already runs (the legacy `--workspace --features chaos` form did, and
wasted ~95 % of the job's wall-clock budget).

## Failure mode each test covers

The full table lives in `tests/chaos/README.md`. Highlights:

* **No infinite watch loops** — `k8s_watch_persistent_500_terminates`
  asserts the controller surfaces an error after `max_attempts` instead
  of DoSing the API.
* **Retry-After actually waited** — `k8s_watch_429_respects_retry_after`,
  `foundry_429_storm_respects_retry_after` measure wall-time elapsed.
* **429 propagated** — `foundry_429_propagates_to_caller` asserts the
  router surfaces `429` to the agent, not a synthetic 500. Without this
  test, agents lose the signal needed for client-side rate limiting.
* **No task leak under churn** — `agt_relay_repeated_churn_no_task_leak`
  bounds upstream calls to exactly the script length.
* **Single-flight on token refresh** — `entra_single_flight_one_network_call`
  exercises 16 concurrent refresh attempts and asserts exactly **one**
  network call lands on Entra.

## Performance baselines

| Surface | Bench | Baseline | Regression gate |
|---|---|---|---|
| Synthetic controller decision | `controller/benches/reconciler_bench.rs` | trusted event base, measured in the same job | candidate ≤ baseline + 25 % |
| Synthetic router proxy work | `inference-router/benches/proxy_bench.rs` | trusted event base, measured in the same job | candidate ≤ baseline + 25 % |
| End-to-end router smoke | `tests/k6/router_smoke.js` | inline (`p95 < 100 ms`, `err < 0.1 %`) | nightly only |

`Bench Regression` measures both revisions sequentially on one Ubuntu 24.04
runner, with Rust 1.99.1 and an explicit identical bench profile (thin LTO,
opt-level 3, controller codegen-units 16, default codegen-units 1). Production
release profiles are not changed. Both lockfiles are enforced with `--locked`;
Criterion declarations and locked versions/checksums must match. A harness
migration fails closed rather than comparing different measurement tools.

For PRs the baseline is the event's base SHA, and the candidate is the checked-out
merge SHA. For pushes the baseline is `before` and the candidate is `after`.
Manual CI and legacy Release dispatches require `benchmark_base_sha`, an explicit
full ancestor commit SHA. Reusable callers forward that input on manual runs;
they otherwise retain the originating PR/push event identity. Missing/all-zero
bases, wrong repositories, unavailable commits, self-comparisons and non-ancestor
bases (including divergent force pushes) fail closed. There is no fallback to
`HEAD~1`, a branch name, or a historical timing snapshot.

CI calls:

```bash
python3 ci/bench_regression.py <base-policy.json> <base-criterion-dir> <candidate-criterion-dir>
```

The base commit's `benches/baselines.json` supplies the required benchmark names
and unchanged 25% threshold. Its historical timing values remain untouched and
are **not** rewritten or used as cross-machine ceilings. Each revision/component
writes to its own fresh `CRITERION_HOME` under `bench-evidence/`, separate from
restored build caches. The gate reads `new/benchmark.json` identities and full-precision
nanosecond medians from `new/estimates.json`. Bencher text remains diagnostic only:
its integer nanosecond formatting truncates subnanosecond operations to zero.
The gate does not round, clamp, omit fast operations, or change the 25% threshold.
Missing, duplicate, extra, nonnumeric, nonfinite or nonpositive measurements fail
closed. Compilation and pipeline failures
also fail the job. Both comparison steps run after successful measurements even
when one reports a regression. The `bench-comparison-<run>-<attempt>` artifact
retains raw Criterion estimates/samples, logs, policies, SHAs, compiler/profile
and CPU details, including failures.

These are synthetic microbenchmarks, not production reconciler latency or router
p99 measurements, and do not establish customer acceptance.

## Adding a new chaos case

1. Identify the production reliability invariant under threat. If the
   invariant is not in `tests/chaos/README.md`'s table, add it.
2. Add a test in the most relevant `tests/chaos/tests/*.rs` file.
   Annotate with `#[tokio::test(flavor = "multi_thread", worker_threads = 2)]`
   when the test does any HTTP IO; HTTP requests against the in-process
   axum mock require a real reactor.
3. Use `harness::ChaosScript` to script upstream behaviour. For
   retry-respect tests use `Retry-After=1` (sub-second wall clock; the
   tier should stay under 5 s of total wall time).
4. Assert the **specific** invariant: no panic, bounded calls, eventual
   convergence, or correct status propagation. Do not assert "no
   error" — those assertions decay.
5. Update `tests/chaos/README.md` and this document.

## Anti-patterns

* **Do not** depend on real Azure / GitHub services. Every upstream is
  mocked.
* **Do not** sleep with real `tokio::time::sleep` for retry waits when
  you can avoid it; if you must, keep the value ≤ 1 s.
* **Do not** chase end-to-end controller / router wiring in chaos
  tests. The chaos tier protects HTTP-layer invariants. End-to-end
  wiring lives under `tests/e2e/`.
* **Do not** add chaos tests behind any feature other than `chaos`.
  CI's PR-blocking signal depends on `--features chaos` toggling the
  full set on / off atomically.
