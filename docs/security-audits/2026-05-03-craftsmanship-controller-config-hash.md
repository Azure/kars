# Security Audit — Phase G P2 #12: Controller-config hash derivation

**Date:** 2026-05-03
**Scope:** Compute a deterministic 16-hex-char SHA-256 digest of
the controller's runtime configuration; log on startup and
surface as a Prometheus info-metric.
**Closes §14.6 line item:** "Controller-config hash derivation
(env vars → hash field) (P2 #12)".

## Change summary

A new module `controller/src/config_hash.rs` introduces:

* `CONFIG_HASH_INPUTS` — a hard-coded list of 26 env var names
  that contribute to the hash. Adding/removing a name is itself a
  hash change and is called out in this audit.
* `compute_with(inputs, lookup) -> String` — pure, testable hash
  computation. Inputs are sorted, deduped, joined with `=` + NUL.
* `compute_from_env()` — production wrapper reading
  `std::env::var`.
* `azureclaw_controller_config_info{config_hash}` — Prometheus
  `IntGaugeVec`, always set to `1`. Operators query in PromQL via
  `count by (config_hash) (azureclaw_controller_config_info)`.

Wired in `main.rs` immediately after the tracing subscriber init
so the hash appears in the first log line and on `/metrics`
before any reconciler fires.

## Determinism guarantees

* **Stable input domain:** the list is in source, not derived from
  process env iteration.
* **Order independence:** `compute_with` sorts its `inputs` slice
  with `sort_unstable` + `dedup` before hashing.
* **Unset == empty:** missing env vars and env vars set to the
  empty string produce the same hash. This matches the controller's
  operational behaviour — both are treated as "unconfigured".
* **Truncation:** SHA-256 is truncated to its first 8 bytes (16
  hex chars). Sufficient for change detection (collision space
  2^32 for the birthday bound; we expect ≪ 10⁵ distinct configs
  in any cluster's lifetime).

## STRIDE delta

| Threat | Posture |
|---|---|
| **I**nformation disclosure via the hash | The hash is a **one-way** digest; reversing requires brute-forcing 26 env-var values, each potentially long. Operators with `/metrics` access can already read most of these env vars via `kubectl describe pod`. Net: no new disclosure surface. |
| **I**nformation disclosure via the metric label cardinality | Each controller pod emits exactly one series per its current config. On rollover the new hash gets a new series; the old one ages out at scrape-staleness. Worst case: ~5 series per controller restart cycle. |
| **T**ampering — attacker mutates an env var to change behaviour | Out of scope: env vars come from the K8s Pod spec / Deployment, which is RBAC-gated. The hash *exposes* tampering by changing visibly. |
| **D**oS — hash computation cost | Single SHA-256 over <2 KB of input at startup; sub-millisecond. |

## Fail-closed semantics

* `compute_from_env()` is infallible — `std::env::var` errors
  (`NotPresent`, `NotUnicode`) are mapped to empty strings via
  `.ok().unwrap_or_default()`. A non-UTF-8 env var value is
  therefore silently treated as absent. This is the safest
  operational choice: we never crash the controller because of a
  weird env var, and we never spuriously change the hash if a
  non-ASCII byte slips in via a tooling roundtrip.

## OWASP-LLM mapping

Indirect: gives operators a fast supply-chain integrity signal
(**LLM05 Supply Chain**) — if two controller pods report
different `config_hash`, one of them is running the wrong config,
which is an immediate red flag for a partial rollout or
misconfigured CI.

## Test coverage

`controller/src/config_hash.rs::tests`:

* `hash_is_deterministic`
* `hash_is_input_order_independent`
* `hash_changes_when_value_changes`
* `hash_treats_unset_and_empty_as_equivalent`
* `hash_changes_when_key_added_to_input_set`
* `duplicate_inputs_do_not_affect_hash`
* `record_config_hash_renders_info_metric`
* `production_input_list_is_non_empty_and_unique`

440/440 controller tests pass (was 432, +8 new); clippy clean;
rustfmt clean.

## Scope deferrals

* **Per-CR `status.observedHash`** — wiring this hash into a
  per-ClawSandbox skip-cache (§14.6 P0 item 3) is a follow-up; it
  needs a referenced-Secret resourceVersion + canonical-JSON spec
  digest combined with this controller hash. Tracked separately.
* **`CONTROLLER_GIT_SHA` build-time input** — not yet plumbed
  through the build pipeline; the env-var-only digest is
  sufficient for the operational signal §14.6 calls out, and the
  image digest already surfaces independently via
  `kubectl describe pod`.

## Verification commands

```sh
cargo test --package azureclaw-controller             # 440/440
cargo clippy --package azureclaw-controller --all-targets -- -D warnings
cargo fmt --all -- --check
```
