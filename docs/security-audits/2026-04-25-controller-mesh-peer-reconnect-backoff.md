# 2026-04-25 — controller mesh-peer exponential reconnect backoff

## Summary

Replaces the fixed 5-second / 10-second post-disconnect sleeps in the
controller's mesh-peer reconnect loop with exponential backoff (5 → 10 → 20 →
40 → 60 s cap) plus ±25 % jitter, and resets the backoff to the floor once a
connection has lived for ≥ 120 s. Targets the AKS reconnect-storm signature
captured in checkpoint `033-aks-mesh-storm-diagnosis.md` (≈ 30 reconnect
cycles in 5 minutes, ~5 s cadence).

## Threat model delta

No new asset exposure. Failure mode shift only:

- **Before:** under sustained relay/registry failure the controller
  reconnected every 5 s indefinitely, hammering the K8s API for Lease
  renewal and the relay WebSocket endpoint. A single transient relay
  outage produced a thundering-herd in multi-cluster deployments.
- **After:** controllers exponentially back off and stagger reconnects via
  jitter, reducing co-ordinated relay load.

STRIDE: **D**enial-of-service mitigation against the relay (we no longer
self-DoS the relay during its own outage). No change to authN/authZ /
confidentiality / integrity / repudiation surface.

## OWASP mapping

- **OWASP LLM Top 10 v2.0 → LLM10 (Unbounded Consumption / Model DoS):**
  controller-side mitigation; we previously contributed to relay-resource
  exhaustion during outages.
- **OWASP MCP Top 10 → MCP10 (Resource Exhaustion):** same bucket, applies
  to the AGT mesh transport layer the relay underwrites.

Control: bounded reconnect rate with exponential escalation + jitter.

## AuthN / AuthZ path

Unchanged. Reconnects re-use the existing controller mesh identity (Ed25519
key persisted in the `azureclaw-mesh-peer-identity` Secret). Lease-based
leader election unchanged. Outage mode: the controller fails closed in the
sense that pairing/offload requests cannot be answered while disconnected;
in-flight requests are unaffected.

## Secret + key custody

Unchanged. No new secret material introduced.

## Egress surface delta

Unchanged. Same single outbound WebSocket to the AGT relay; only the
**rate** of reconnect attempts changes.

## Audit events emitted

Reconnect events now carry structured `tracing` fields:

- `sleep_secs` — the backoff sleep used
- `connection_lifetime_secs` — how long the failed connection was alive
- `prior_backoff_secs` (info, on reset) — the pre-reset value

These flow through the existing controller log pipeline. No new AGT
audit-log entries; this is operational telemetry.

## Failure mode

Fail-closed remains the contract: while disconnected, no pairing or
offload requests are processed (existing behaviour). Backoff escalation
caps at 60 s so the controller never permanently abandons reconnection.
On a stable reconnection (≥ 120 s lifetime) the backoff resets to 5 s so
genuinely-fresh failures don't inherit a long sleep from prior flapping.

## Negative-test coverage

This change is observability/cadence only — no new wire protocol, no new
crypto path. Existing controller `cargo test -p azureclaw-controller`
(136 tests, all passing) covers the reconnect-loop surface that wasn't
modified. No conformance-corpus entry needed.

## Vendored / third-party dependency delta

None. Uses only `tokio::time::sleep`, `std::time::SystemTime`, and
`std::cmp::min`.

## Sign-offs

- Author: GitHub Copilot CLI (Claude Opus 4.7) — 2026-04-25
- Independent reviewer: pallakatos (this PR before merge)

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>


Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
