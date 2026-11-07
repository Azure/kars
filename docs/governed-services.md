<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Router governed services

The separate [optional keyless GitHub service](github-services.md) supplies
repository-scoped API/git access and real Actions job logs. Its operator-owned
App enrollment does not widen this request queue or grant decisions.

These APIs provide an in-process capability-request queue and bounded router
telemetry. They do **not** deliver assignments, run agents, create approvals,
grant capabilities, install resources, or provide a durable execution ledger.

**Qualification gate:** the combined source includes the operator-authorized
[SRE identity/migration prerequisite](how-to/sre-authority.md). Qualification
requires successful real-API migration and governed-service lifecycle checks on
the actual candidate; merging an implementation locally is not deployment
qualification. Changing mounts alone does not establish operator-only authority.

The Kind service gate reports failures as `GOVERNED-SERVICES-FAILURE` followed
by a bounded JSON record before removing its private temporary files. The record
contains a fixed stage/category, numeric expected/actual HTTP status, and boolean
credential-presence, identity-presence and scope-comparison results. Status `0`
means no valid HTTP status was recorded, not an authentication denial. Boolean
`false` can also mean that a later check was not reached; interpret it with the
reported stage. Raw command errors, tokens, identities, scope IDs and response
bodies are not emitted. The original failure and owned port-forward/file cleanup
remain mandatory; these diagnostics do not qualify the failing operation.

Native credential acceptance also records failure-only Namespace retirement
metadata, including orphaned `kars-native-*` namespaces after their Sandbox has
been removed. The bounded snapshot retains Namespace name/UID, deletion state,
known lifecycle conditions and finalizers; unknown finalizers are counted, not
printed. Condition messages and API bodies are never retained. Explicit
unavailable/truncated markers prevent interpreting incomplete diagnostics as
successful cleanup. Governed credential projection reads and pre-write rechecks
use the same closed error-category/HTTP-status classifier, including Task owner
and recovery rejection categories. Neither diagnostic changes acceptance or
finalizer behavior. Existing controller process capture retains checked-in panic
source coordinates and fixed stack-overflow signatures, not compiled stack
frames; a stack-overflow abort without a panic location still requires hosted
investigation and is not localized by enabling ordinary panic backtraces.
Namespace capture also records whether its discovery-failure message mentions
`metrics.k8s.io/v1beta1`, and the fixed metrics APIService's closed availability
status/reason. It never emits the message, service endpoints or API error body.
An empty Namespace with discovery failure does not prove which API is unavailable
until this dependency evidence is observed; no finalizers or TLS checks are bypassed.
The Native runtime lane builds the maintained Azure Linux 3 metrics-server recipe
from its exact Core checkout and loads it into all Kind nodes before bootstrap.
Only the Native fixture selects that local image with `pullPolicy: Never`; the
Metrics API remains enabled with the chart's serving and kubelet TLS validation.
Before Helm installation, Native verifies both owned Kind nodes' public kubelet
certificate chains using the shared CLI verifier, then configures their trusted
CA bundle, hostname addresses and host aliases. Node/container identities are
rechecked after certificate collection; an unexpected node, changed identity or
invalid certificate stops installation. Private keys are never read. This fixes
the Kind IP-SAN mismatch without insecure TLS or changing production defaults.
Bootstrap currently waits for the controller, not Metrics API readiness: loading
an image is not proof of working discovery or successful Namespace retirement.
Failure capture reuses the controller's Namespace/ServiceAccount/Deployment/
ReplicaSet/Pod identity checks for metrics-server, then projects only closed
scrape and storage-readiness categories from bounded current/previous log tails.
It rechecks identities before retaining facts; raw log messages, node addresses,
certificates and error bodies are discarded. `MissingEndpoints` alone is not a
kubelet certificate diagnosis, and an empty log tail does not prove health.

