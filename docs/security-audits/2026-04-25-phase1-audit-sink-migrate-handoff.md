# Security Audit: `phase1/audit-sink-migrate-handoff`

**Capability:** migrates 13 audit-emit call-sites in
`inference-router/src/routes/handoff.rs` from the direct
`state.governance.audit.log(...)` field access to the four-seam
[`AuditSink`] trait via `routes::audit_events::handoff_event(...)`.

**Type:** call-site migration. **Backend is unchanged** — the in-tree
`impl AuditSink for Governance` writes onto the same hash-chained
`agentmesh::AuditLogger` the legacy calls used. One chain, one set of
receipts.

## 1. Summary

Before this PR, `handoff.rs` had two flavours of audit emission:
1. `routes::audit_events::handoff_init(...)` — the one site already
   migrated under `phase1/audit-sink-in-tree`.
2. `state.governance.audit.log(&state.sandbox_name, action, details)`
   — direct field access bypassing the trait, 13 sites.

Mixed shape made the trait less useful as a contract: a future
alternate `AuditSink` implementation (e.g., `AgtAuditSink` backed by
the AGT SDK's remote receipt service, or a `NullAuditSink` for
dev-only manifests) would have been bypassed by the 13 legacy sites.

This PR routes all 13 through `routes::audit_events::handoff_event()`,
a thin async helper that:

- Builds a structured [`AuditEvent`] (`timestamp_ms`, `principal`,
  `action`, `payload_digest_hex`, `verdict`, `labels`).
- Calls `state.audit_sink.append(event)` (the four-seam trait).
- Logs sink errors at WARN but does **not** fail the caller — handoff
  state has already mutated by the time we audit it, and rejecting the
  request because the sink is unreachable would be a denial-of-service
  vector against the sink. Strict-mode fail-closed semantics belong on
  *policy* decisions, not on audit emission post-mutation.

The legacy `governance.audit.log` is now used only inside `Governance`
itself (private, no external callers).

## 2. Threat model delta

**No change.**

- Same backend (in-memory hash-chained log via `agentmesh::AuditLogger`),
  same receipts, same chain ordering.
- Same set of audit events emitted (action labels are byte-identical).
- Same data exposed in each event — `details` is now placed in the
  `labels: [("detail", details)]` field instead of squashed into the
  legacy `decision` string. The `audit_impl::event_to_legacy_args`
  flattener re-squashes labels into the chained record, so the on-chain
  representation is equivalent.
- New WARN log on sink-error path. Information disclosed: action name +
  formatted error. No principal or payload data.

## 3. OWASP mapping

- **OWASP LLM Top 10 v2.0 — LLM05 Improper Output Handling:** unchanged.
- **OWASP MCP Top 10 — MCP04 Unsafe Tool Output:** unchanged.
- **CIS-style auditability:** **strengthened** — all handoff audit
  events now flow through a single, swappable contract, making it
  possible to add tamper-evident remote sinks (AGT receipt service,
  immutable Azure Tables, etc.) by swapping `AppState.audit_sink`
  without touching `routes/handoff.rs`.

## 4. AuthN / AuthZ path

Unchanged. Audit emission is not authn/authz-relevant.

## 5. Secret + key custody

Unchanged. No secrets or keys are produced, consumed, or logged. The
only label key passed today is `detail`, whose value is the same
operator-friendly free-form string the legacy path used (e.g.,
`size=128B hash=abc1234...`).

`token_hash` labels (in `handoff_init`) continue to be truncated to 16
chars. No raw tokens leak.

## 6. Egress surface delta

None. The in-tree `AuditSink` impl writes to a local
`Mutex<Vec<AuditEntry>>`; no network calls.

## 7. Audit events emitted

The 13 migrated `action` labels (byte-identical to before):

| Action | Site | Verdict | Detail label payload |
|---|---|---|---|
| `handoff:snapshot` | snapshot create | info | `size={n}B hash={hex16}` |
| `handoff:restore:failed` | decryption error | info | `decryption_error={e}` |
| `handoff:restore:rejected` | blob too large | info | `blob_too_large size={n}B` |
| `handoff:restore:sanitized` | chat snapshot sanitization | info | `chat_sanitized original={n}B sanitized={n}B` |
| `handoff:restore:sub-agent` | sub-agent re-spawn | info | `respawned={amid} original_amid={amid}` |
| `handoff:restore` | restore success | info | `from={amid} size={n}B` |
| `handoff:verify` | hash verify | info | `hash={hex16} match={bool}` |
| `handoff:abort` | aborted | info | `aborted_from_phase={phase}` |
| `handoff:succession` | registry succession | info | `predecessor=… successor=… registry_status=…` |
| `handoff:pending` | pending created | info | `direction={d} reason={r}` |
| `handoff:pending:rejected` | rejected | info | `{error_string}` |
| `handoff:confirmed` | user confirmed | info | `direction={d} reason={r} token_hash={hex16}` |
| `handoff:confirm:rejected` | confirm failed | info | `{error_string}` |

`verdict = "info"` is used as the neutral default (these are observation
records, not policy decisions). Plus the pre-existing `handoff:init`
event (verdict = `"success"`) from the prior migration.

## 8. Failure mode

Audit-sink append is now non-fatal:
- Success: receipt is recorded in the chain.
- `AuditError::Unreachable` / `Internal` / `QueueFull`: WARN log,
  request continues.

This **differs** from the legacy direct-field path's silent success: any
sink error now produces a log line, which is strictly better. The
in-tree impl never returns these errors today (the `Mutex<Vec>` is
local), but switching `audit_sink` to a remote impl tomorrow will
surface failures correctly.

Strict-mode fail-closed for audit append is **not** enabled here.
Rationale: the handoff state machine has already advanced phase by the
time we audit it; rejecting the response on audit-sink failure would
desynchronise the predecessor and successor without giving anyone a
recovery path. Strict-mode applies to the [`PolicyDecisionProvider`] —
*before* the state mutation, not after.

## 9. Negative-test coverage

- `tests/agt_governance_integration.rs`: existing 26 tests around
  `Governance` audit chain continue to pass — the in-tree
  `impl AuditSink` is the same backend they covered before.
- `audit_impl.rs` unit tests (17): cover idempotency, label flattening,
  `get` round-trip, ISO-8601 parsing. Continue to pass.
- 376 workspace tests still green.

The trait-level negative tests (`AgtAuditSink` outage / `NullAuditSink`
admission) belong on follow-up branches landing those impls; they are
not gated by this migration.

## 10. Vendored / third-party dependency delta

None.

## 11. Sources / verification (§0.2 #10)

- `inference-router/src/providers/audit.rs` — `AuditSink` trait surface.
- `inference-router/src/providers/audit_impl.rs` — in-tree impl on
  `Governance`, including `event_to_legacy_args` flattener that keeps
  on-chain representation identical to the legacy path.
- `inference-router/src/routes/audit_events.rs` — new generic
  `handoff_event(state, action, details)` helper.
- `inference-router/src/routes/handoff.rs` — 13 call-sites migrated;
  LOC unchanged at 1570 (within `ci/loc-budget.yaml` cap).
- `agentmesh::AuditLogger::log` (vendored, used by `audit_impl`) —
  confirmed the SDK function signature is `log(agent_id, action,
  decision)` and that the trait impl preserves the `(agent_id, action,
  decision)` triple by flattening labels into `decision`.

## 12. Sign-offs

Signed-off-by: GitHub Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
