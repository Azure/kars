<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Governed inference budgets — v1 contract

**Implementation candidate, not yet qualified for publication.** Only the new
explicit scope can pass syntactic launch validation; materialization and cadence
still require the actual configured broker, immutable account and privacy proof.
Rust qualification and the disposable Kubernetes API gate have not yet run.
See the [security audit](security-audits/2026-09-08-governed-inference-budgets.md).

## What the limits mean

`spec.envelope.budget.scope: GovernedInference` explicitly selects accounting for
inference dispatched through the Kars router. The two ceilings are:

- `tokens`: provider input plus output tokens, including cache input and reasoning
  categories covered by the configured model contract.
- `usdMicros`: **configured maximum inference-price units**, not measured invoices.
  One million units represents one US dollar in the operator's maximum tariff.
  These are deliberately not estimates of total task cost.

Compute, GPU/VM time, tools and MCP services, storage, network, external services,
taxes, currency conversion, and invoice adjustments are outside this scope.
They are **not asserted to be free**. An omitted or zero currency limit remains
unbounded. Token-only accounts require trustworthy token bounds, not an invented
price. No Copilot, local model, or provider is assigned a guessed zero price.

Legacy tasks without the new scope retain the foundation's planning-only
interpretation of positive aggregate budgets. Aggregate limits are never copied
into a per-sandbox daily allowance.

### Router calendar counters

Daily/monthly `InferencePolicy` limits are enforced by the sandbox's router,
not by the controller or the console launch-cap preflight. When the accounting
service is enabled, newly materialized sandboxes receive a separate calendar
account in its namespace. Its UID is sealed into the sandbox namespace.
The same router-only projected token, workload verification and private TLS
transport protect both calendar accounting and lifetime accounting. Agent
containers do not receive that token.

The broker admits at most 64 concurrent TLS handshakes, each with a ten-second
deadline. An unfinished handshake does not serialize otherwise healthy
connections. Rejected and timed-out handshakes release their slots and are logged
separately; listener shutdown drops pending handshakes. This transport bound does
not change certificate verification, reservation authority or fail-closed
accounting, and does not introduce automatic inference or reservation retries.

Governed router readiness uses `/readyz` with a ten-second timeout and a
fifteen-second period. It checks live calendar accounting, the audience-bound
broker authority, model contracts and provider credentials; Kubernetes' default
one-second probe deadline is not an adequate budget for that multi-request
path under load. Slow or failed checks still make the Pod unready. Local
`/healthz` liveness and per-inference authorization checks are unchanged; no
successful authority result is cached or substituted for the live check.

Calendar ConfigMap names retain the full sandbox-UID hash (`calendar-` plus
64 hexadecimal characters). Resource identity validation follows Kubernetes'
253-byte resource-name limit; namespace labels and DNS hosts retain their
63-byte label limits. Existing account names, sealed UIDs and counters are not
renamed or reset.

Calendar commits use checked arithmetic, UID/resource-version preconditions and
idempotent per-router sequences. Reads do not treat missing, replaced or corrupt
accounts as zero. Unavailable accounting returns 503 rather than a quota 429;
telemetry reports an error rather than inventing an unused balance. Writer
receipts are bounded at 256 per account and retained across month boundaries so
a delayed retry cannot be counted twice. Exhausted receipt capacity blocks
accounting; it never discards history to admit another writer.

**Calendar rollout is not yet qualified.** Existing sandboxes without a sealed
calendar account are refused rather than silently initialized to zero: migration
needs a verified drain and final counter transfer. A telemetry snapshot alone,
or the handoff drain flag without provider admission/in-flight tracking, is not
that barrier. A missing Deployment is not proof that the sandbox has never run.
Zero initialization requires a controller-owned, Sandbox-UID-bound fresh-namespace
or explicit next-Sandbox prestage marker and no existing Deployment. The marker
is consumed when the account is sealed, or retired before any legacy execution
without the accounting service. Sealed accounts cannot fall back to volatile
accounting when the service is disabled. Existing or ambiguous unsealed namespaces
require reconciliation; their counters are not inferred or reset.
Do not deploy this change over existing volatile counters without resolving
that migration boundary.
Legacy handoff is also refused for a bound calendar account: it cannot transfer
the sealed account identity or its pending liability.

