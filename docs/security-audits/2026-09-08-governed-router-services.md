# Capability audit — Scoped router governed services

Date: 2026-09-08
Status: Implementation candidate; independent review and sign-offs pending.

## Scope

Bounded access-request services, operator-only reset/inspection/decisions,
explicit policy-gated waits, and metadata-only inference/MCP/governance
observations. Narrow controller changes project qualified identity and a
separate router-only service-control credential.

No task delivery, runtime worker, structural execution plan, GitHub write
service, managed MCP deployment, skill-package installation, memory-mount
suite, provider implementation, or agent transport is introduced.

## Authority and lifecycle

- Operator service controls do not trust localhost or the agent-visible legacy
  admin token. Their separate credential is never mounted in agent containers.
- Scope is server-owned and includes UID-qualified Sandbox/namespace identity;
  task attribution requires current task UID, generation, full effective
  authorization, launch contract, and sandbox binding.
- Reset is compare-and-swap fenced, atomically coordinates request/telemetry
  scope changes, and cancels old waits. Old IDs cannot become new grants.
- Requests, records, IDs, reasons, lifetimes, rates, waits, and telemetry
  correlation/storage are bounded.
- Decisions do not modify permissions. Egress waits require both approval and
  the existing normal policy check; resets do not modify budgets or policies.
- Existing finite/shared/monetary launch-budget restrictions remain intact.

## Observation integrity and privacy

- No prompts, tool arguments/results, request URLs/headers, provider error
  bodies, or credentials are retained in the new telemetry.
- Model proposals have no fabricated success. Harness outcomes are labelled
  as reported; MCP `isError` and dispatch failures remain failures at HTTP 200.
- Missing usage, incomplete streams, cancellations, and retention loss are
  explicit. Accepted traffic is never replayed by the observer.
- Existing provider provenance, typed failure classification and routing
  recovery are unchanged except for attaching optional observations.
- This infrastructure is not a durable execution/receipt ledger or shared
  budget broker. Existing legacy operator APIs are not redefined by this slice.

## Verification

Local qualification completed before independent review:

- 1,067 router unit tests passed, including a real TCP forward-proxy denial
  that queues a scoped request without introducing an implicit wait.
- 24 new HTTP/lifecycle/telemetry integration tests passed. These include a
  live loopback HTTP server, strict credential separation, scope fencing,
  rate/body limits, reset/cancel/timeout, policy-gated wakeup, provider identity,
  fragmented streams, missing usage, MCP `isError`, and transport failures.
- 73 existing qualified provider/governance/egress/guardrail integration tests
  passed unchanged apart from construction of new optional observer state.
- Four controller projection tests passed, including current full task
  authorization/UID checks and continued rejection of unsupported launch budgets.
- Strict affected-crate Clippy and formatting checks passed.
- Static A2A-isolation, null-provider, and tracked-source copyright checks
  passed; new candidate files are also checked for headers and module limits.

The existing disposable Kind harness now includes the actual deployed router:
it checks that the control Secret is mounted only in the router, rejects the
agent-visible token even through loopback port-forwarding, compares returned
Sandbox/namespace UIDs with live objects, exercises private decisions/reset,
and rejects stale scopes after reset. Private fixture credentials travel via
curl's stdin rather than command arguments or logs. The port-forward process
and named scratch files are cleaned up on exit. Shell syntax passed locally;
hosted execution of this added case remains pending.

No active customer cluster, Azure mutation, deployment, or image publication
was performed during preparation. Dependencies were not changed or installed. The root Cargo target
was reused offline/locked with incremental compilation disabled and a guarded
disk reserve. Committed-diff publication gates and independent audit sign-offs
remain the parent publication process's responsibility.

## Verdict

Pending. No reviewer approval or signature is claimed. Genuine author and
independent reviewer sign-offs are required by the publication process.
