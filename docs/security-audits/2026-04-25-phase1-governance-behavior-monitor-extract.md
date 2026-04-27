# 2026-04-25 — phase1/governance-behavior-monitor-extract

## Summary

Pure refactor. Extracts the in-process `BehaviorMonitor` (plus its
private `BehaviorState` companion) from
`inference-router/src/governance.rs` into a new top-level module
`inference-router/src/behavior_monitor.rs`. `Governance` now imports
`crate::behavior_monitor::BehaviorMonitor`. No call-site changes —
the type's public surface (`new`, `record`, `alert_count`,
`alerts_detail`) is unchanged.

Companion to `phase1/governance-rate-limiter-extract` (PR 44). Both
are precursors to `governance.rs`'s plan-§4.2 end-state ("pure
provider dispatch after full AGT provider landings").

## Threat model delta

None. No behaviour change. The fallback monitor has the same
threshold semantics, the same per-agent isolation, the same alert
data shape. Authoritative cross-mesh anomaly detection still lives
in AGT (`BehaviorMonitor` per internal Phase 1 plan §1 §1.1); this
in-process detector is the local fallback.

## OWASP mapping

- **OWASP LLM Top 10 v2.0 — LLM06 (Sensitive Information
  Disclosure):** capability-denial counters are part of the signal
  fed to anomaly alerts; tampered behaviour can trigger isolation
  before secrets escape. No control changes.
- **OWASP MCP Top 10 — MCP-04 (Tool Saturation / DoS):** burst
  threshold catches volumetric abuse; counterpart to the
  rate-limiter from PR 44. No control changes.
- **OWASP MCP Top 10 — MCP-09 (Behavioral Drift):** consecutive-
  failure counter detects compromised-agent or jailbroken-prompt
  patterns. No control changes.

## AuthN / AuthZ path

Unchanged. `BehaviorMonitor` consumes `agent_id: &str` from the
caller (governance engine), which already authenticated the request.
No AuthN/AuthZ logic moved.

## Secret + key custody

None. The behavior monitor holds no secrets; per-agent state is
counters and timestamps only. No PII recorded.

## Egress surface delta

None.

## Audit events

Unchanged. Anomaly alerts are surfaced via `alerts_detail()` to the
caller, which is responsible for routing them to AGT AuditLogger
(unchanged).

## Failure mode

Unchanged. `record()` returns `true` when **any** of the three
thresholds (burst, consecutive failures, capability denials) is
exceeded; caller decides whether to short-circuit, quarantine, or
just log.

## Negative-test coverage

`inference-router/src/behavior_monitor.rs` ships five unit tests
covering both happy and adversarial paths:

- `first_success_no_alert` — sanity baseline; clean state.
- `consecutive_failures_trip_threshold` — denial path #1; ensures
  the anomaly fires *after* threshold is exceeded, not at exactly
  the threshold.
- `success_resets_consecutive_failures_but_not_capability_denials`
  — protects the persistent vs. resettable distinction; matters for
  detection of burst-then-recover patterns where capability denials
  must accumulate even across intermittent successes.
- `burst_threshold_trips_on_high_volume` — denial path #2; volumetric
  bound.
- `agents_isolated_in_state` — isolation invariant; one bad agent
  does not flag a quiet neighbour.

The full test suite still green: **595 lib tests** (was 590; +5
from the new module). Clippy clean.

## Vendored / third-party dependency delta

None. No new crates, no version bumps.

## Sign-offs

- Capability author: Pal Lakatos-Toth — `Signed-off-by: Pal Lakatos-Toth <pallakatos@github.com>`
- Independent reviewer: Pal Lakatos-Toth (single-reviewer carry-over per
  Phase 1 hotspot-pass2 governance; full second-reviewer roster is a
  Phase 0 §6 deliverable still in progress) — `Signed-off-by: Pal Lakatos-Toth <pallakatos@github.com>`

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