The router acknowledges a durable Begin immediately before sending supported
Chat Completions, Responses or Anthropic Messages requests. Finish records
complete raw-provider usage, before returning a buffered result or forwarding a
terminal SSE event. This also covers clients that stop reading on the terminal
event rather than waiting for transport EOF. Route-level usage reporting does
not count the same call again.

An unfinished call retains pending liability in the account. Replacement writers
are refused while another writer has pending work; terminating routers can still
settle against their original workload identity. Unknown transport outcomes,
cancelled streams and incomplete usage are never assigned zero tokens. Specific
structured model/API rejections before inference can close a Begin with zero,
preserving API negotiation without treating arbitrary provider failures as free.
Unresolved liability needs trustworthy reconciliation, not deletion of the
account or creation of a fresh balance. Automatic recovery is not implemented.

This remains completed-usage calendar accounting, not the token-amount
reservation protocol below: one call or concurrent calls can cross a calendar
threshold before subsequent admission sees their completed usage. The new
pending-liability and streaming behavior still requires installed qualification.
Without an enabled accounting service, the retained local/file tracker does not
claim pod-replacement durability.

## Console daily launch caps

Console **Policies → Inference budgets** also exposes the separate daily cluster,
workspace and user launch caps. These use measured completed-run usage, not the
root-lifetime accounting described below. A zero daily cap is uncapped. Passive
mode alerts; buffer and strict modes can block new launches.

Budget editors retain entered values and show an error when validation or a save
fails. A timeout or connection failure is an unconfirmed outcome, not proof that
the server rejected the write: check the current budget before retrying. Inline
policy edits display the successfully saved amount rather than the previous
value. Changing these daily caps does not change a mission's governed enrollment.

A stopped Mission feedback revision with verified failed reviewed-phase evidence
can explicitly prepare a retry from its Deliverable review panel. This creates a
fresh request nonce while retaining the exact complete documents and feedback,
review history, Task identity, authority, lifetime account and existing charges.
It does not launch compute: the reviewer still reviews the normal launch checks
in Overview. Pending, active or ambiguous work is not automatically retried;
unavailable or changed failure evidence refuses the action. A write with an
unconfirmed outcome must be refreshed, not automatically replayed.

## Bridge preflight

Package validation accepts `budget_tokens`, `budget_usd_micros`, and explicit
`budget_scope: "GovernedInference"`. Stored-task validation retains all three
fields from the persisted envelope. A positive cap without scope fails preflight;
Bridge does not add scope implicitly or convert an existing UID into first-time
governed enrollment.

Explicit scope reports a warning, not an enforcement pass: Bridge preflight does
not verify broker readiness, provider contracts, available reservation capacity,
or the bound account. Admission, controller materialization and router dispatch
remain authoritative and may reject execution. Missing limits also warn; autonomy
tiers and cluster defaults are not proof of a hard aggregate spending limit.
Home reviews finite token caps as explicit `GovernedInference` enrollment for a
new mission. The same scope and limit travel through package validation and
creation. Invalid, fractional, unsafe, zero or negative supplied limits are
rejected rather than silently discarded; a positive limit without reviewed scope
is rejected too. The creation action also preserves a reviewed currency limit,
when supplied, rather than replacing it with null. Blank limits remain explicitly
unbounded, not an enforcement claim. Existing draft UIDs are not migrated: create
a newly reviewed mission to enroll for the first time.

## Root lifetime and immutable identity

A standalone task tree has one account for its **root Task UID**. A Team and all
its principals, members, and cadence runs share one account for the **Team UID's
lifetime**, not a fresh allowance per run or per day.

The controller pins a `KarsBudgetAccount` name and UID in protected Task/Team
status. Accounts live in the controller namespace, outside the task workspace,
so deleting the workspace cannot silently remove its accounting first.
Bindings include cluster and workspace namespace UIDs, root kind/name/UID, Task
and parent/root Task UIDs, full effective authorization and digest, Sandbox UID,
runtime namespace UID, and Pod UID.

Dispatch authentication retries a changing Task-lineage read inventory at most
three times. Each attempt keeps the original leaf UID/generation and repeats the
full readiness, attenuation and UID/resource-version checks. Persistent churn,
replacement, revoked authority or API denial still prevents inference; this
does not retry provider calls or increase the account allowance.

