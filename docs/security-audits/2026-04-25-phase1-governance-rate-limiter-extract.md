# 2026-04-25 — phase1/governance-rate-limiter-extract

## Summary

Pure refactor. Extracts the local-fallback token-bucket `RateLimiter`
(plus its private `TokenBucket` companion) from
`inference-router/src/governance.rs` into a new top-level module
`inference-router/src/rate_limiter.rs`. `Governance` now imports
`crate::rate_limiter::RateLimiter`. No call-site changes — the type's
public surface is unchanged.

This is the next step in plan §4.2's stated end-state for
`governance.rs`: "Becomes pure provider dispatch after full AGT
provider landings." `RateLimiter` is the in-process fallback (used
when the AGT-side rate limiter is not configured / has degraded);
moving it out of the giant governance file is a precondition for
later collapsing `Governance` to pure provider dispatch.

## Threat model delta

None. No behaviour change. The fallback rate limiter has the same
constructor signature, the same `allow(agent_id)` semantics, the same
runtime-update behaviour, and the same introspection accessors
(`global_rate`, `global_capacity`, `per_agent_rate`,
`per_agent_capacity`).

The authoritative cross-mesh rate-limit enforcement still lives in
AGT (`AgtPolicyProvider` / `AgtRateLimiter`) — see
`docs/agt-boundary.md`. This in-process limiter is a local DoS
guard, not a mesh-wide governance primitive.

## OWASP mapping

- **OWASP LLM Top 10 v2.0 — LLM04 (Model Denial of Service):** the
  in-process rate limiter is one of the controls that bounds
  per-agent and global request rates against the router. No control
  changes.
- **OWASP MCP Top 10 — MCP-04 (Tool Saturation / DoS):** same;
  unchanged.

## AuthN / AuthZ path

Unchanged. `RateLimiter` consumes `agent_id: &str` from the caller
(governance engine), which already authenticated the request. No
AuthN/AuthZ logic moved.

## Secret + key custody

None. The rate limiter holds no secrets.

## Egress surface delta

None.

## Audit events

Unchanged. Rate-limit-deny audit events are still emitted by the
caller in `governance.rs` (and in due course by the AGT-backed
provider).

## Failure mode

Unchanged. `allow()` returns `false` when either the global or the
per-agent bucket is empty; caller decides the user-facing failure
shape (typically HTTP 429).

## Negative-test coverage

`inference-router/src/rate_limiter.rs` ships five unit tests covering
both happy and adversarial paths:

- `first_request_allowed` — sanity baseline.
- `global_capacity_caps_burst` — tighter global bucket short-circuits
  per-agent allowance (denial path #1).
- `per_agent_capacity_isolates_agents` — exhausting one agent's
  bucket does not affect another agent (denial path #2 + isolation
  invariant).
- `update_rates_clears_per_agent_buckets` — runtime config update
  resets prior per-agent throttling state (state-transition
  invariant; matters for hot-reload of `ToolPolicy` /
  `InferencePolicy`).
- `rate_introspection_reflects_constructor_args` — accessor
  semantics; protects metrics/observability surfaces that consume
  these.

The full test suite still green: **590 lib tests** (was 585; +5
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
