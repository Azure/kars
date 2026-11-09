<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Built-in Hermes SRE

The canonical `sre` Sandbox diagnoses the cluster and creates **pending**
`KarsSREAction` proposals. It never approves or executes its own remedies.
Installation is opt-in; label or namespace occupancy alone is not authority.

## Install and authorize

First install the qualified Core controller/router and follow
[registered SRE authority](../how-to/sre-authority.md). Existing sources require
explicit UID/resourceVersion review, enrollment and migration. Do not bypass that
workflow with an ordinary Helm enable or a direct ServiceAccount grant.

For a genuinely fresh source, a registrar can run:

```sh
kars sre install --context <context> --namespace kars-system --release kars
```

This command upgrades the Core release, not just the SRE workload. Coordinate it
with other release changes. The chart creates `sre-inference`, `sre-tools` and
the Hermes `sre` source in the release namespace. The controller claims
`kars-sre`, then projects the registered private identity after authorization.

The Hermes client uses HTTPS **127.0.0.1:9446**, with the agent-safe token and CA
projected by the controller. The router, not the agent, holds Kubernetes
diagnostic authority. Both initial authorization and later requests can fail
closed when registration, namespace ownership or credential privacy changes.

For local-model qualification, configure the existing
[local inference route and egress](../local-inference.md) first. Changing
`--model` alone does not select a local provider. Never silently substitute a
paid provider for an unavailable local route.

### Select the SRE provider and model

SRE has its own preferred provider/model pair, independent of Team and Mission
choices. Set `sre.provider` and `sre.model` in the Core Helm values, or use
`kars sre install --provider <configured-tag> --model <deployment>`.
The same flags are available on `kars sre authority stage-source` for a genuinely
new source. Install preserves existing release values when a flag is omitted;
an unpinned SRE inherits the configured `controller.inferenceDefaults` pair.
A fresh chart with neither selection configured refuses to invent a provider.

These settings feed the existing `sre-inference` policy, not a direct Hermes
connection. Register a non-default named provider's endpoint, credentials and
egress using [the existing provider configuration](../local-inference.md) first.
A provider tag alone is not endpoint registration. Normal router model recovery
and failover semantics still apply; the preferred pair is not proof of the
provider/model that ultimately served a request.

Selection does not grant authority, renew funding, reset balances, change token
limits or lower safety controls. After first-install adoption, `kars sre install`
automatically retains the original raw `sre.provider` / `sre.model` values in
`sre.modelRouteBaseline`. Helm/GitOps changes must supply that same baseline
(`provider` and `model`, including empty strings for inherited defaults), then
keep it unchanged on later selections. It must reproduce the sealed adoption
receipt and original policy; it is not a replacement receipt. Only the primary
route changes. Fallbacks, request bounds, content safety, identities and funding
remain pinned, and the router still requires a configured provider and applicable
model contract. Prefer this release-owned path for SRE; a later Helm upgrade must
retain the selection.

If a legacy policy has conflicting `helm` / `kubectl-create` ownership, preserve
the failed release and inspect the exact field conflicts. For conflicts confined
to `spec.modelPreference.primary.provider` and `.deployment`, an administrator
can use Bridge **Policies → Edit sre-inference** to change only that pair. Submit
normally first; use the editor's explicit **Force — take ownership** only after
reviewing those two conflicts against the fresh resource version. Preserve the
policy UID, Helm labels/annotations and every other spec field. A stale version
must still be rejected, even with Force. Then publish the identical pair through
the release-owned path with the original `sre.modelRouteBaseline`, without Helm
force. This is a reviewed ownership handoff, not permission to force unrelated
fields or bypass the private-root upgrade checks.