Account bootstrap is metadata-first. A domain-separated note signed through the
existing controller signing provider proves authorship even after a lost CREATE
acknowledgement; labels alone cannot authorize adoption of a pre-existing ledger.
The root pins the API-generated account UID before initialization. An initialized
ledger is sealed before dispatch. Missing, replaced, malformed, or uninitialized
pinned accounting never becomes a new zero balance.

The public fields are:

- `KarsTask.status.inferenceBudget`: scope, account reference, immutable root and
  ancestry, and the full effective authorization digest.
- `KarsTeam.status.inferenceBudgetAccount`: the lifetime account reference.
- `KarsSandbox.spec.inferenceBudgetRef`: the controller-owned Task binding.

Workspace producers create Tasks/Teams; they do not create accounts, write
balances, manufacture status, or inject this Sandbox field. Root UID recreation
is a new grant identity, not continuation of an old account.

The shared controller `task_identity::resolve` helper captures a stable live
same-workspace UID chain, with canonical readiness, attenuation and effective
authorization, and a verified optional Team owner. It rechecks UID/resourceVersion
after collection. `VerifiedTaskLineage::verify_pins` compares a consumer's
authoritative persisted references; traversal alone does not create immutable
continuity. Budget enrollment owns persistence and financial state. Credential
consumers can reuse identity checks without depending on the budget ledger.

Paused Teams can prepare immutable account/Task bindings without activating their
ledger nodes. New members register dormant, including beneath a dormant parent;
ordinary reconciliation must not undo the recovery worker's subtree closure.
Authority edits remain bounded and inactive while paused. Resuming reactivates
the same ancestry in parent-first order without resetting spending or reopening
closed Pod sessions; runtime enrollment and each provider send still require
their separate live admission checks.

## Reserve, authorize one send, settle

All ancestor meters and the root meter are updated in **one Kubernetes object
status PUT**, with the current UID/resourceVersion and bounded conflict retries.
No router-local cache is balance authority.

1. **Reserve:** hold the complete configured maximum input/context plus the
   request's enforced output maximum, and the corresponding maximum price.
2. **BeginDispatch:** atomically consume the one-time dispatch permission. Only
   the first transition returns permission. A lost acknowledgement does not
   authorize retransmission of that attempt.
3. **Settle:** trustworthy complete provider usage can reduce the reservation.
   Missing, malformed, incomplete, disconnected, cancelled, or uncertain accepted
   work is charged at the full reserved maximum.

The boundary is every **actual final provider/model/transformed-wire send**,
including compatibility translations and each retry/fallback. A broker denial
is not a provider health failure or permission to try another provider.
Output filtering does not refund already-performed inference.

Only provably **undispatched Reserved** attempts expire/refund (30-second maximum).
An InFlight attempt never refunds merely because its TTL, router, Pod, or Task
disappears. Recovery commits stale InFlight maxima after the transport deadline
and a margin. Cancellation closes sessions/subtrees without erasing spend.
Quota held by active reservations, pending enrollment, and catalog/store outages
block **new admissions**, not revoke already funded executions. Team seats/runs
retain their Task UIDs and launch intent during such waits; the Task controller
does not tear down an existing execution solely for a budget-admission failure.
Explicit pause, removed authority, invalid policy and UID replacement retain
their normal fail-closed revocation paths. Every new send still needs the broker.
Normal terminal rows compact behind durable Pod sequence high-water marks.
Uncertain attempts retain their contract as tombstones: late over-bound usage
freezes the account without granting another refund.
Anthropic streams require a valid final usage-bearing `message_delta` with a
recognized stop reason, followed by `message_stop` and clean transport completion.
Start-of-message usage or a terminal event alone never proves final output usage.
Missing, malformed, reset, decreasing or inconsistent final evidence commits the
complete token and maximum-price reservation.

Arithmetic is checked, uses signed-Kubernetes-compatible integer ranges, and
rounds input/output tariff categories upward separately. An observed provider
bound breach freezes the account and preserves the observation. Such a breach
invalidates the configured contract's guarantee; it is not reported as successful
hard-ceiling enforcement.

