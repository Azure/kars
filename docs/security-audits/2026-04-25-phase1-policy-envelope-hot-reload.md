# Security audit — Phase 1 policy-envelope hot-reload core

**Capability:** `inference_router::policy_envelope` — pure-function policy
hot-reload core (snapshot + transition + container) shared by the future
Phase 2 K8s informer reconciler.

**Branch:** `phase1/policy-envelope-hot-reload`
**Date:** 2026-04-25

## 1. Summary

This PR lands the **pure** half of the §7 item 14 hot-reload story for
`ToolPolicy` / `InferencePolicy`. It introduces `PolicyEntry`,
`PolicySelector`, `PolicyEnvelopeSnapshot`, `PolicyChange`, and
`PolicyEnvelope` (a `RwLock<Arc<Snapshot>>` container with atomic
`replace_snapshot` semantics, mirroring `a2a::trust_store::TrustStore`
and `a2a::mandate_trust_store::MandateTrustStore`). The transition
function `apply_change(snapshot, change) -> ApplyOutcome` is total,
deterministic, and side-effect-free. Generation counters are
monotonic and saturate at `u64::MAX`. No K8s, no informer, no SSE,
no network.

The Phase 2 reconciler will translate K8s CR Add/Update/Delete events
into `PolicyChange::{Upserted, Deleted, Reset}` values, run the pure
transition, and publish via `PolicyEnvelope::replace_snapshot`. The
`PolicyDecisionProvider` impl will then read snapshots via
`PolicyEnvelope::snapshot()`. Both halves can land independently
because the seam is the in-memory snapshot.

## 2. Threat model delta

No new network surface, no new secret material, no new trust roots.
Pure data structures and a synchronous lock-protected `Arc` swap.
The only STRIDE category that touches this code is **Tampering**, and
only via the same path that already governs writes to other
hot-reload stores: callers that hold a `&PolicyEnvelope` and can call
`replace_snapshot`. In the prod wiring, that caller is the
controller-side reconciler which itself only acts on
admission-validated CRs. No code in this PR widens that surface.

`docs/threat-model.md` is unchanged because no new asset, channel,
or principal is introduced.

## 3. OWASP mapping

- **OWASP LLM Top 10 v2.0 — LLM05 (Improper Output Handling) /
  LLM07 (System Prompt Leakage):** N/A. This module does not handle
  model output or prompts.
- **OWASP MCP Top 10 — A06 (Insecure Configuration Updates):** the
  `PolicyChange::Reset` variant exists specifically so a full
  resync (informer relist) cannot leave stale entries that pre-date
  the relist. Every `apply_change` call returns a fresh snapshot
  with bumped generation, so observers can detect "I missed a
  reload" via generation gaps. Test
  `generation_saturates_at_u64_max` documents the wraparound bound.

## 4. AuthN / AuthZ path

In-process. Caller is by construction the controller reconciler
goroutine that owns the `Arc<PolicyEnvelope>`. No tenant boundary is
crossed at this layer. AGT outage modes (`Strict`,
`CachedRead`, `DegradedDev`) are enforced upstream of this module:
the reconciler never even calls `replace_snapshot` while AGT is
unreachable in `Strict` mode. This module is a pure data structure;
it has no concept of "AGT down".

## 5. Secret + key custody

None. `PolicyEntry` carries a `serde_json::Value` payload field
which is opaque to this module. The Phase 2 `PolicyDecisionProvider`
impl will define the concrete payload schema. No secrets are stored
inline — secret material referenced by policy is dereferenced at
decision time against AGT, not embedded in the snapshot.

Agent (UID 1000) cannot read this in-memory state at all — it lives
in the router process only.

## 6. Egress surface delta

Zero. No outbound calls. No DNS. No file I/O. Pure CPU + heap.

## 7. Audit events emitted

None from this module directly. The Phase 2 reconciler will emit
`policy.snapshot_replaced { generation, structurally_unchanged }`
via `AuditSink` on each successful `replace_snapshot`. That audit
event is part of the Phase 2 reconciler PR's audit doc, not this
one.

## 8. Failure mode

**Fail-closed by construction.**
- Default snapshot is empty → `select(...)` returns nothing →
  policy decisions on top of an empty envelope deny by default in
  the upstream `PolicyDecisionProvider`. (The provider is responsible
  for that semantic; this module guarantees only that "no entries"
  is the initial state.)
- `RwLock` poisoning panics rather than silently returning a stale
  snapshot. We treat envelope corruption as unrecoverable; the pod
  restarts under K8s, the reconciler relists, and the snapshot is
  rebuilt. This matches `TrustStore` and `MandateTrustStore`.
- `apply_change` is total: every variant produces a valid snapshot.
  There is no path that yields a half-applied state.
- Generation counter saturates instead of wrapping: bounded
  monotonicity is preserved even under absurd churn (test
  `generation_saturates_at_u64_max`).

There is no `provider: null|noop|disabled` path here. The module
exists or it doesn't.

## 9. Negative-test coverage

17 unit tests (`policy_envelope::tests::*`):

- `empty_envelope_yields_empty_snapshot` — initial state.
- `replace_snapshot_visible_on_next_snapshot_call` — atomic publish.
- `arc_view_pins_pre_replace_snapshot` — readers holding an `Arc`
  see consistent state across a concurrent swap.
- `upsert_into_empty_yields_one_entry_and_bumps_gen`
- `upsert_same_version_is_structurally_unchanged` — idempotent
  no-op detection.
- `upsert_different_version_is_a_change`
- `delete_known_id_removes_and_bumps_gen`
- `delete_unknown_id_is_structurally_unchanged_but_still_bumps_gen`
  — generation always bumps so observers never miss an event, but
  `structurally_unchanged` lets downstream skip needless rebuilds.
- `reset_replaces_entire_set`
- `reset_with_identical_set_is_structurally_unchanged`
- `iter_is_sorted_by_id` — deterministic ordering for diffing.
- `empty_selector_matches_anything`
- `tool_selector_requires_exact_match`
- `label_selector_is_subset_match`
- `select_filters_by_selector`
- `drive_envelope_via_apply_change_orchestration` — end-to-end
  reconciler shape against the public API.
- `generation_saturates_at_u64_max` — bounded monotonicity.

This module is too small / too pure for `tests/conformance/`.
The Phase 2 informer-driven reconciler PR will own the conformance
corpus entry "policy change propagates within 5 s and prior-decision
cache is invalidated" (plan §7 item 14, last bullet).

## 10. Vendored / third-party dependency delta

None. Uses only `std::collections::BTreeMap`, `std::sync::{Arc,
RwLock}`, and `serde` (already in the workspace). No new crate, no
new npm package. No vendored-patch table change.

## 11. Sign-offs

This is a Phase 1 close-out PR matching the precedent set by
PR 35 (`a2a::snapshot_rebuild`) and PR 38 (`mandate_trust_store`):
**pure-function, K8s informer plumbing explicitly Phase 2**. The
plan permits this shape (§0.2 principle 8 — "no scaffolding on
production code paths" applies; this module is reachable by no
production caller until the Phase 2 reconciler PR wires it up, at
which point that PR will carry its own audit doc).

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
