# Security audit — Phase 1 · Outage-semantics pure-decision module

Audit ID: `2026-04-24-phase1-outage-semantics`  
Scope reference: `docs/implementation-plan.md` §1.3, §0.2 #8 (fail-closed defaults).

## What landed

1. `inference-router/src/providers/outage.rs` — new module:
   * `OutageMode` enum (`Strict`, `CachedRead`, `DegradedDev`) with serde
     `camelCase`, `FromStr`, `Display`, `Default = Strict`.
   * `OutageConfig` struct (`mode`, `cached_ttl`) + `validate_for_env()`.
   * `OutageConfigError` enum (`DegradedDevInProd`, `CachedTtlZero`,
     `CachedTtlTooLarge`).
   * `CachedDecision<T>` with `is_expired(ttl, now)` — fail-closed on
     backwards-clock.
   * `OutageAction<T>` enum (`Deny { mode }`, `ServeCached { verdict }`,
     `AllowWithWarning`).
   * `decide_outage()` — pure, clock-injected decision function.
   * 19 unit tests covering all three modes, cache freshness/expiry,
     backwards-clock fail-closed, serde round-trip, serde rejects unknown
     mode, env-validation rules, TTL bounds.
2. `inference-router/src/providers/mod.rs` — removed the placeholder
   `OutageMode` stub enum; re-exports the real types.
3. `controller/src/providers/mod.rs` — mirrored `OutageMode` enum with
   `from_spec`, `is_dev_only`, `validate_for_env`, and `OutageModeError`,
   plus 4 new tests.

No call-site in the router consumes `decide_outage` yet — that lands with
the first AGT provider implementation in Phase 1. Landing the pure logic
first (with tests) locks the semantics before any AGT wiring pressures
them.

## STRIDE

| Category | Applies | Mitigation / note |
|---|---|---|
| **Spoofing** | N/A | No identity/auth surface. |
| **Tampering** | Partial | `OutageMode` serde refuses unknown variants → a manifest can't smuggle a novel mode through the router. `is_expired` refuses when the clock has moved backwards, preventing a skewed clock from extending a cache window. |
| **Repudiation** | N/A | No log/audit writes here; call-site will log the `OutageAction` with the mode returned by `Deny { mode: … }`. |
| **Information Disclosure** | N/A | Pure data. No payloads touched. |
| **Denial of Service** | Partial | `CachedRead` + max TTL (15 min) caps how long a cache can absorb an incident. Zero-TTL rejected at validate. `Strict` still fails closed even with a fresh cache. |
| **Elevation of Privilege** | **Yes** | `DegradedDev` is the one mode that lets a call succeed without a provider decision. `validate_for_env(false)` rejects it, matching the null-provider VAP logic from Phase 0. Controller + router both enforce. Default is `Strict`. |

## Principle mapping

* §0.2 #1 — zero regressions: no existing path consumes `OutageMode` yet;
  the stub enum it replaces was never read. Mesh/policy/audit/signing
  modules still compile; `cargo test --all`: **105 + 155 + 15 + 26 + 3 =
  304 passed**, previous baseline preserved.
* §0.2 #2 — configurable not replacement: mode is a per-sandbox config
  field; vendored still runs under any mode.
* §0.2 #3 — AGT boundary: pure decision fn has zero AGT import surface.
  It takes the *provider error* implicitly (the call-site decides when to
  invoke it) and produces a local verdict.
* §0.2 #4 — LOC: `outage.rs` 360 lines (`wc -l`), well under the 800-line
  default and the Phase 1 router-module cap.
* §0.2 #5 — language: Rust for router + controller, matches policy.
* §0.2 #8 — fail-closed: default `Strict`, backwards-clock treated as
  expired, `CachedRead` without a cache denies, `CachedTtlZero` rejected.
* §0.2 #9 — this audit doc.
* §0.2 #10 — references: `std::time::SystemTime::duration_since`
  semantics cited (Err when the clock has moved backwards) —
  https://doc.rust-lang.org/stable/std/time/struct.SystemTime.html#method.duration_since.

## What was **not** done (deliberate, reads as future work)

* No call-site uses `decide_outage`. That wiring lands in the first AGT
  provider PR (currently blocked on AGT AgentMesh, but AGT Rust SDK
  providers are unblocked per §1.4 — the immediate consumer will be
  `AgtPolicyProvider::decide()`).
* No CRD field `spec.agt.outageMode` yet. The enum string values above
  are what that field *must* accept; the CEL validation for the enum
  lands with the minimal-CRDs branch.
* No router metric names — `outage_action_total{mode=…, action=…}` is
  the planned SemConv-aligned shape but not emitted until the consumer
  lands. Absence of metrics is worse than emitting the wrong metric
  shape; defer.

## Re-audit triggers

* First call-site wired up → re-audit for the specific provider.
* Max TTL constant changed.
* A new `OutageAction` variant added.
* `decide_outage` grows an I/O dep (should not — keep it pure).

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