The full-context reservation is deliberately conservative. A token ceiling
smaller than the model's hard context/output maximum can refuse even a short
prompt; concurrency needs room for all simultaneous maxima. This version does
not substitute a character heuristic for exact pre-dispatch token evidence.

## Account observations

`KarsBudgetAccount.status.phase` (the `kubectl` Phase column) and the standard
`Ready`/`LedgerValid` conditions describe the last observed account state.
They include `observedGeneration`; condition transition timestamps change only
when the corresponding True/False/Unknown value changes.

- **Bootstrap:** the UID anchor is not sealed; it cannot dispatch.
- **Active / LedgerAvailable:** the validated, sealed ledger has headroom.
  This is not router health, provider availability, or permission for a send.
- **Blocked / BudgetReserved:** reservations occupy a declared ceiling.
  Already funded executions and their Task UIDs remain intact.
- **Blocked / BudgetExhausted:** settled or uncertain charges occupy a ceiling.
- **Blocked / AuthorityRevoked** or **AttemptCapacityReached:** enrolled
  authorities are closed, or bounded attempt storage has no room.
- **Frozen / ContractBreach**, **Closing**, or **Closed / AccountRetired:**
  the ledger's enforcement phase is retained, including historical liabilities.
- **Corrupt / LedgerInvalid:** identity, sealing or ledger validation failed.
  Reporting does not repair, reinitialize, or zero the ledger.
- **Unknown / ReconciliationUnavailable:** a live API read or reconciliation
  could not complete. If the account API itself is unavailable, an error cannot
  be persisted: the last stored observation remains, and the operation fails.

Bootstrap, ledger transitions, and periodic recovery populate these observations.
Reporting writes use a fresh UID/resourceVersion and replace the complete status
without changing its ledger. The original `status.ledger.phase` wire contract
remains unchanged. Neither a Phase value nor `Ready=True` is spend authority:
every send still requires live Pod/Task authority, the sealed validated ledger,
an operator contract, and an atomic reservation/begin transition.

Recovery revokes a stale Task authority only while that exact captured authority
is still current inside the account CAS, including after resourceVersion retries.
If concurrent enrollment installed a newer authority, recovery leaves its sessions
and funded work intact and checks its live Task source on the next scan. Old
accepted work remains conservatively charged; deferral never refunds liabilities.

## Operator contracts and unavoidable configuration

Enable the optional Helm `inferenceBudget` section only after supplying:

- A versioned catalog and expiry for each supported exact
  provider authentication identity, endpoint, model, and operation.
- The provider-enforced complete input/context bound, including framing and tool
  schemas; Kars does not estimate this from characters.
- A maximum output field that bounds **all** output, including reasoning/thinking.
- For monetary ceilings, a per-request maximum or maximum input/output
  micro-unit rates per million tokens plus any fixed maximum charge.
- A TLS Secret and public CA for
  `kars-inference-budget.<controller-namespace>.svc:9447`.
- `routerImageDigest`, the operator-qualified SHA-256 manifest digest of a router
  implementing this contract. Finite pods keep the configured router repository
  and `:latest` tag but pin that immutable digest. A floating legacy sidecar that
  ignores the new budget environment must not be mistaken for enforcement.

When upgrading a digest-pinned router, update `inferenceRouter.image.tag`
(`latest@sha256:...`) and `inferenceBudget.routerImageDigest` to the same qualified
manifest digest in one Helm upgrade. The chart rejects conflicting explicit
pins rather than silently running the previous image for finite-budget Missions.
A floating `latest` tag still uses the separately qualified budget digest.

The TLS Secret has type `kubernetes.io/tls`, `tls.crt`/`tls.key`, and annotation
`kars.azure.com/inference-budget-tls: v1`. Choose either installation mode:

- **Existing Secret:** set `tlsSecretName` and leave `tls.certificate` and
  `tls.privateKey` empty. The chart does not create, adopt or rotate that Secret.
- **Core Helm-managed Secret:** set `tlsSecretName`, and supply both
  `tls.certificate` and `tls.privateKey` using protected values or `--set-file`.
  Core installs the supplied PEM identity; no extra product release, raw manifest,
  certificate generator or automatic development trust is required. A conflicting
  live Secret not already owned as budget TLS by this release is rejected.
  **The private key is stored in Helm release history.** Do not use this mode
  if your secret-management policy prohibits that storage; never commit these
  values or expose rendered manifests/logs. Helm deletes its managed Secret when
  the supplied identity is removed, the feature is disabled (with identity fields
  cleared), or the release is uninstalled. Finite inference then fails closed.