On Kind, an adopted release may select another enrolled provider without changing
its bootstrap route. The workspace provider store must retain its UID-bound Ready
credential grant and the original bootstrap endpoint/credential. The selected
route needs its own named API key or token, an HTTPS endpoint on port 443 and a
matching provider/model/endpoint contract. A legacy `COPILOT_GITHUB_TOKEN` alone
does not authenticate a separately named route.
Existing-source changes retain the normal authority and private-root upgrade
checks. Verify the rendered policy and the actual provider/model in a funded
installed sweep before accepting the change.

### First-install trust rotation recovery

An adopted first-install SRE intent retains its original immutable CA, funding,
source and policy pins. After an operator-reviewed private-root retirement and
budget TLS rotation, the controller can accept a different **single CA anchor**
from the operator catalogue only when the accounting namespace has a current
Qualified private epoch. It rechecks the enforcing admission bundle, authenticated
root identity/template, current private consumers and legacy Secret denial. The
TLS Secret UID/resourceVersion and SHA-256 SPKI key must match the namespace's
reviewed and qualified pins; the replacement CA must verify that certificate's
chain, validity and `kars-inference-budget.<accounting-namespace>.svc` name, with
a matching private key. Adoption alone never authorizes a different CA; appended
unrelated trust anchors, stale qualification and substituted TLS material fail
closed. The original exact-CA path does not require private activation.

Recovery deliberately uses the independent private-root proof, not the SRE
registration's own Ready status: SRE readiness depends on first-install validation.
An `AuthorityBlocked` SRE can also block `bridge-orchestrator` at
`GovernedServicePrivacyNotReady`; that is the existing shared privacy fence, not
permission to bypass it or add private annotations to unrelated namespaces.
Ordinary `Provisioning`/`Migrating` privacy qualification instead leaves dependent
sandboxes `Pending` (`GovernedServicePrivacyPending`, `Ready=False`,
`Progressing=True`). Reviewed task phases remain `Launching` and retry after
qualification; no control credential is issued or reused while waiting. Failed
privacy checks still degrade the sandbox and quarantine owned credentials.
After installing a qualified repair through the normal reviewed root migration,
verify SRE registration readiness, controller-managed credential reprojection and
orchestrator recovery. Never recreate revoked credentials manually, restore old
keys/CAs, rewrite the sealed intent, reset accounts or replay completed migrations.
Expired initial funding still needs its exact durable operator-authorized renewal;
trust recovery does not grant tokens or change ToolPolicy/inference safety checks.

## Proactive health sweeps

When `SRE_ENABLED` is enabled, the maintained Hermes entrypoint starts
`kars_runtime_hermes.plugin.sre_watcher`, even without channel credentials.
The watcher:

1. Acquires a process-lifetime file lock so duplicate supervisors do not overlap.
2. Checks the registered SRE HTTPS `/readyz`, the inference router's `/readyz`
   (including its existing finite-contract/calendar readiness), installed Kars
   plugin and loopback model configuration.
3. Reads existing proposals and invokes the installed Hermes oneshot command
   with the **`sre` toolset only**, the existing Kars governance hooks and the
   loopback inference provider. It asks the real agent to run `sre_diagnose`,
   investigate current failures, and propose only supported, evidence-backed
   remedies. Initial failures are not silently absorbed.
4. Records completion, failure, timeout or blocked authority, then waits before
   the next sweep. A timed-out process group is killed and reaped before another
   turn can start. SIGTERM also cleans up the active turn.

Every inference request still traverses router budget reservation and accounting.
There is no watcher-owned budget, grant, credential fallback or retry route.
Missing authority/contracts block the sweep rather than starting an ungoverned
turn. Existing interactive human sessions are not locked by the supervisor.
Blocked preflight checks retry after 30 seconds without invoking inference;
completed, failed or timed-out Hermes turns use the full configured cadence.

Environment settings on the existing Hermes `extraEnv` surface:

