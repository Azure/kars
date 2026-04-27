# 2026-04-25 — phase1/governance-trust-ops-extract

## Summary

Pure refactor. Splits `inference-router/src/governance.rs` into a
module:

```
inference-router/src/governance/
    mod.rs        # struct + init + policy/redaction/audit methods
    trust_ops.rs  # update_trust / all_trust_scores /
                  # get_trust_score_json / delete_trust /
                  # report_content_flag (the trust-management impl)
```

The five extracted methods are kept as inherent `impl Governance`
methods in a sibling module file; Rust's "child modules see private
items of ancestors" rule lets `trust_ops.rs` access the few private
fields of `Governance` (`peer_last_seen`) without any visibility
change. `tier_label` is brought in via `use super::{Governance,
tier_label};`.

This finishes the local-fallback-extraction step that started with
PR 44 (RateLimiter) and PR 45 (BehaviorMonitor). With this PR,
`governance/mod.rs` lands **under the Phase 1 cap of 900** for the
first time (837 LOC, baseline 1252).

## Threat model delta

None. No behaviour change. Trust updates still:

- delegate to AGT's `TrustManager` (the authoritative store);
- enforce the ±200 per-update delta clamp + 500 max for new agents +
  self-trust rejection (these checks are the AzureClaw-side wrapping
  of the TrustManager, kept identical);
- emit `audit:trust-clamp` and `audit:trust-update` events to AGT
  AuditLogger via `self.audit`;
- update `metrics::trust_*` counters via `self.metrics`.

## OWASP mapping

- **OWASP MCP Top 10 — MCP-09 (Behavioral Drift) / MCP-07
  (Identity Spoofing):** trust-update path is the gating mechanism
  for which peers count as "verified" — unchanged.
- **OWASP LLM Top 10 v2.0 — LLM07 (System Prompt Leakage) / LLM08
  (Vector Weakness):** content-flag reporting (`report_content_flag`)
  feeds AGT's behaviour monitor with policy-violation signals;
  unchanged.

## AuthN / AuthZ path

Unchanged. Trust-mutating callers in `routes/governance.rs` still
authenticate via AGT bearer or sandbox-internal trust-token; this
PR only re-locates the methods, not the callers' auth path.

## Secret + key custody

None. Trust scores are not secrets; they're public peer-reputation
data (visible via `/agt/status`).

## Egress surface delta

None.

## Audit events

Unchanged. The same audit events with the same payloads are emitted
from the moved methods.

## Failure mode

Unchanged. `update_trust` returns `Err(...)` on self-update or
unknown-agent rejection; callers translate to HTTP 4xx.

## Negative-test coverage

No new tests in this PR — pure refactor. **595 lib tests** continue
to pass, including the in-tree governance tests (`tier_labels_are_*`,
`rate_limiter_*`, `behavior_monitor_*`) that exercise the
trust-related plumbing indirectly via `Governance::new` + methods.

## Vendored / third-party dependency delta

None.

## Sign-offs

- Capability author: Pal Lakatos-Toth — `Signed-off-by: Pal Lakatos-Toth <pallakatos@github.com>`
- Independent reviewer: Pal Lakatos-Toth (single-reviewer carry-over per
  Phase 1 hotspot-pass2 governance) — `Signed-off-by: Pal Lakatos-Toth <pallakatos@github.com>`

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
