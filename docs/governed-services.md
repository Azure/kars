# Router governed services

These APIs provide an in-process capability-request queue and bounded router
telemetry. They do **not** deliver assignments, run agents, create approvals,
grant capabilities, install resources, or provide a durable execution ledger.

**Qualification gate:** the combined source includes the operator-authorized
[SRE identity/migration prerequisite](how-to/sre-authority.md). Its real-API
migration acceptance (#551) remains pending; merging its implementation locally
does not establish successful hosted qualification or make this candidate ready
for deployment. Changing mounts alone does not establish operator-only authority.

## Identity and operator authentication

Each router has a unique process-instance scope. On Kubernetes the controller
projects the current Sandbox name, source workspace, Sandbox UID, and runtime
namespace UID. Task-owned sandboxes additionally carry the task UID,
generation, and **effective authorization digest**, validated against current
task status and its sandbox reference. The envelope-only digest is not used as
authorization, and unsupported finite/shared launch budgets remain rejected.

Operator endpoints require a separate bearer credential. The controller creates
`router-services-admin` in the authorized sandbox namespace and mounts its
`control-token` key **only in the inference-router**, at
`/etc/kars/services/control-token`. The legacy `router-admin-token` is also
available to some agent plugins and is deliberately not accepted here.

Issuance and reuse require the shared live Secret GET/LIST/WATCH denial checks
for the legacy SRE principal. If an SRE registration exists, it must carry current
verified v2 authority: either Ready with its exact source/controller/namespace
identities, enforcing admission and no unsafe token aliases, or fully Retired
with current denial evidence. A Ready registration's privacy epoch is recorded
on newly issued control credentials. No registration is needed for ordinary
standalone installations where the legacy principal is actually denied.

Existing unqualified **controller-owned** credentials rotate with Secret UID/RV
preconditions; matching names or partial ownership labels never authorize
adoption. The Deployment's credential-version annotation restarts cached-token
consumers without changing their image pins, selectors or unrelated annotations.
The controller does not report its credential transition complete while old
version Pods remain, including terminating Pods. Loss of the checked privacy
proof quarantines the owned credential and scales only a proven owned consumer
to zero. Restoring authority requires a new token, not reuse of the potentially
exposed cache. Foreign consumers remain untouched and block automatic recovery.
Initial qualification still pending is not permission to issue or reuse a
credential, but it is not itself evidence of privacy loss and does not stop the
rollout being qualified. Privacy qualification completes only after fresh
denial/v2/UID/epoch/template checks and termination of old-epoch or
old-control-version Pods. Workload availability is separate: the SRE readiness
endpoint itself depends on qualified authority, so it cannot be an input to
that qualification. Canonical SRE's early authorization failure
also performs the ownership-fenced, no-issuance privacy-loss quarantine.
These checks occur during reconciliation; they are not a claim of instantaneous
cluster-wide revocation or cancellation of already accepted upstream work.

Standalone operators can provide `KARS_SERVICES_ADMIN_TOKEN` (32–256 nonblank
ASCII characters). Without a control credential, operator APIs return 503;
there is no fallback to the legacy admin token or localhost exemption.
`ROUTER_ADMIN_ALLOW_IPS`, when configured, also restricts authenticated operator
origins. Agent APIs accept only the same-router loopback client or an
authenticated allowed operator. They never accept a caller-selected Sandbox.

## Request lifecycle

| Endpoint | Caller | Purpose |
|---|---|---|
| `GET /v1/access-requests` | Agent | Discover the current scope and inspect own requests |
| `POST /v1/access-request` | Agent | Queue a capability request, never a grant |
| `POST /v1/access-requests/{id}/cancel` | Agent | Cancel its pending/approved wait |
| `GET /v1/access-requests/{id}/wait?scope_id=…&timeout_ms=…` | Agent | Wait for a decision or lifecycle transition |
| `GET /internal/access-requests` | Operator | Inspect scope and the `entries` queue |
| `POST /internal/access-requests/decision` | Operator | Record one immutable approval/denial |
| `POST /internal/access-requests/reset` | Operator | Start a fresh scope and cancel old waits |

A request body contains `scope_id`, `kind`, `target`, and an optional `reason`.
Kinds are `egress`, `tool`, `skill`, `mcp`, `command`, `permission`,
`clarification`, and `tier`. Targets are bounded machine identifiers; egress
uses a bare DNS hostname plus a separate `port` (default 443). A tier request
uses `tier: 1..5` and an empty target. Reasons are explicitly submitted
untrusted text, limited to 512 bytes; they are not copied from proxied API
bodies or written to diagnostic logs. Never put credentials in a reason.

The service generates `request_id`. Decisions require that exact ID and its
`scope_id`, with `verdict: approved|denied`. There is no decision-by-host
fallback. Deduplication includes port and tier, preserves the original payload
and expiry, and never inherits a decision for a different capability.

Requests live for 15 minutes. The default queue is 64 entries and creation is
limited to 32 attempts/minute, including duplicates. Requests cannot evict
other pending requests when full. Stale scopes, expired requests, and repeated
terminal decisions are rejected. Cancelled requests retain any original
operator decision separately; cancelling a wait does not revoke policy.

Reset requires the current `scope_id` as a compare-and-swap condition and can
include an `assignment_id` correlation label. It returns a fresh scope with
the same trusted identity, clears request/telemetry state, and cancels old
waiters. Reset returns conflict while an approved dispatch is already claimed.
An assignment label is not evidence that a runtime executed anything.
Reset neither changes policy nor resets existing token-budget counters.

## Policy-gated egress waits

Ordinary denied `/egress/fetch` requests still return immediately. A caller can
opt in by supplying `scope_id` and `wait_for_approval_ms` (at most 300,000).
At most 16 service waits can be active; dropping a wait, cancellation, reset,
timeout, or shutdown releases its slot. Operator control routes have separate
concurrency from agent waits and model traffic.

An approved decision **does not open the network**. The fetch resumes only
after the ordinary egress policy also allows that exact operation. The
existing signed-allowlist/controller path remains responsible for enforcement;
private-address checks, normal egress filters, and redirect behavior still
apply. Denial, cancellation, expiry, and reset never bypass those filters.

After the asynchronous policy check, the waiter revalidates the request. An
atomic dispatch claim then coordinates with cancellation and reset immediately
before sending. Cancellation acknowledged before that claim prevents dispatch;
after the claim it returns conflict rather than promising prevention. The claim
stays active through response handling and releases on completion or caller
cancellation. It does not assert that the upstream accepted the request.

Transparent proxy traffic records bounded denied requests and policy
observations, but keeps its existing immediate-denial/retry behavior. It does
not acquire a new implicit human-approval wait.

## Telemetry

| Endpoint | Purpose |
|---|---|
| `GET /telemetry/cursor` | Current `scope_id` and sequence |
| `GET /telemetry/trace?since=N` | Scoped observations; send `X-Kars-Service-Scope` |
| `POST /telemetry/tool` | Complete a scoped, AGT-correlated harness report |
| `GET /telemetry/budget` | Existing per-sandbox counters/limits, not a task budget |

The trace ring retains at most 1,024 events and pages at most 256 at a time.
Follow `has_more` using the returned cursor; retention loss is exposed through
`dropped_events`. Native/model correlation state is separately bounded.

Model `round` events describe **forwarding attempts**, not autonomous agent
turns. Fallback attempts retain their actual configured provider identity.
Missing usage remains null; incomplete/cancelled streams and transport errors
are not successes. Metadata parsing is bounded, and `partial_observation`
identifies incomplete observations. Streams are forwarded unchanged and are
never replayed by telemetry after acceptance.

HTTP acceptance is distinct from semantic completion: buffered Responses with
`failed` or `incomplete` status and OpenAI error frames followed by `[DONE]` do
not become completed generations. Their accepted status, original response
bytes and available usage remain intact, without retaining provider error text.

Only model identifiers, tool names/IDs, status, timing, and reported usage are
retained. Prompts, assistant text, URLs, headers, arguments, and result bodies
are not retained in telemetry. A model-proposed tool has no successful outcome
until an outcome is observed. Harness reports are explicitly labelled
`harness-reported`, not authoritative router execution. A native completion
requires a unique `tool_call_id` registered by an allowed `/agt/evaluate`
request whose context contains the current `scope_id`.

MCP `isError`, JSON-RPC failures, and missing results are distinguished from
HTTP transport success. Telemetry makes no receipt-completeness, durable
budget, shared broker, or billing claim.

## Integration contract for later task delivery

The existing planned reset path remains
`POST /internal/access-requests/reset`, but its caller must use the private
service-control credential and the scoped compare-and-swap body. Do not
restore the old shared-admin-token/unscoped-reset implementation.

A future task controller must compare the returned task UID, generation, and
effective authorization with current live authority before acting on a
request or decision. It must revoke stale assignment-scoped grants through
the real policy controller before reusing a sandbox. Reset is not that
revocation, and this slice intentionally provides no autonomous grant worker.
