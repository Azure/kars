# Capability audit — Scoped router governed services

Date: 2026-09-08
Status: **Source audit approved under explicit maintainer delegation**;
current-base build and native integration qualification remain mandatory.

## Current delegated approval (2026-09-10)

The maintainer explicitly authorized publication sign-offs after additional
focused review rounds. The authorization is recorded in
[comment 5615522306](https://github.com/Azure/kars/pull/551#issuecomment-5615522306).
The AI attestation below is disclosed as delegated review, not a claim that a
second human reviewed or signed this change.

The earlier cancellation and semantic-failure findings were repaired and
source-reviewed as described below. A fresh focused AI review was requested
for `e711cb69e5462d351e5f24e46eed062bf6cd5980` against SRE prerequisite
`8618e9f45d71f3223f35788c6233ebcf56ca5587`, covering scoped router services,
private control-credential issuance, current-scope reset/wait boundaries and
privacy/consumer-retirement behavior. It reported no security vulnerabilities
in the reviewed changes. The later `fd7e646e` forward changes test ordering
and its evidence only; its production controller, router, shared and Helm
sources are unchanged from `e711cb69`.

The restack preserves the services layer's stronger Pending/current-epoch/
control-version checks. It does not restore the weaker prerequisite-only
availability behavior, disable governed services, or authorize credentials
while privacy is pending. The SRE dependency now contains the reviewed
ReplicationController boundary and staging/retirement compatibility repairs.
Its actual legacy/fresh SRE acceptance passed in the 164-pass run whose single
remaining failure was the independently repaired asynchronous smoke lookup.
That scoped evidence is not success of this services composition.

The fresh review was static AI review, not an independent native test run.
Local restack checks passed Rustfmt, Helm lint, 107 Python harness cases and
source gates. Current exact-head Rust, private service and full Kind acceptance
must still succeed; no unsuccessful check or new finding is waived. Actual PR
merges target `kars-bridge` only after the prerequisite lands, never its
intermediate feature branch. No `main` or customer/H100 deployment is approved.

Signed-off-by: pallakatos (maintainer delegation recorded above) <lakatos.toth.pal@gmail.com>
Signed-off-by: GitHub Copilot (delegated AI audit, not an independent human) <223556219+Copilot@users.noreply.github.com>

## Original blocking finding and dependency requirement

The supported legacy SRE installation grants its agent-held Kubernetes
credential cluster-wide Secret reads. It can therefore obtain the purportedly
operator-only service token through the API. Router-only mounts do not close
that path. The separately reviewed operator-controlled UID registration,
migration and SRE credential boundary is now included as a prerequisite;
its full composed acceptance remains required
before deployment or merge. Legacy grants cannot be retired merely by
trusting names, labels or ownership-looking annotations.

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

Two MEDIUM review findings have been repaired and source-reviewed separately:
the cancellation/expiry race across an asynchronous policy check, and semantic
model errors reported as completed generations. Dispatch now has a mutex-
coordinated claim boundary; cancellation/reset cannot acknowledge prevention
after that claim. HTTP acceptance remains distinct from semantic failure or
incompleteness, with no response rewriting or replay.

Repair qualification passed 1,071 router unit tests and 29 governed-service
integration tests, including deterministic cancellation/reset/expiry/shutdown
interleavings and accepted failed/incomplete/error-plus-DONE responses. Strict
router all-target Clippy passed; the focused 12 telemetry integrations passed
again after a type-safe initializer adjustment. These results do not resolve
the separate HIGH SRE credential-access issue.

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

Source approval is recorded above under the maintainer's explicit delegation.
Publication remains pending exact-head technical and native qualification;
historical results in this document must not be represented as a passing
current-base composition.