Partial supplied identities and public certificates containing private keys are
rejected. PEM framing is checked at render time; the broker validates certificate
and key parsing/pairing, and routers validate server trust and hostname. Operators
must supply an unexpired certificate for the service DNS name and its trusted CA.
The key is never mounted into agents. The CA ConfigMap is public certificates only.

Managed certificate/key changes update the controller Pod checksum and trigger a
rollout. With an externally managed Secret, explicitly roll the controller after
rotation. Both modes require trustworthy CA overlap and updating/recreating finite
router Pods through their supported lifecycle so they trust the new issuer before
removing the old CA. Certificate issuance and accurate, maintained provider
bounds/tariffs remain operator responsibilities. A locally issued Kind development
certificate is validation-only and does not qualify production/AKS trust.

Finite accounts fail closed for missing/expired/unknown bounds, missing prices
when any ancestor needs a currency cap, or unavailable admission/privacy proof.
Adding a monetary cap after unpriced historical or InFlight work is rejected;
that history is not retrospectively priced at zero.

The broker logs rejected requests with the error code, HTTP status and sanitized
contract error or fixed Kubernetes API stage/status. Tokens, request payloads
and Kubernetes response bodies are not logged. Correlate these controller logs
with a router's failed budget stage; a generic HTTP 503 alone does not establish
exhaustion or authorize a retry, budget reset or provider fallback.

The initial closed operation families are text Chat Completions, Anthropic
Messages, and Responses. Contracts reject hosted tools, multimodal generation,
async/background work, stateful prior responses, multiple output candidates,
unknown request options, and incompatible output-limit fields. Embeddings,
legacy completions, image/audio/video generation, Foundry internal generation,
fine-tuning, agent runs, evaluations, memory generation, and generic Foundry
proxy operations are not authorized by these text contracts.
At the proxy's finite-budget gate, unsupported inference operations return HTTP
503 with the OpenAI error code/type `inference_budget_unavailable`. The operation check
runs before unrelated default-provider credential acquisition, so a missing
credential cannot mask this budget denial as a generic 502. Supported operations
still resolve credentials before the final normalized-wire reservation/begin
boundary; this early rejection does not grant authority, contact the broker,
refund work, or change legacy unbounded error handling or preceding policy denials.
Standalone moderation API stages also lack a v1 bounded contract: when a policy
requires one, the entire finite request is rejected before that API call.
The stage is never silently disabled or asserted to have zero cost. Native
provider guardrail annotations remain part of the supported governed send.

Opaque CONNECT, redirected TLS and raw HTTP tunnels are unsupported for finite
accounts: hostname checks cannot prove encrypted HTTP authority or prevent
domain-fronting/coalescing. Native SDKs needing such tunnels must use supported
mediated router operations instead; this does not affect unbounded sandboxes.
Mediated HTTP egress additionally requires an exact operator-declared
`nonInferenceEgressHosts` entry **and** the existing Kars egress policy. This
cannot include configured/catalog model hosts. The declaration is an exclusion
of non-inference service costs, not permission to tunnel model traffic.
Existing content safety and governance checks still apply.

Plain Sandbox spawn and handoff cannot carry authoritative accounting ancestry.
They fail closed in finite mode; use controller-enrolled Task delegation and
retry the same Task UID with a new Pod, preserving its account.

## Authentication and authority prerequisites

Routers receive a short-lived, Pod-bound projected ServiceAccount token with the
exclusive audience `kars.azure.com/governed-inference-budget`. Only the secure
router mounts it. Broker requests use TokenReview and live UID/ownership checks,
not agent headers, task names, or the legacy shared admin token.

The enabled feature installs a shared, runtime-verified admission bundle:
controller-only accounting/status and finite Sandbox authority; namespace
fencing; trusted Deployment/ReplicaSet production; kubelet-only audience minting;
protected public CA projection; and no exec/attach into finite runtime namespaces.
The broker compares exact policies and unrestricted Deny bindings with current
CEL compilation status, not their names or Helm flags.