The Native lane also runs the existing default-worker Team rebind/projection
regression after the runtime journey, including when that journey fails, using
the same unoptimized Cargo profile as its controller binary. The regression is
mandatory and does not replace live acceptance. Its fixed phase markers can
localize a test failure, not establish the live overflowing frame. After Task
reattestation/resume, the same fixture now polls the real parent Sandbox reconciler
through its metrics wrapper, Namespace ownership and credential rejection path
with injected Sandbox resourceVersion drift. It checks the exact rejection,
error-policy retry, workload pause, projection revocation and unchanged owned
identities before retrying direct projection. Successful parent reconciliation
beyond credential projection and live watcher concurrency remain outside this
regression. H29 reproduced a default-worker stack overflow after
`sandbox-parent-poll`, before `sandbox-parent-returned`. The parent now keeps
post-credential provisioning behind a boxed continuation constructed by a
separate synchronous factory. Credential rejection therefore does not construct
that continuation, while successful provisioning retains the parent's Namespace
lock and authorized inputs. The same hosted regression must prove the repair;
fixed numeric future-size output measures layout, not the overflowing instruction.
No worker stacks, readiness deadlines or security policies change.
Target-recheck diagnostics distinguish bundle reference, Task authority,
metadata/spec, execution-transition and materialized-caller rejection from the
strict pre-write target UID/resourceVersion fence. Late-private controller
reissuance/restore deadlines likewise retain fixed categories rather than being
misreported as unknown CLI failures; they still fail and preserve the receipt.

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

Task-owned sandboxes also receive an identity-bound TLS configuration in that
same private Secret, mounted only in the router. The controller imports pending
requests into the owner Inbox and returns decisions over the dedicated HTTPS
control listener on port 9449. It pins the exact router Pod, Sandbox/Task identity,
credential version and issuing CA; redirects and ambient trust roots are disabled.
NetworkPolicy permits this port only from controller Pods in the operator namespace.
This path does not require enabling optional service observation or giving its
recipients additional access. The observation listener remains read-only;
assignment resets remain on the existing local service-control surface. Missing
or invalid TLS material prevents a task router from starting rather than silently
running without its approval channel. Certificates are reused while current and
renewed before expiry through the existing credential rollout safeguards.

Issuance and reuse require the shared live Secret GET/LIST/WATCH denial checks
for the legacy SRE principal. If an SRE registration exists, it must carry current
verified v2 authority: either Ready with its exact source/controller/namespace
identities, enforcing admission and no unsafe token aliases, or fully Retired
with current denial evidence. A Ready registration's privacy epoch is recorded
on newly issued control credentials and the initial consumer Pod template. This
avoids a second privacy-stamping rollout interrupting newly dispatched work.
No registration is needed for ordinary standalone installations where the legacy
principal is actually denied.

Existing unqualified **controller-owned** credentials rotate with Secret UID/RV
preconditions; matching names or partial ownership labels never authorize
adoption. The Deployment's credential-version annotation restarts cached-token
consumers without changing their image pins, selectors or unrelated annotations.
For admin Secrets that also hold `builtin-memory.json`, this version binds the
Secret UID and a deterministic hash of every other data key. Renewing only the
memory TokenRequest therefore does not restart healthy consumers; changes to
control tokens, TLS configuration or other control material still do. Secret
UID/resourceVersion compare-and-swap and privacy-epoch checks remain mandatory.
Initial memory enrollment returns the post-write control version to the shared
runtime projection before the Deployment is created. New Hermes and OpenClaw
Pods therefore start with that version instead of triggering a false stale-cache
drain when memory configuration is first added.
Admin Secrets without memory configuration retain UID/resourceVersion versions,
including late-enrollment receipts. Late enrollment accepts the controller's
Task-bound `control-token` plus `config.json` TLS transport, as well as existing
token-only runtimes. Before retiring Pods it validates the exact Task identity,
certificate chain, hostname, expiry and matching private key. Qualification
requires both the token and TLS key to rotate under the same Secret UID; receipts
retain only material hashes, never tokens or PEMs. The exact router-only
`builtin-memory.json` mount is supported for direct Task/Sandbox memory ownership,
using the maintained `control-v1` content version. Apply validates memory scope,
CA/expiry, protected account ownership and the live bound Secret UID; qualification
requires a different anchor UID, epoch and token while retaining the memory binding.
JWT claims are checked for consistency, not independently authenticated: issuance
remains the controller/Kubernetes path. Preview reads no Secret values, receipts
retain hashes/anchor identity only, and exact receipt resource-version checks remain
fail-closed on later renewal. Shared reviewed-root/Team memory and customized
mounts still require their owner-specific retirement rather than automatic adoption.
The controller's v4 resume fence accepts the same closed TLS/memory receipt fields
as the CLI. Before restoring replicas it requires the exact qualified configuration
hashes, rotated proofs, unchanged Secret UID, new resourceVersion and the shared
control version in the Deployment template. Unknown data or missing proofs remain
fail-closed; this byte binding does not replace the CLI's certificate and live-anchor
validation. Native runs the existing runtime-apply fixture with these receipt shapes
and negative cases on its normal debug profile/default worker stack after the Team
regression. Neither regression alone establishes live observer or writer acceptance.
Other credential purposes retain their existing version semantics.
The controller does not report its credential transition complete while old
version or mismatched verified-epoch Pods remain, including terminating Pods. Loss of the checked privacy
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
| `POST /internal/access-requests/decision` | Operator | Record one immutable approval/denial or close a reviewed tier request without a grant |
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