| Setting | Default | Meaning |
|---|---|---|
| `SRE_WATCHER_ENABLED` | `true` | Set `false`, `0`, `no` or `off` to disable |
| `SRE_HEALTH_INTERVAL_SECONDS` | `900` | Completion-to-next-start delay, 60–86400 seconds |
| `SRE_HEALTH_TIMEOUT_SECONDS` | `180` | Per-turn deadline, 10–900 seconds |
| `SRE_WATCHER_NOTIFY_TARGET` | unset | Optional explicit `telegram:recipient`, `slack:recipient` or `discord:recipient` |

The old deterministic event/phase watcher is replaced, not run alongside this
supervisor. Legacy `SRE_WATCHER_MODE`, 10-second event polling, dedupe and message
rate-limit settings no longer select a second behavior. This includes the old
chart's `phase-changes-only` value; it does not prevent real health sweeps.

### Reports and optional notifications

`$HERMES_HOME/sre-latest-report.json` is atomically replaced after each sweep.
`sre-latest-turn.log` retains at most **256 KiB** of combined stdout/stderr.
Overflow kills and reaps the full child process group and records a failed
turn, never a healthy result. Capture is bounded while the process runs, not
just when reading the completed log. Successful-turn JSON contains at most
64 KiB and marks truncation. A blocked sweep does not make that
older process log current. `finished` means the process exited successfully,
**not** that diagnostics or a live acceptance gate passed.

These local files have the lifetime of the existing Hermes home volume; they
are not durable across an `emptyDir` replacement. The watcher also publishes each
finished, failed, timed-out or blocked sweep through **POST `/reports/latest`** on
the existing authenticated private SRE HTTPS facade (9446). Reporting never calls
inference or bypasses its readiness/budget gate: a budget-blocked sweep can be
published while registered SRE authority remains live. Publication failure retains
local evidence and records `publication_error`, not a fabricated persistence receipt.
Timeout and output-overflow reports retain captured partial diagnostic output.

The controller provisions exactly **`kars-sre/sre-health-report`**, a ConfigMap
owned by the canonical registration, with one `report.json` value. The router
receives only named `get/update` permission through its existing private namespaced
Role; no report create/delete, generic resource write, remediation approval or
inference capacity is granted. A dedicated fail-closed admission policy reserves
creation/deletion and metadata/source changes for registrar authority, except that
`system:serviceaccount:kube-system:namespace-controller` may DELETE the retained
report only when the actual namespace has a deletion timestamp and phase
`Terminating`; this grants no CREATE/UPDATE rights. Even Bridge's general ConfigMap
mutation grant cannot edit this target. The publisher
is bound to live registration, sandbox and runtime namespace UIDs, not caller
supplied identities. The existing authority checks run before storage operations
and again before acknowledgment. UID/resourceVersion compare-and-swap prevents
an observed target replacement or concurrent update from being overwritten.