Issuance also requires the core's real privacy-epoch helper: actual shared Secret
GET/LIST/WATCH denial checks and, when present, current v2 SRE registration proof.
There is no agent-readable fallback credential.

## Standalone CLI and upgrade workflow

`kars budget create -f task.yaml -n <workspace>` explicitly scopes a **new**
Task/Team manifest's existing positive budget to governed inference. It uses
Kubernetes CREATE, never force/adoption, and does not imply launch readiness.
`kars budget status <name> --kind task|team -n <workspace>` reads the pinned
account and reports reserved, settled, uncertain, and unpriced amounts.
CLI inputs/output arithmetic use exact JavaScript-safe integers; larger account
values are rejected rather than rounded.

Existing `kars up`, upgrade, and old/reused Helm values omit the broker by default.
Do not introduce finite enforcement retroactively into a running unbounded UID:
create a reviewed new scoped Task/Team plan. Do not clear pins, reparent funded
tasks, replace accounting objects, or delete data to “fix” a budget denial.
Inspect conditions, account phase, catalog validity, TLS, UID conflicts, and
admission/privacy status instead.

Disabling/uninstalling the optional broker makes finite sends unavailable; it
does not turn finite runtimes into legacy unbounded routers. Workspace or Team
deletion closes authority and conservatively accounts for accepted work.
Persist/backup account objects and root pins together. Restoring missing
accounting requires operator recovery of the original authoritative state,
not a new zero account.

## Explicit v1 capacity

One account has at most 128 retained Task identities, 256 retained Pod sessions,
256 detailed attempts, a 64-attempt replay window per Pod, and a 512-KiB ledger.
An effective authorization snapshot is capped at 64 KiB. These are deliberate
fail-closed lifetime/capacity limits, including for long-running Teams: this
version does not promise unlimited cadence history. Capacity exhaustion requires
operator planning; there is no automatic balance reset or replay-fence eviction.

## Native enforcement fixture

The existing full E2E harness runs `tests/e2e/inference-budget-enforcement.mjs`
against real routers and the private broker in its disposable Kind cluster.
It verifies the built router's manifest and platform content on **every** Kind
node, adds only a missing same-image canonical containerd alias, and checks both
canonical and literal `image:tag@manifest` references through CRI. It never
force-tags, substitutes a config ID for a manifest, or changes production image
pins or pull policies. The fixture registers its named local provider before
launch, and uses an ephemeral `CA:FALSE`, `serverAuth` TLS leaf with the exact
private broker DNS SAN. Certificate files and UID-owned fixture Secrets are
removed during cleanup; diagnostics contain only bounded, fixed stage facts.

Readiness pins the fixture-created Task UID, its account binding, Sandbox UID,
claimed Namespace UID, and Deployment UID. Only nonterminating, Running Pods
owned through that Deployment's current ReplicaSet template, using the exact
verified image and private budget binding, can be forwarded. A legitimate Pod
roll or exited owned tunnel permits at most three reconnections within the
original 120-second deadline. Both `/healthz` and the private `/readyz` contract
must pass, followed by another ownership check. A 503 or transport error alone
does not authorize a new target, and replaced parent identities fail immediately.
This is test-harness lifecycle handling, not a production readiness bypass:
provider-attempt, sibling denial, pending/settled spending, and conservatively
funded cancellation assertions remain unchanged.

Accepted-work cancellation uses the same loopback `kubectl proxy` API client as
the native workload proof. The pinned kubectl v1.30.5 reads `--patch-file -` as a
literal filename, not stdin; cancellation therefore sends a real merge-patch
request instead. Only an HTTP 409 with a matching Kubernetes `Status/Conflict`
permits another attempt (at most three, within 30 seconds). Every attempt rereads
the workspace and Task, retaining the created Task UID, generation, complete spec,
budget binding, workspace UID, and fresh resourceVersion. Changed intent or
identity, 403/422, malformed responses, and transport failures remain fatal.
`BUDGET-CANCELLATION` prints only fixed verb/resource/status/reason facts, not API
messages, headers, or bodies. These client checks do not replace the subsequent
real uncertain-liability and settled/reserved accounting assertions.
