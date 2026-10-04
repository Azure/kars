<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Observability

Bridge surfaces three complementary views of a run and the fleet, all from real
telemetry — never fabricated.

## Efficiency engine (Insights)

**Operator Console → Insights** is a live (8s auto-refresh), rich efficiency +
governance view computed from real runs:

- **Outcome funnel** — attempted → delivered → human-accepted (the honest signal
  is *accepted*, not merely "tokens were spent").
- **Efficiency frontier** — a Pareto scatter of routes by cost (tokens per
  delivered outcome) vs delivery success, with the recommended route ringed. Plus
  an A/B head-to-head of the top two routes on the same outcome metrics.
- **Reliability** — `pass^k` across packages run more than once (honest about `k`
  and sample size), and top-fault attribution.
- **Latency** — p95 wall-clock + mean time-to-first-action.
- **Governance integrity** — receipts issued, tamper-evident log size, over-reach
  attempts blocked.
- **Budget utilization** — today's measured cluster spend vs the
  [inference-budget](inference-budgets.md) caps, with live meters.

## Agent / org graph

The Activity tab renders a radial graph of a run: the principal hub, the tools it
called, the external hosts it reached, and — when it delegates — each **sub-agent**
it spawned as a branch node with its own live activity.

When a run has sub-agents, an **Org activity** roster makes the delegation tree
legible: every agent (principal + subs) with its role badge, live phase, per-agent
tool-call count, and hosts reached — the "who did what".

## Revision-bound router observations

A delivered Mission can show **runtime-forwarded router observations** beside its
recorded producer and run. OpenClaw captures the scope and round IDs returned by
the shared inference router, requests only those recorded rounds, and forwards
them through the existing successful-delivery artifact path. Bridge binds the
projection to the current Task UID and run; a later revision does not inherit an
earlier revision's observations.

- `kars-router-observations.json` is **system evidence**, not a deliverable. It
  does not turn a writing task into a code review or replace useful output files.
- A router round is an upstream request, not necessarily an agent turn. Only
  returned response IDs are selected; hidden retries, failures before a response,
  and adapters that cannot forward IDs may be absent.
- Tool proposals are not execution results. A correlated result identifies both
  its proposal round and the later reporting round; ambiguous reused call IDs
  are not attributed. Reported outcomes are not independent tool-success proof.
- Missing token usage remains unavailable, not zero. Missing rounds, recorder
  eviction, truncation, and read failures are shown explicitly.
- These observations are not independent authorship, approval, or cryptographic
  attestation. Existing admission and immutable artifact custody establish the
  producer/run binding.

The bounded attachment is collected only after successful negotiated `text-v1`
execution. It never displaces useful files when the artifact allowance is full.
Runtimes without returned IDs produce no attachment; retained deliveries are not
backfilled. The shared recorder serves multiple runtimes, but this forwarding
integration is currently OpenClaw-specific. Existing nonempty legacy evidence
remains visible separately.

Downloads stay pinned to the selected run. Observation JSON contains selected
metadata, not prompts, tool arguments/results, credentials, or request URLs.

## Proofs & attestations

The graph also visualizes **where and when** a run's cryptographic proofs are
made, from real data:

| Proof point | When | From |
|-------------|------|------|
| **Trust envelope signed** | at admission | the envelope digest (`status.envelopeDigest`) |
| **Agent mesh identity (DID)** | at registration | the AGT registry DID + reputation |
| **Sub-agent attenuation verified** | at spawn | each sub-agent's envelope proven a strict SUBSET of the principal's |
| **Governance receipt (DSSE)** | at delivery | the receipt scheme + key id + inclusion-log sequence |

Each point shows ✓ when produced in the run. This makes the otherwise-invisible
signing points concrete — how trust is established, end to end.

## Live stream

One SSE stream per run (`/api/namespaces/{ns}/tasks/{name}/stream`) feeds both the
graph and the per-round/per-tool feed. The BFF aggregates the principal's trace
and every spawned sub-agent's, tagging each event with the emitting agent and its
role; the client de-dupes on the router-stamped `seq` so an event never
double-counts across the persisted seed and the live tail.
