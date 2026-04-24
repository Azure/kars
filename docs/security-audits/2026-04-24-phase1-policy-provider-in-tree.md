# Phase 1 — PolicyDecisionProvider in-tree impl on `Governance`

**Date:** 2026-04-24
**Branch:** `phase1/policy-provider-in-tree`
**Scope:** Wire the `providers::PolicyDecisionProvider` four-seam contract (landed as a trait in `phase0/provider-trait-scaffolds`) to a real implementation and migrate one production call-site. No new capability; no new network path; no new secret custody. This closes the §0.2 #8 pseudo-impl hole opened when the trait shipped without an implementation.

## What landed

1. **`impl PolicyDecisionProvider for Governance`** in `inference-router/src/governance.rs`.
   - Translates `PolicyRequest` → `(agent_id, action, extra)` → `Governance::evaluate` → back to `PolicyVerdict`.
   - Called inline (no `spawn_blocking`) — `Governance::evaluate` is synchronous, microsecond-scale, CPU-bound. Rationale documented in the `decide` rustdoc.
2. **Free helpers** `policy_request_to_legacy_args`, `legacy_verdict_to_policy_verdict`, `verdict_to_legacy_json` — pure functions, testable in isolation, no state.
3. **`AppState.policy_provider: Arc<dyn PolicyDecisionProvider>`** populated by coercion from the existing `Arc<Governance>`. Same instance as `AppState.governance` — no duplicate state, no extra lock, no separate metrics.
4. **Call-site migration.** `routes/handoff.rs::sandbox_spawn` now calls `state.policy_provider.decide(req).await` and matches on `PolicyVerdict::{Allow, AllowWithLabels, Deny, NeedsApproval}`. On `Err(PolicyError)` it returns 503 fail-closed (Strict-mode default).

## What *didn't* land (explicitly)

- **No wrapper type.** Earlier drafts introduced `VendoredPolicyDecisionProvider` in `providers/vendored/`. It added no state, used misleading naming (the repo already reserves "vendored" for `/vendor/` patched upstream forks), and was a pure adapter. Removed in this PR; the trait impl lives directly on `Governance`.
- **No AGT-backed provider.** `AgtPolicyDecisionProvider` is a separate future branch — it *does* carry real state (SDK client, tenant config) and so warrants its own type.
- **No migration of every call-site.** This PR migrates only `sandbox_spawn` as the proof-of-life call-site so the seam is actually wired in production. The remaining `state.governance.evaluate(...)` call-sites migrate incrementally; that keeps this PR's blast radius the size of one route while still satisfying §0.2 #8 "wired into call sites, not trait-definition-only".

## Threat model delta

None. Policy evaluation, trust scoring, rate-limiting, audit append, and behavior-monitor telemetry all continue to go through the same `Governance::evaluate` call that existed before. The trait is a typed view over the same function. The `sandbox_spawn` path continues to fail-closed on error (HTTP 503 + `FAIL_CLOSED` label).

| STRIDE | Before | After |
|---|---|---|
| Spoofing | AGT trust score gate in `evaluate` | unchanged |
| Tampering | — | — |
| Repudiation | audit append in `evaluate` | unchanged (same sink) |
| Info disclosure | — | — |
| DoS | rate-limiter in `evaluate` | unchanged |
| Elevation | Allow/Deny verdict mapping | unchanged semantics; mapping is 1-to-1 |

## OWASP mapping

OWASP LLM Top 10 v2.0 and MCP Top 10 items are *not* affected — this PR does not touch prompt ingress, model egress, tool framing, or OAuth. It's a typed-seam refactor plus one call-site rewrite.

## AuthN / AuthZ path

Unchanged. Every seam call lands in `Governance::evaluate` which:
- resolves the agent via `TrustManager`
- runs `PolicyEngine::evaluate`
- stamps `AuditLogger`
- decrements `RateLimiter` counters
- feeds `BehaviorMonitor`

Outage behaviour is unchanged (Strict default). `PolicyError` → HTTP 503 in the one migrated route. `agt.outageMode` wiring is covered in a separate branch (`phase1/outage-semantics`, already on dev).

## Secret / key custody

No new secrets, no new keys. Agent UID 1000 still cannot read anything under `/etc/agt/keys`.

## Egress surface delta

Zero. No new outbound destinations. The trait call is process-local.

## Audit events emitted

The existing `AuditLogger` append inside `Governance::evaluate` fires exactly once per `decide()` call — same as before the refactor. No new event types; no duplication; no suppressed events.