For a human-approved tier request whose Task is controller-owned or whose
execution is already launched/consumed, the controller first persists the
`requires-fresh-run` authority outcome on the same UID/digest-bound
`KarsApproval`. It then closes the exact pending queue entry with
`verdict: cancelled` and a nonblank `explanation` of at most 8192 UTF-8 bytes.
This control-only outcome accepts tier requests only, forbids `answer`, wakes
native waiters, and is retained on duplicate requests. It leaves the human
approval Approved, the queue's authority decision unset, and current authority
unchanged; it is not a human denial or permission to replay a consumed run.
The explanation identifies the required fresh authorized run (and source
Team/package promotion for controller-owned Tasks). OpenClaw and Hermes return
this outcome unchanged; policy gates still apply. Invalid/stale approval
bindings cannot produce this outcome. No second approval ticket is opened.

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

Model identifiers, tool names/IDs, status, timing, and reported usage are
retained. New model proposals also carry bounded, credential-redacted argument
previews, including ordinary file/message bodies. Both Team and Mission Activity
use these same previews for OpenClaw and Hermes; historical arguments that were
not captured remain unavailable. Prompts, assistant prose outside tool arguments,
and raw result bodies are not retained.

Argument capture accepts at most 64 KiB of complete JSON per call; malformed,
incomplete or oversized capture produces an explicit unavailable marker. Each
preview is at most 2,048 UTF-8 bytes, with bounded depth, fields and strings.
A recorder scope retains at most 64 KiB of encoded preview content, followed by
explicit omission markers; this limits telemetry retention, not execution or
tokens. Sensitive keys, recognized credential text, known sensitive environment
values and secret-file bodies are redacted. URL credentials, queries and fragments
are omitted. Redaction and truncation are labelled. Pattern-based redaction cannot
identify every arbitrary unlabelled secret: never intentionally place credentials
in tool arguments. Previews are untrusted diagnostic text, not proof of the exact
arguments executed; consumers render them as text and preserve call/round/producer
binding. A model-proposed tool has no successful outcome
until an outcome is observed. Harness reports are explicitly labelled
`harness-reported`, not authoritative router execution. A native completion
requires a unique `tool_call_id` registered by an allowed `/agt/evaluate`
request whose context contains the current `scope_id`.

MCP `isError`, JSON-RPC failures, and missing results are distinguished from
HTTP transport success. Response outcome parsing skips tool content and error
payloads, so large valid results do not create observation gaps merely because
they exceed the request-metadata size limit. Malformed response metadata and
oversized response batches still produce explicit gaps. Telemetry makes no
receipt-completeness, durable budget, shared broker, or billing claim.

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
