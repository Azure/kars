# Security Audit — Phase G P2 #10: Prometheus Metrics Expansion

**Date:** 2026-05-03
**Scope:** Controller observability surface — adds skip-cache-hit
counter and condition-transition counter to the existing
`/metrics` endpoint.
**Closes §14.6 line item:** "Prometheus metrics
(`reconcile_total`, `reconcile_duration_seconds`,
`skip_cache_hits`, `conditions_transitions_total`) on
`:9091/metrics` from the controller (P2 #10)".

## Change summary

Pre-PR the controller exposed:

* `azureclaw_controller_reconcile_errors_total{crd_kind, error_class}`
* `azureclaw_controller_reconcile_retries_total{crd_kind}`
* `azureclaw_controller_reconcile_duration_seconds{crd_kind, outcome}`
* `azureclaw_controller_reconcile_total{crd_kind, outcome}`

This PR adds the two metrics §14.6 P2 #10 calls out and that
`controller-runtime` operators ship by default:

* `azureclaw_controller_skip_cache_hits_total{crd_kind}` —
  incremented at every reconcile pass that observes a
  fully-converged `.status` and skips the API patch via
  `running_status_matches_with_extras`. Lets operators alert when
  the steady-state fast path stops firing (a regression that
  burns API quota).
* `azureclaw_controller_conditions_transitions_total{condition_type, new_status}` —
  incremented inside `preserve_transition_time` whenever the
  helper stamps a fresh `last_transition_time` (i.e. status
  flipped). Lets operators alert on flap rate without parsing
  CR yaml.

## STRIDE delta

| Threat | Pre / Post |
|---|---|
| **I**nformation disclosure via metric label cardinality blow-up | Both new counters use small bounded label sets: `crd_kind` ∈ 8 values; `condition_type` ∈ {`Ready`, `Progressing`, `RuntimeReady`, `Suspended`, `AuditIntegrity`, …} ≈ 8 values; `new_status` ∈ {`True`, `False`, `Unknown`}. Worst-case time-series count for both metrics combined: ~32 — well below any sensible cardinality budget. No PII / namespace / sandbox-name labels exposed. |
| **D**oS via unbounded series via attacker-controlled labels | None of the labels accept user input. `crd_kind` is hard-coded at the call site; `condition_type` is hard-coded in the conditions module; `new_status` is one of three K8s-defined strings. |
| **T**ampering — counter increments are not authenticated | Same posture as the existing 4 counters. The `:9091/metrics` endpoint is only exposed inside the controller pod and behind the cluster-internal Service; not accessible from sandbox pods or externally. |

## Fail-closed semantics

* Both new counters use `LazyLock` registration via the global
  Prometheus registry — same panic-on-collision contract as the
  existing four metrics.
* If recording panicked we would crash the controller pod, but
  `IntCounterVec::with_label_values(...).inc()` is infallible
  given non-empty static label sets, so this is structurally
  unreachable.
* The skip-cache hit path is the steady-state hot path; the
  counter increment is a single atomic add (sub-microsecond) so
  no measurable controller throughput impact.

## OWASP-LLM mapping

Indirect: improved observability of the controller's reconcile
behaviour gives operators a faster signal on **LLM10 (Model
Theft)** posture (rate of `Suspended -> True` transitions) and
**LLM06 (Sensitive Information Disclosure)** posture (rate of
`AuditIntegrity -> False` transitions, once Phase D lands).

## Test coverage

`controller/src/metrics.rs` mod `tests`:

* `record_status_patch_skip_increments_counter`
* `record_condition_transition_uses_independent_label_pairs`
* `skip_and_transition_metrics_render_in_text_format`

432/432 controller tests pass; clippy clean; rustfmt clean.

## Scope deferrals

* **Workqueue depth gauge** (controller-runtime's
  `workqueue_depth`) is not yet wired — kube-rs's `Controller`
  does not expose its scheduler internals publicly, so capturing
  this would require a fork. Tracked under a follow-up.
* **OTel GenAI SemConv traces** for reconcile spans (§14.6 line
  item beyond P2 #10) are out of scope here; tracked under a
  separate Phase G follow-up.

## Verification commands

```sh
cargo test --package azureclaw-controller             # 432/432
cargo clippy --package azureclaw-controller --all-targets -- -D warnings
cargo fmt --all -- --check
```