## Failure mode

Fail-closed by default:
- `PolicyVerdict::Deny { reason }` → HTTP 403 with the reason labelled.
- `PolicyVerdict::NeedsApproval { approver, ttl }` → HTTP 202 with the approver + TTL surfaced (consumer polls).
- `Err(PolicyError::*)` → HTTP 503, `FAIL_CLOSED=1` label, no downstream action taken.

The `AllowWithLabels` path is used only when `matched_rule` is populated; the labels are reflected back onto the audit event for traceability. They don't widen authorization — it's strictly additive metadata on an already-allowed request.

## Negative-test coverage

New tests in `inference-router/src/governance.rs::tests`:

| Test | Property asserted |
|---|---|
| `policy_request_to_legacy_args_passthrough` | context + payload digest round-trip into the `extra` JSON under stable keys |
| `policy_request_to_legacy_args_empty_context_none` | empty context + empty digest → `None` (doesn't fabricate a JSON object) |
| `legacy_verdict_allow` | `action: allow` → `PolicyVerdict::Allow` |
| `legacy_verdict_allow_with_rule_becomes_label` | `matched_rule` gets preserved as a `(k,v)` label pair, not dropped |
| `legacy_verdict_deny_with_reason` | reason propagates verbatim |
| `legacy_verdict_deny_without_reason_has_default` | missing reason → non-empty fallback (never empty string) |
| `legacy_verdict_requires_approval` | approver defaults to `"human"`; TTL ≥ 60 s (not zero) |
| `legacy_verdict_unknown_action_is_internal_error` | unknown `action` → `PolicyError::Internal`, **not** a silent Allow |
| `legacy_verdict_missing_action_is_internal_error` | missing `action` → `PolicyError::Internal`, **not** a silent Allow |
| `verdict_to_legacy_json_allow_roundtrips` | Allow ↔ JSON is lossless |
| `verdict_to_legacy_json_allow_with_labels_preserves_rule` | matched rule survives the JSON hop |
| `verdict_to_legacy_json_deny_preserves_reason` | deny reason survives the JSON hop |
| `verdict_to_legacy_json_needs_approval_roundtrips` | approval flag survives the JSON hop |
| `decide_through_trait_matches_evaluate` | `.decide()` output equals the legacy `.evaluate()` verdict for the same input |
| `decide_via_arc_dyn_trait_coercion` | `Arc<Governance>` → `Arc<dyn PolicyDecisionProvider>` coercion works and `.decide()` via the trait object returns a valid verdict |

Existing `tests/agt_governance_integration.rs` continues to pass unchanged (it already exercises real `Governance::evaluate` via HTTP routes).

The two failure-mode tests (`*_is_internal_error`) are the bug-class guard: they ensure an unrecognised legacy JSON shape **never** decays to Allow. This is the class of silent no-op §0.2 #8 exists to prevent.

## Vendored / third-party dependency delta

None. `async-trait` was already a workspace dep; `serde_json` and `tokio` unchanged.

## Vendored-patch audit

N/A — no `vendor/` files touched.

## Sign-offs

### Capability author
**Copilot** — 2026-04-24.
I confirm:
- No TODO/FIXME/unimplemented!/todo!/panic! on any production path.
- No hand-rolled crypto or protocol framing (rule §0.2 #8) — this PR is pure translation between an existing synchronous API (`Governance::evaluate`) and a typed async trait.
- All 6 CI gates pass locally with `BASE_REF=origin/dev`.
- `cargo test --all` passes (350 tests).
- The `unknown action → Internal error` path is explicitly tested and fails the build if it regresses to Allow.

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>

### Independent reviewer
**Pál Lakatos-Tóth** (`pallakatos@microsoft.com`) — 2026-04-24.
I confirm:
- The trait impl does not bypass any existing gate inside `Governance::evaluate` (trust score, rate limit, audit append, behavior monitor) — verified by re-reading `evaluate` and confirming every branch returns through `render_verdict`.
- The `Arc<Governance> → Arc<dyn PolicyDecisionProvider>` coercion shares state — no dual-writer race, no lock duplication.
- `sandbox_spawn` correctly fail-closes on `PolicyError` (HTTP 503), not HTTP 200 with an empty verdict.
- The `providers/vendored/` directory is fully removed and no call site imports it.
- Naming discipline respected: "vendored" reserved for `/vendor/` patched forks.

Signed-off-by: Pál Lakatos-Tóth <pallakatos@microsoft.com>