Only the **latest** report is retained, across agent pod replacement. Text is an
at-most **8192-byte UTF-8** advisory excerpt; request/envelope limits are **65536
bytes**. Existing Hermes and AGT credential redaction removes recognized patterns
before storage; terminal controls are stripped. Pattern-based redaction cannot
guarantee detection of every secret. Do not intentionally include credentials in
diagnostic output. Long excerpts keep failure context and the final diagnosis;
truncation is explicit. A 32-hex sweep ID and digest bind the typed sanitized
content. Within the five-minute publication window, an identical latest replay
returns the original receipt without rewriting; conflicting IDs or older/equal
completion times are rejected. One transport/5xx retry uses the same payload.
No cross-sweep history or tool-result correlation is invented.
Failed turns containing an exact router calendar-budget denial are reported as
local budget failures, not provider outages or instructions to switch providers.
Review the UID-bound calendar separately from lifetime funding; an authorized
finite daily-limit increase for an adopted SRE installation is supported by the
[calendar budget workflow](../governed-inference-budgets.md#router-calendar-counters).
Publication preserves the failed turn status and original local output; it does
not grant capacity, reset accounting, or convert failed diagnostics into health.

Authenticated **operator/admin** readers see the latest report in Bridge's
**SRE Actions → Latest automatic health report** through
`GET /api/operator/sre-report`. Plain-text output cannot execute UI actions.
`finished` is **health unverified**; blocked/failed/timeout, unknown coverage,
truncation and unavailable reads remain explicit. Reports become stale after
completion + configured interval + timeout + 60 seconds, or a privacy epoch
change. Retired/disabled authority, replaced sources and invalid records hide the
report text. Source replacement under the same registration clears the old record
atomically; ordinary pod replacement preserves it. A different registration UID
cannot adopt the former target: registrar-authorized cleanup is required. Do not
rely on garbage collection to bypass that admission boundary. Revocation removes
the existing private grants but does not turn retained evidence into current
health. `KarsSREAction` remains the separate, unchanged remediation review surface.

No channel is required. Telegram can infer a recipient only when its bot token
and `TELEGRAM_ALLOW_FROM` are both configured. Slack/Discord require their
matching token and an explicit notification target; they never default to
Telegram. Notification failures are recorded separately and do not discard the
health report. The watcher sends through the existing `hermes send` path.

## Diagnosis and proposals

`sre_diagnose` reports controller readiness, CRD presence, resource phases,
workload availability and reconcile freshness. Failed/malformed inventories,
failed workload reads, missing phases and absent/invalid freshness timestamps
produce explicit `coverage_errors`. A zero degradation count with incomplete
coverage **does not mean healthy**.

The tools also include `sre_describe_state`, `sre_logs`, `sre_explain_error`,
`sre_describe_resource`, `sre_what_changed`, `sre_endpoints_inspect`,
`sre_image_probe` and `sre_top`. Error hypotheses must be confirmed against
resource evidence. Metrics availability is optional.

`sre_describe_resource` supports namespaced `KarsEval` detail reads through the
same authenticated, read-only diagnostic proxy. It returns the existing CR
conditions/generation summary, target sandbox, schedule/drift setting, compact
last-result counts and Job/report references, plus recent events. It does not
return arbitrary spec/status fields, annotations, corpus contents or report
payloads. Missing results remain unknown, not zero-failure passes; errored cases
remain distinct from passed/failed cases. Referenced evidence is not independently
verified by this tool. Missing namespaces, unsupported kinds and API denials remain
explicit errors; this support does not expand RBAC or authorize eval execution.

Namespaced `EgressApproval` detail reads use the same read-only path. They return
sandbox, host/port entries, TTL, reported phase/conditions/generation, effective
and expiry timestamps, merged digest, host count and usage count. Missing status
or counts remain unknown; a reported `Active` phase is not independent proof of
current router enforcement or unexpired access. Arbitrary fields, annotations,
reason and ticket text are omitted. Reading an approval neither grants egress nor
renews its TTL, and does not authorize a remediation action.

`sre_what_changed` reads all event pages from both Kubernetes event APIs and
filters their last observed occurrence into the reported `since`–`as_of` UTC
window (15 minutes by default, constrained to 1–60 minutes). Repeated-event
series and legacy timestamps are supported; `lastObservedTime` is the normalized
occurrence used for filtering. `excluded_undated` counts events whose occurrence
cannot be established; those are not evidence of either recent failure or a
healthy interval. Read/pagination failures remain explicit errors. Kubernetes
retention and aggregation still limit this evidence, and an aggregator readiness
event is not a datapath capture timestamp or proof of witness freshness.

`sre_propose_fix` supports `DeleteResourceQuota`, `DeletePod`,
`ScaleDeployment`, `PatchDeploymentImage` and `RolloutRestart`. It creates a
Pending proposal and returns the actual action name only after a successful
CREATE response, or reuses an identical Pending action (`cr_reused=true`,
`cr_created=false`) found in the live inventory without rewriting it. Unknown
proposal inventory blocks creation rather than risking repeated proposals.
`cr_error` means persistence was not confirmed. Unsupported
remedies require operator review, never a fabricated action ID.

```sh
kars sre actions --context <context>
kars sre approve <action-id> --context <context> --note "Reviewed evidence"
kars sre reject <action-id> --context <context> --reason "Not supported by evidence"
```

Approval is a separate human operation requiring the appropriate RBAC. The
controller subsequently validates and executes supported approved actions.
The executor creates a separate ServiceAccount, Role and RoleBinding for each
attempt in the target namespace. The Role permits only `get` and the required
mutation verb on the exact `resourceNames` target (including `deployments/scale`
for scaling). The writer client uses only the API-issued in-memory bearer token;
controller credentials and impersonation are removed. The stable `sre-writer`
ServiceAccount stays permissionless and the private SRE router identity is unchanged.

The TokenRequest requests **600 seconds**, Kubernetes' minimum, not the previously
documented 300 seconds. It is intrinsically bound to the **per-action SA UID**, not
the unrelated SRE Pod. Kubernetes tokens are not inherently single-use: the
controller consumes the action attempt once, dry-runs and executes with target
UID/resourceVersion fences, then deletes the binding, SA and Role. Expiry outside
the requested bound (allowing 30 seconds of clock skew) fails closed.

`status.execution` durably records the attempt and cleanup obligation without
credentials. A finalizer remains until all authority is confirmed absent. Cleanup
errors appear as `AuthorityCleaned=False` and retry, including after restart or
deletion. An active attempt has a 90-second local timeout; other replicas wait
until its persisted 120-second deadline before interruption cleanup. An unknown
outcome is **not retried**: inspect the target and submit a fresh reviewed proposal
if needed. These are authority lifetime bounds, not beta inference budget limits.
Legacy `writerCrbName` references are cleared only when the binding is absent;
existing legacy bindings lack full-UID ownership proof and require operator
verification/revocation rather than automatic deletion of potentially foreign RBAC.
Source implementation and local regression tests are not live Kind acceptance.

## Live Kind acceptance

Package the coordinated Hermes changes once; do not independently deploy an
SRE-only alternative. With qualified Core/router authority and an explicitly
configured real local model, observe an **unprompted** sweep discovering actual
Kind state. Verify real Hermes tool calls, evidence-backed findings, explicit
unknown coverage, and any returned action ID against the actual Pending
KarsSREAction. Verify that no action was automatically approved or executed.

Reporting requires a coordinated controller/admission/router/Hermes/Bridge rollout;
source and local tests alone do not qualify the installed beta. Respect the
first-install image seal and existing finite inference contracts: do not reset,
upgrade around the seal, replenish budgets or change funding to demonstrate reports.
Verify an unprompted persisted report in the authenticated operator UI, matching
its source UID, sweep ID and completion/persistence times. Observe a budget-blocked
report without a model call, stale/incomplete coverage, rejection of nonoperators,
and persistence across an approved pod replacement. Verify source retirement and
replacement hide obsolete output. Until these installed checks pass, automatic
operator-visible reporting remains unqualified.

Unit tests, a ready pod, process exit zero, historical fixtures, a manual chat
or a deterministic watcher alert are not autonomous-agent acceptance.

For an interactive diagnostic smoke check use `kars connect sre --web --context
<context>`. Once connected, transient port-forward resets reconnect on the same
loopback port and Kubernetes context without reopening the browser. During a
rollout, recovery waits up to three minutes for the replacement agent container
to become Ready instead of forwarding to a Pending pod. A replacement dashboard
may start listening after pod readiness; connection refusal and renewed Pending
races retry within the same deadline. Ctrl+C also cancels reconnect backoff and
readiness polling. Initial startup, authorization, and port-collision failures
remain explicit errors; reconnection is not evidence of workload health.
Retire the reviewed registration before `kars sre uninstall`; see the authority
guide for exact sequencing.
