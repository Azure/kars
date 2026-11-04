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
unbounded. Accounts with any effective finite token ceiling require trustworthy
token bounds, not an invented price. No Copilot, local model, or provider is
assigned a guessed zero price.

When current authenticated funding and **every effective ancestor** have neither
a token nor a monetary ceiling, model spending contracts are not required.
The broker authorizes record-only attempts at both reserve and begin; router
identity, provider credentials, request-shape checks, egress controls and replay
fences remain enforced. Provider-reported input/output usage is settled exactly
once. Missing usage retains an uncertain attempt and increments
`unknownUsageAttempts`; a zero token reservation is not known-zero consumption.
Late usage resolves that counter without losing history, including after session
closure. Unresolved record-only attempts prevent retroactively imposing finite
ceilings. Unpriced usage remains explicitly unpriced.

New models may use provider-reported context/output capacity for runtime setup.
Azure's model-list API omits both capacities for `gpt-6.1-sol`; when an Azure
endpoint reports exactly one successful, inference-enabled entry with that exact
ID, the router uses Microsoft's [model catalogue](https://ai.azure.com/catalog/models/gpt-6.1-sol)
limits: 1,050,000 context tokens and 128,000 output tokens (verified 2026-11-04).
Live valid capacities take precedence. Missing models, duplicates, partial or
invalid capacities, other models/aliases, and non-Azure providers do not receive
this fallback. Both Hermes and OpenClaw consume this shared router metadata.
Capacity metadata is not a spending guarantee or a price contract. Any effective
finite ceiling still requires the existing exact provider/endpoint/model/operation
contract, with maximum prices where money is capped.

Legacy tasks without the new scope retain the foundation's planning-only
interpretation of positive aggregate budgets. Aggregate limits are never copied
into a per-sandbox daily allowance.

### Explicit unbounded token allowance

Kind beta uses an operator-authorized, **unbounded and non-expiring** token
allowance. This is not a large finite grant or automatic renewal. Actual token
usage, settled and uncertain liabilities, and replay fences remain recorded.
Currency limits, provider/model bounds, identity, isolation and revocation checks
still apply. Accounts without an allowance retain finite enforcement.

For an existing account, `kars budget status` exposes its canonical `ledgerDigest`.
Prepare a receipt with `id`, `configUid`, `accountUid`, the exact `root`,
`originalLimits`, that `ledgerDigest`, and sorted `previousTokenTopUpIds`.
There is deliberately no token amount or expiry field. Publish it with:

```sh
kars budget authorize-unbounded -f allowance.json -n kars-system --catalog kars-inference-budget-contracts
```

The command requires native `karsbudgetaccounts/fund` and ConfigMap patch
permission. The controller consumes `unbounded-token-allowances.json` through
its existing identity-checked account CAS, not a workload-provided flag. Original
funding and all top-up receipts must remain present and unchanged. Preserve the
receipt in `inferenceBudget.unboundedTokenAllowances` on Helm upgrades, alongside
original funding and top-ups. The SRE adoption comparison restores the originally
sealed allowance list (including absence on older installs), without changing the
selected list emitted to the catalog. Only an empty new default is accepted during
initial adoption; subsequent allowance changes retain the separate native `fund`
authorization. Deleting a receipt revokes authorization; it never
erases spending history. New finite top-ups cannot be added to an unbounded account.
Cold-install receipt carry-forward validates the original bindings and preserves
existing JSON bytes when the resulting allowance list is unchanged. This avoids
server-side apply conflicts caused only by operator/Helm serialization differences;
changed allowances still require the same authorization.

All nodes in the authorized account receive the token override, including future
children; currency caps remain as authored. The same allowance overrides daily
and monthly Kars token ceilings in Chat Completions, Responses, and Anthropic
Messages, for both runtimes. An over-limit calendar check reads the authenticated
broker catalog and verifies the live operator receipt before permitting this
override; an older broker without the field retains finite enforcement. Missing,
corrupt or uncertain calendar accounting still blocks inference under the default
calendar policy; the explicit Kind-beta record-only policy below retains rather
than clears unresolved usage. Calendar
Begin/Finish, lifetime reservations, settlement and final-send authority checks
remain enabled; neither counters nor historical uncertainty are reset. A safely expired, closed Bridge
orchestrator can reactivate without reopening any closed Pod session. Stopped or
frozen workflows cannot be reopened by this allowance. Fresh accounts require
their own explicit authorization; the setting does not globally waive policy.

### Explicit calendar record-only policy

Production defaults to `inferenceBudget.calendarPolicy: enforce`. Core's
`values-kind-beta.yaml` explicitly selects `record-only`: authenticated calendar
Begin/Finish writes continue, known tokens accumulate, and historical pending
receipts remain intact, but calendar daily/monthly ceilings and unresolved usage
do not stop new beta requests. Both Hermes and OpenClaw use this shared path.
This policy belongs to Core; adding or removing Bridge does not change it.

The protected operator catalog carries `calendar-policy`. Changing it requires
`karsbudgetaccounts/fund` permission, with the admission policy verified before
record-only authorization. It cannot be selected by an agent environment flag.
Reverting to `enforce` restores strict admission on subsequent requests without
resetting history. Missing policy means enforcement; invalid policy fails closed.

Missing/corrupt accounting, identity or authentication failures, unacknowledged
commits, sequence errors and persistence failures still block. Lifetime funding,
revocation, currency limits, provider limits and sandbox isolation are unchanged;
record-only is not a replacement for lifetime unbounded-token authorization.
The task budget endpoint reports `calendar_policy`, `calendar_ceilings_enforced`
and `inflight_or_unresolved_requests`. Pending requests can be active or have
unknown usage; known token counters are not evidence of a complete usage total.
Older brokers without this metadata remain strict and report pending as unknown.

### Bound Foundry memory service

Foundry Memory Store can invoke its own chat and embedding models without
returning a trustworthy token bound or full usage. It is not one of the three
closed inference operations and is never settled as a fabricated chat request.
Under governed budgets, the router permits this external service only with a
live `KarsMemory` binding, current authenticated unbounded-token authority, and
no applicable monetary ceiling. Finite or money-capped accounts remain denied
until a bounded memory metering contract is available. Missing, revoked or
unavailable broker authority fails closed; no workload flag enables an exemption.
Other generic Foundry routes remain denied.

Exact store/scope authorization, update ownership, path validation, governance,
threat checks and Foundry authentication still apply. Every allowed dispatch
requires a hash-chained audit intent. Outcome records distinguish upstream
failure, received response and unknown outcome; asynchronous acceptance is not
persistence. Numeric provider-reported usage is retained separately as unsettled
external-service usage. Absent usage is `unreported`, never zero. Internal memory
cost is not included in inference totals, and the service is not asserted free.
Request contents are not copied into this audit. Existing model inference
reservations, calendar counters, settlement and historical liabilities are unchanged.

### Composing a usable finite budget

The composer receives current model reservation bounds and must explain estimated
model-call counts and cumulative token allowances for research and synthesis.
For sequential work, a role needs enough capacity for tokens settled **before**
the last planned call plus that call's complete reservation. Releasing unused
reservation capacity does not refund settled tokens. The shared Mission ceiling
must also cover its roles and synthesis; a larger root does not enlarge a role.

Native research can repeatedly send accumulated source/tool context and make
compaction calls. Neither final-answer length nor tool-call count alone estimates
that cost. Use conservative context-growth estimates and available actual usage,
while retaining finite, explicitly reviewed limits and any user ceiling. These
estimates are not completion guarantees; admission checks still enforce live
capacity. A consumed run's budget is not silently increased to retry it.

A model-composed Mission must explicitly select a current provider/deployment
pair. Missing, malformed or unavailable selections enter the existing bounded
composer repair, rather than silently accepting the cluster default with another
model's rationale and budget. Manual package review remains available.

Team package preflight includes the reviewed member roster, with role names bound
exactly to the execution plan. Each member's token cap is checked against its own
provider/model override and applicable fallbacks, not the principal's model. An
unset member route inherits the same defaults as Team creation; Mission workers
continue to inherit their principal route. Missing, duplicate or mismatched Team
roles fail preflight. These checks neither enlarge budgets nor replace router
admission against the live provider, endpoint, operation and account.

### Contended authority snapshots

Task budget preparation retries a changing UID/resource-version inventory using
three complete read-only lineage checks. If that inventory or the account CAS
remains contended, reconciliation requeues without publishing degraded governance
or changing the existing runtime. Contention is not evidence of revocation and
must not stop an accepted, non-replayable assignment.

This grants no new authority: a new Task remains unready, changed generation or
authorization still fails readiness, and every broker request continues to verify
live identity, lineage and accounting. Missing, corrupt or invalid authority keeps
its existing fail-closed handling; settled and uncertain charges are unchanged.

### Renewing reviewed model contracts

Funding renewal does not renew model bounds. An expired model contract continues
to block readiness and inference even when the account has valid funding.
After original SRE Helm adoption, operators may renew `inferenceBudget.contracts`
through an ordinary Core upgrade: retain each contract's identity, ordering,
provider, endpoint, model, operation, token/wire bounds, reasoning coverage and
maximum prices; change only its `version` and `validUntil`, with a new
`inferenceBudget.catalogVersion`. Each changed contract needs a strictly later,
unexpired UTC deadline. Reconfirm the reviewed bounds before extending validity.
After adoption, the same ordinary Core upgrade can append reviewed contracts for
additional provider/model routes with a new catalog version. Preserve every
original contract in order, except supported validity/version renewals. Additions
need unique identities and provider/model/operation routes, explicit endpoints,
current provider-supported input/output bounds and unexpired validity. They cannot
replace or extend an original provider/model route. Removing contracts or changing
sealed fields requires explicit reviewed migration, not this additive path.

These per-request model bounds are not workspace spending allowances. Adding a
route does not create funding, change the default provider, reset recorded usage
or impose an aggregate cap on an unbounded workspace.

The chart and controller compare the renewal against the immutable original
intent and require completed adoption without rewriting either receipt. The
renewed catalog is emitted normally;
past reservation/quote snapshots and liabilities remain unchanged. This creates no
funding, waives no provider qualification and disables no authentication. Retain
all separately authorized `tokenTopUps` in upgrade values, including renewals.

### Updating operator-classified non-inference egress

After original SRE adoption, an operator can update
`inferenceBudget.nonInferenceEgressHosts` through the ordinary Core Helm upgrade.
Use at most 64 exact lowercase DNS names. Wildcards, URLs, ports and catalog model
hosts are rejected. The router also refuses configured model hosts. Reviewed,
controller-published sandbox egress grants now authorize ordinary non-model HTTP
access without duplicating each approved domain in this global catalog. The
router checks the live model catalog first; neither signed egress nor learning
can authorize a configured/catalog model endpoint outside its brokered route.
Learning alone cannot authorize an unclassified, unreviewed destination.
Approving ordinary egress does not meter external service charges as inference
or declare those services free.

Install the compatible controller with this chart transition. The chart and
controller retain the immutable original intent/adoption receipts, model bounds,
funding and charges. Classification-only changes do not renew model contracts or
funding; a simultaneous model renewal must satisfy the separate rules above.
Removing a catalog classification closes its learning-mode exclusion; revoking
the sandbox's signed egress grant closes reviewed access. Neither action resets
past attempts, restarts failed Missions or authorizes additional calls. Verify
the installed catalog, approved policy and real governed fetch before claiming
execution acceptance.

### Operator-authorized lifetime token top-ups

Existing governed **Teams and the built-in `sre` and `bridge-orchestrator` sandboxes**
can receive explicit additional lifetime tokens without replacing the root,
re-enrolling the sandbox, changing its original grant, or resetting any usage. This is separate from Team Edit,
`kars sre install`, daily/monthly allowances and ordinary workload delegation.
No exhaustion event creates funding automatically.

The private account's immutable `spec.limits` and original `ledger.limits` remain
unchanged. An append-only `ledger.tokenTopUps` receipt map records each extension;
`status.effectiveLimits` and `kars budget status` expose the recorded additive
ceiling. The CLI also reports `originalLimits`, `addedTokens` and `tokenTopUps`.
The Team page/API charter budget remains the **original grant**, not the effective
ceiling. A recorded ceiling is neither remaining capacity nor live authorization.
Reserved, settled, uncertain, unpriced-attempt and replay-fence records are retained.
Currency ceilings/prices, calendar quotas, isolation and authentication are unchanged.

An extension has a positive finite token increment and a unique lifetime ID. It
pins the operator ConfigMap UID, account UID, complete original root identity
(including cluster/workspace UIDs), original normalized limits, and explicit
**active top-level node UID/authorization-digest targets**. Those nodes must have
an original token cap equal to the root cap. Only these unattenuated top-level
nodes gain the increment; narrower principal/role caps and all descendant caps
remain unchanged, even if a descendant's numeric cap equals the root cap.
The Team root ceiling receives the increment once, not once per target. Changed
node authorizations do not inherit another digest's extension. There is no
arbitrary Task or sandbox funding path; sandbox top-ups are restricted to the
built-in SRE and Bridge orchestrator roots.

#### Operator use

Use an operator credential with account `get` and the explicit custom **`fund`**
verb on `kars.azure.com/karsbudgetaccounts` in the accounting namespace, plus
`get`/`patch` on the named operator contracts ConfigMap. A delegated Role rule is
`apiGroups: [kars.azure.com], resources: [karsbudgetaccounts], verbs: [get, fund]`;
bind it only to trusted funding operators, never agents, Team members, routers or
runtime service accounts. The admission policy independently checks `fund` for
funding-key creation/change/deletion. Generic ConfigMap write access is not
funding authority. Cluster administrators already have wildcard authorization.

1. Read the existing Team's `status.inferenceBudgetAccount` or the built-in SRE or
   Bridge orchestrator sandbox's `spec.inferenceBudgetRef.account`, then the referenced sealed account and the
   operator contracts ConfigMap. Confirm root/account/workspace identity and the
   existing root-node authorization digests; do not fabricate or re-create them.
2. Save a JSON authorization file using those exact values. For example (replace
   the illustrative identity strings/digest and choose an explicit finite amount
   and future expiry):

   ```json
   {
     "id": "operator-extension-1",
     "configUid": "existing-configmap-uid",
     "accountUid": "existing-account-uid",
     "root": {
       "kind": "KarsTeam",
       "resource": {"namespace": "workspace", "name": "existing-team", "uid": "existing-team-uid"},
       "workspaceUid": "existing-workspace-uid",
       "clusterUid": "existing-kube-system-uid"
     },
     "originalLimits": {"tokens": 100000},
     "tokens": 500000,
     "expiresAt": "2030-01-01T00:00:00Z",
     "nodes": {"existing-principal-task-uid": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}
   }
   ```

   Copy `originalLimits` verbatim from the normalized ledger, including any
   configured `usdMicros`. For either built-in use its exact `KarsSandbox` root and
   root-node UID/digest instead. The original built-in grant or its explicit renewal must
   still be live: an ordinary top-up does **not** renew either built-in's expiry or
   change its credential binding. The separate renewal operation below supports
   SRE only; an orchestrator top-up cannot extend its original funding expiry.
3. Publish and inspect (accounting namespace and workspace can differ):

   ```bash
   kars budget authorize-top-up -f token-top-up.json -n kars-system --catalog kars-inference-budget-contracts
   kars budget status existing-team --kind team -n workspace
   kars budget status sre --kind sandbox -n kars-system
   kars budget status bridge-orchestrator --kind sandbox -n kars-system
   ```

   Publication uses ConfigMap UID/resourceVersion JSON-patch tests and preserves
   unrelated keys/authorizations. `published: true` is **not** `applied: true`.
   The controller recovery loop or authenticated broker reserve/begin applies
   the entry by account CAS. Confirm the exact durable receipt and effective
   limits before claiming success; the CLI never writes account status. Retry
   the identical authorization ID/content after a lost acknowledgement, not a
   fresh ID. IDs cannot be repurposed; at most 64 receipts are retained per root.
4. Retain **all** published entries in the release's
   `inferenceBudget.tokenTopUps` Helm values; these render `token-top-ups.json`
   in the same protected operator ConfigMap. Otherwise a later Helm upgrade can
   revoke an entry published through the CLI. No chart value enables automatic
   funding, and nonempty funding configuration requires the budget broker.

   For an already adopted SRE release, this one field is compared against its
   sealed initial value only when verifying the original adoption/first-install
   intent. This accommodates older receipts that predate the empty-list default
   and later explicit grants without rewriting either receipt or the original
   SRE grant/expiry. The actual list is still rendered into the protected
   ConfigMap: changing it requires the separate `fund` permission, not merely
   permission to upgrade Helm. All other adoption comparisons remain enforced.
   Complete initial SRE adoption before adding grants. Install the updated chart
   with an empty list and verify the top-up admission policy/binding first; only
   then publish grants and retain them in Helm values. A Helm render or upgrade
   dry-run is not proof of server-side admission authorization or applied funding.

#### Explicit first-install SRE renewal

`kars budget authorize-renewal` is a distinct, operator-only action for an
**existing Active, sealed first-install SRE account**. It requires the same
separate `fund` permission, plus read access to the canonical SRE registration,
source Sandbox and immutable first-install intent. It cannot bootstrap an
account, renew Team/legacy funding, undo revocation, or reopen inactive authority.

Prepare the same top-up fields with a **new receipt ID**, positive finite
additional `tokens`, and a future `expiresAt` strictly later than the original
and all prior renewal expiries. Add this exact `fundingRenewal` object:

```json
{
  "registrationUid": "actual-canonical-registration-uid",
  "intentUid": "actual-immutable-intent-uid",
  "intentDigest": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "originalExpiresAt": "2026-10-16T11:16:36Z",
  "previousTopUpIds": ["existing-applied-top-up-id"]
}
```

Copy every pin from the actual resources; the example digest is illustrative.
`previousTopUpIds` must be the sorted, unique, complete set of currently applied
receipt IDs, including earlier renewals (an empty array only when none exist).
The controller verifies that exact snapshot on CAS, along with original source,
registration, intent, policies, namespaces and account bindings. A raced new
receipt invalidates the proposed snapshot rather than silently widening it.

```bash
kars budget authorize-renewal -f funding-renewal.json -n kars-system --catalog kars-inference-budget-contracts
kars budget status sre --kind sandbox -n kars-system
```

This appends a normal immutable top-up receipt with renewal metadata through the
existing protected `token-top-ups.json` field. No new funding store is created.
It explicitly reauthorizes the original grant and named expired receipts until
the new expiry, adding only the declared tokens. Original grant bytes/digests,
account/node limits, settled and uncertain liabilities, sessions and replay
fences remain unchanged. All covered historical entries must remain identically
configured; removal or modification still blocks new work. An expired unapplied
grant cannot be retroactively covered. Latest renewal expiry is a hard stop.

Install renewal-compatible **controller, router and Bridge BFF readers before
publishing**: older strict readers reject the new nested `fundingRenewal` field.
The existing CRD ledger envelope preserves this metadata; no schema bypass or
status patch is needed. Keep all receipts in Helm values. Status exposes original
and effective expiry and renewal receipts, but is not live authority. Verify the
CAS-persisted receipt, unchanged prior liabilities, recovered privacy authority,
and a real governed operation before counting restored service.

**Compatible router upgrade for an adopted SRE release:** receipt-aware readers
must be installed before funding. Explicitly set `sre.routerImageBaseline` to the
**original** `repository:latest@sha256:<digest>` reference. Obtain the digest from
`budget.routerImageDigest` inside the immutable first-install ConfigMap's
`intent.json`, reached through the canonical SRE registration's
`spec.firstInstallFunding.intent` name/UID/digest binding. Retain the original
router repository from the release values. The chart passes its original digest
as `KARS_INFERENCE_BUDGET_FIRST_INSTALL_ROUTER_DIGEST` to the controller, which
requires an exact match with the immutable, UID/digest-bound first-install intent.
This baseline is used only to validate that original intent; live router identity
checks still require the selected `KARS_INFERENCE_BUDGET_ROUTER_DIGEST`. Without
an explicit baseline, the original and selected digests must still match. The
original reference is evidence, not an exemption: replacing only the selected tag and
`inferenceBudget.routerImageDigest` with those originals must reproduce the
existing immutable adoption digest. A forged repository/original, an unadopted
release, mismatched selected digest or any non-image drift is rejected.

Adoption-baseline compatibility is **not** private-root migration authorization.
After private qualification, controller environment changes (including the first
baseline declaration and selected router image/digest refresh) require the
maintained reviewed private-root retire/rotation/requalification migration;
**never use a direct Helm upgrade** to apply them. The image-delta guard permits
introducing the baseline only from the original qualified deployment's unique,
literal governed router pin, matched to its immutable router image. With no
explicit baseline that pin is already the first-install verifier, so copying it
preserves rather than expands authority. Missing, indirect, duplicate, malformed
or mismatched pins, newly enabled governance, unrelated environment changes and
baseline replacement/removal are rejected. Later migrations retain the baseline
unchanged, even when the selected router has advanced. The chart/controller's
immutable first-install intent and adoption checks remain required.

Set `inferenceRouter.image.tag` to the qualified new `latest@sha256:<digest>` and
`inferenceBudget.routerImageDigest` to that same digest; keep the repository,
pull policy, every other router setting and original `sre.runtimeImageBaseline`
unchanged. A completed private-root image migration may leave Helm's saved images
behind the installed controller. Core publication can carry forward a different
router repository **only** when its selected immutable image, verifier digest and
original-baseline digest exactly match unique literal declarations on that same
live, original-UID controller. Missing, duplicate, indirect or mismatched
declarations fail closed. This cannot select a different image from the installed
one; further repository migrations still require the reviewed private-root path.
The original repository and digest must still reproduce the sealed adoption hash.
Retain `sre.routerImageBaseline` with the same **original** reference on
subsequent upgrades (do not advance it to the last deployed image). This opt-in
has no default and does not replace/reseal receipts, re-enroll SRE, renew its
source grant/expiry or change workload credentials. It composes with the existing
controller/runtime/pull-coverage/Kind-dispatcher transitions and the separately
privileged funding list. A chart/API dry-run validates the transition, not binary
compatibility: qualify the new readers separately, preserve an empty funding list
through mixed-reader rollout, and verify both sandbox router replacements before
publishing any grant. Existing per-request/child ceilings still apply.

**Dispatcher image refresh for an adopted SRE release:** when the dispatcher was
already enabled in the sealed adoption values, supply `missionDispatcher.imageBaseline`
with the **original** `image` object: `repository`, immutable `digest`, and `tag`
only if the original object contained it. The optional tag must be
`latest@sha256:<digest>` and agree with `digest`. Select a qualified new digest
under the same explicit registry (a different repository path is supported),
updating the tag too when present. The Deployment renders the selected image as
`<repository>:latest@<digest>`, matching the qualified installation reference even
when the optional values tag is absent; the digest still fixes the image content.
Keep the original baseline on subsequent upgrades; do not advance it to the last installed image. The chart restores only
that original image for comparison with the immutable adoption hash. It rejects
a forged baseline, registry or field-shape change,
unadopted release, disabled dispatcher, and unrelated configuration drift.
Authentication, relay/registry configuration, source/account identities, funding
and the adoption receipt remain unchanged. Existing separately authorized
controller/runtime/pull-coverage transitions still compose with this refresh.
This is an image update, not authorization to activate a dispatcher or migrate
its identity, and dry-run success is not installed Mission acceptance.

**Kind metrics-server node address refresh:** after adoption, Docker network
recreation may change a Kind node's InternalIP without changing its hostname.
Set `metricsServer.hostAliasesBaseline` to the original sealed `hostAliases`
list and update only each alias's IP. This requires the `kind-beta` profile,
enabled metrics-server, unchanged unique single-node hostnames, and a live,
non-deleting Kubernetes Node whose InternalIP matches the selected address.
The immutable adoption hash verifies the original aliases; other metrics-server
settings remain sealed. Retain the original baseline for subsequent upgrades;
never use this upgrade-only field for a fresh install. This does not permit
production DNS overrides, arbitrary hosts, external addresses or resource changes.

**Kind kube-state-metrics readiness capacity repair:** after adoption, set
`monitoring.kubeStateMetricsResourcesBaseline` to the original sealed
`monitoring.kube-state-metrics.resources` and increase only its CPU/memory
requests and limits. This upgrade-only transition requires enabled monitoring
and the `kind-beta` profile. Both resource maps require exactly `requests` and
`limits`, each containing positive integer string quantities in millicores (`m`)
and MiB (`Mi`), with at most nine digits. Requests must not exceed limits, and no
quantity may fall below its sealed baseline. Protected publication compares
quantities by value when the API canonicalizes units (for example, `1000m` to
`1` CPU or `1024Mi` to `1Gi`); this does not permit a different amount or extra
resource fields. Retain that original baseline on subsequent upgrades. The
immutable adoption digest continues to seal images,
probes, selectors, credentials and all other configuration. Protected publication
keeps completed admission bootstrap jobs unchanged rather than rerunning them.
Cluster/host capacity admission still applies; this transition neither changes
production policy nor establishes monitoring readiness without live verification.

**Private Core publication continuity:** the maintained private-root migration
publishes the original controller at `controller.replicas=0` while its authority
is retired, then restores its captured replica intent after qualification.
SRE may remain `Provisioning` during this owned Pending publication: only a fully
validated retired continuity binding permits that temporary state, with the
original registration generation, enabled state, identities and funding intact.
Rendering does not qualify SRE or mark its consumers Ready. The
chart accepts this temporary zero only with the live namespace's owned
`preparing` publication marker and canonical retired receipt, bound to the exact
root/template, namespace, account, controller profile and TLS identity. It
compares the receipt's replica intent against the original sealed adoption hash;
it does not exclude replicas from that hash. A launched/unknown publication is
not permission to repeat it.

Carry forward `inferenceBudget.caBundle` from the already-rotated live,
release-owned catalog, not stale Helm values. The original TLS Secret UID,
changed resource version and retained retirement baseline must agree. Later
ordinary upgrades require the qualified v2 receipt, restored replica count,
matching activation epoch and qualified TLS lineage. Never replay the paused
release's saved zero replicas after finalization. Only the comparison copy uses
the original sealed CA; emitted values retain the live CA and paused/restored
replicas. Catalog grants, accounting, adoption receipts and workload identities
are not rewritten. The maintained CLI still verifies cryptographic key rotation,
CA chain, captured-pod retirement, exact public-catalog preservation and guarded
publication ownership; the chart's continuity check does not replace those
checks or authorize retirement/rotation by itself.

Credential enrollment can finish private qualification before the first ordinary
Core upgrade. In that case the chart permits initial SRE adoption with only
`schemaHook.enabled=false` and the already-rotated live CA, preserving the full
sealed values, original restored replicas, identities and funding. The qualified
v2 retirement receipt is independent authority for this CA transition: SRE may
be `Blocked` because its first-install verifier requires adoption before trusting
the rotated CA. Helm does not mark SRE Ready; the controller must subsequently
revalidate the adopted intent. Pending/paused roots, stale registration generations,
unqualified receipts and unrelated initial changes remain rejected.

Qualified continuity verifies every reviewed consumer in the release namespace,
including a separately installed Bridge BFF. Each live workload must retain its
reviewed kind, UID, namespace and template digest. Private-authority consumers
must also retain their activation epoch; reviewed workloads without private
authority are not stamped by qualification. An existing marker must still match.
The full consumer set must reproduce the retained retirement binding and include the Core
root. Duplicate, missing, foreign or changed consumers fail closed. This does not
expand the separate root-image migration command's supported review scope.

**Foundry project onboarding after SRE adoption:** an operator may configure
`foundry.projectEndpoint` without changing the working inference endpoint or
credentials. Set `foundry.projectEndpointBaseline` explicitly to the exact
original project URL (including an empty string when none was configured).
The selected URL must be a canonical HTTPS
`<account>.services.ai.azure.com/api/projects/<project>` URL without userinfo,
port, query or fragment. Substituting only the original project endpoint must
still reproduce the immutable adoption digest; all other Foundry settings,
credentials, policies, grants and funding remain sealed. This opt-in has no
default, cannot be used during initial adoption, and composes with qualified
image refreshes. Keep the **original**, not last-selected, baseline on later
upgrades. Do not replace receipts or reset accounting to onboard a project.

The shared router sends the installed Azure resource key using `api-key` for
project APIs, just as for Azure OpenAI. Confirm that the selected project belongs
to the credential's resource before applying; this transition does not replace
credentials. Workload Identity and exclusive Entra sidecar authentication retain
bearer tokens and the `https://ai.azure.com` audience. Metadata access alone does
not qualify model-backed memory operations or cross-runtime memory isolation.

Initial SRE ownership adoption still requires `Ready` authority. An already-adopted
release may repair its controller while the registration is `Blocked`, provided
its immutable adoption receipt, original identities, current observed generation
and unchanged finite funding still validate. Initial adoption requires unexpired
original funding; Helm parses `Z` as UTC independently of the operator's timezone.
An already-adopted repair can render after expiry so compatible readers can be
installed before an explicit operator renewal. The Kind beta profile requires an
upgrade with the schema hook disabled and both original intent and adoption
receipts present; the SRE transition still verifies their full sealed bindings.
Cold installs and initial adoption still require unexpired authorization.
Bridge also permits an upgrade of its existing release while that adopted Core
reports `AuthorityBlocked`, verifying Core-owned immutable receipts, their intent
UID/digest, and the existing controller binding. It still rejects blocked initial
Bridge installs, stale enrollment, foreign ownership, or missing adoption.
Rendering does not renew funding.
`Ready` requires qualified v2 privacy;
`Blocked` may retain that revision or have cleared it on failure, but cannot name
an unknown revision. Missing, unknown, pending or retired phases are not accepted.
This permits the Helm repair, not
runtime access: it does not patch registration status, issue credentials, change
funding or declare health. The controller must re-establish live privacy authority
and router readiness before any funding or customer run resumes.

Removing/changing an applied entry, replacing its ConfigMap, or allowing it to
expire without explicit renewal coverage blocks new reserve/begin and funded
Team/Task admission. Receipt content cannot be edited to renew it. The receipts
and recorded ceiling remain so already dispatched work can settle; settlement
never requires renewed top-up authorization. Frozen, closing or corrupt roots
cannot be reopened. Closed roots require the narrowly scoped, explicit expired
Bridge reactivation described below; ordinary extensions never reopen accounts
or renew expired SRE funding. Existing paused/revoked authorities are not resumed. Overflow and malformed,
foreign, duplicated or replaced grants fail closed. Invalid global funding JSON
blocks consumption rather than guessing which entries were intended.

Every account conflict retry re-reads the live operator source and verifies the
admission bundle before applying funding. ConfigMap authorization and account CAS
are different Kubernetes objects: there is no cross-object atomic transaction.
A revocation racing the final account write can overlap that one operation; the
next admission rechecks current authority. This follows existing live-authorization
snapshot semantics and is not a claim of instantaneous cross-object revocation.

Receipts use the ledger's existing preserved-field storage envelope. The shared
ledger validator enforces their structure, identity bindings and 64-entry limit;
the CRD deliberately does not newly type `ledger.tokenTopUps`, which could narrow
arbitrary values preserved by older schemas. `status.effectiveLimits` is an
optional observation outside that envelope, not new spending authority.

**Rollout:** update the CRD schema, exact admission bundle, controller/broker and
all shared-ledger readers before publishing the first entry. Empty receipts are
omitted and preserve the old ledger representation. Older deny-unknown readers
fail closed on funded ledgers; do not roll them back or strip receipts to recover.
A mixed old/new admission bundle is also intentionally refused. Source/local
checks are not native admission, deployed-runtime, provider or Kind acceptance;
those remain separate operator qualification steps.

#### Explicit expired Bridge reactivation

The same `kars budget authorize-renewal` command also supports the **existing,
sealed, expired Closed `bridge-orchestrator` account**. This is separate durable
operator consent, not an ordinary top-up, automatic reopening, authentication
bypass, or permission to turn arbitrary Closed/Frozen accounts Active. It needs
the existing protected `fund` permission and read access to the original Sandbox,
account, catalog, workspace namespace and `kube-system` namespace. No SRE
registration/intent pin is substituted for the Bridge's original catalog grant.

First install and qualify Bridge-reactivation-compatible **controller, router
and BFF** readers; SRE-renewal-only versions reject the new field. Do not publish
during mixed-reader rollout. The existing preserved ledger envelope and funding
admission policy are reused without new stores or schema exemptions.

Read `kars budget status bridge-orchestrator --kind sandbox -n kars-system`.
Its funding view includes `closedLedgerDigest` for this Closed root: SHA-256 of
the complete shared ledger serialized as compact recursively key-sorted JSON
(with an empty `tokenTopUps` map omitted). Copy it, the original expiry, exact
account/root/catalog UIDs, limits, node digest and every applied receipt ID into
an ordinary top-up-shaped grant with a **fresh ID**, positive finite additional
tokens and a future expiry extending all prior renewals. Instead of SRE pins,
use exactly:

```json
{
  "originalExpiresAt": "2026-10-20T05:57:35Z",
  "previousTopUpIds": ["existing-applied-top-up-id"],
  "bridgeReactivation": {
    "closedLedgerDigest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
  }
}
```

The example pins are illustrative, not operational authority. Expiry/digest
must be copied exactly, including fractional seconds; never edit the original
grant or truncate its timestamp. Choose finite headroom and validity explicitly.

```bash
kars budget authorize-renewal -f bridge-renewal.json -n kars-system --catalog kars-inference-budget-contracts
kars budget status bridge-orchestrator --kind sandbox -n kars-system
```

Publication only appends to protected `token-top-ups.json` using ConfigMap
UID/resourceVersion tests. The controller independently re-reads account and
original source/catalog authority on every CAS attempt. It requires the same
original grant/root/catalog/account and binding digests, an actually expired
funding chain, a single inactive root node, all sessions closed, zero reserved
liability, and no pending or observed-breach attempts. Deleted, replaced, revoked,
Frozen, Closing, unexpired, corrupt or stale authorities fail closed. Even a
previously Frozen account later closed by recovery retains breach evidence and
cannot use this transition. Raced ledger changes invalidate the entire consent
hash; reassess current state and publish a new reviewed intent rather than
altering any applied receipt. An unapplied stale intent must be explicitly
withdrawn from the protected funding list before replacing it; retain every
applied and unrelated grant unchanged.

Successful CAS changes **only** phase to Active, the original root node's active
flag, and the appended receipt. All settled/uncertain/unpriced amounts, original
limits and authority, previous receipts, historical closed sessions, attempts
and replay fences remain exact. Replaying that receipt after another closure
cannot reopen the account; a later expiry requires new consent and a fresh
complete digest/receipt snapshot. Ordinary top-up and first-install SRE behavior
is unchanged. Removing any original authority or applied grant continues to
block new execution, while historical settlement remains possible.

Preserve the complete published list in subsequent Core Helm
`inferenceBudget.tokenTopUps` values, including unrelated Team headroom grants;
never replace it with only this renewal. No root/TLS migration is involved.
After the CAS receipt is present, use the maintained runtime rollout lifecycle
for a fresh pod/session: historical pod sessions deliberately remain closed.
Verify real router readiness and an authenticated governed operation separately.
A published receipt, Active ledger or installed image is not runtime/customer
acceptance, and this renewal does not waive separate calendar or policy limits.

### Team usage observations

For governed Teams, `status.tokensSpentTotal` is the sealed lifetime account's
`ledger.meters.settled.tokens`, across all roles, runtimes and root inference.
The controller reuses the admission reader's configured catalog, current
principal binding, Team/account UID pins and validated ledger; it does not read
an unauthenticated public balance or sum subtree-inclusive node meters.
Exhausted, capacity-full and closed accounts still expose authenticated settlement
without admitting more work. Missing, unreadable, foreign, unsealed or
limit-mismatched accounts expose **unavailable**, never zero or root-only fallback.

`tokensSpentScope: GovernedInferenceLifetime` and `tokensObservedAt` identify the
snapshot. Bridge requires matching Team generation and a timestamp no older than
five minutes; older controllers without this provenance display unavailable.
Unchanged observations retain their timestamp until a reconciliation finds it
at least 60 seconds old; the normal requeue interval is also 60 seconds.
Identical status does not trigger another API write,
preventing self-generated status events from starving version-fenced Team edits.
Changed facts are published immediately; missing, invalid, future or expired
observation timestamps are refreshed after a successful read. Failed observations
still clear stale totals and timestamps, and writes retain UID/resourceVersion fences.
These are settled tokens, excluding reserved and uncertain charges—not monetary
cost. In particular, `unpricedAttempts > 0` must never be interpreted as free
inference. Invoice, compute, tool and storage costs remain outside this metric.

Non-metered legacy Teams retain `RetainedRootSynthesis` totals from retained root
run outputs only. Individual outcome rows use the same root-synthesis scope;
immutable outputs are not rewritten. Team digests carry the explicit scope and
nullable token count. The UI does not divide lifetime usage by retained outcomes:
retention and lifetime windows differ, so that ratio is not per-outcome efficiency.

### Router calendar counters

For an adopted SRE installation, an operator may raise `sre.dailyTokens` through
an ordinary Helm upgrade. Both chart and controller accept only a positive integer
above the original sealed daily limit, at most 9,007,199,254,740,991. Completed
adoption is required; the original intent and adoption receipt stay immutable.
The Helm revision and Kubernetes policy update record the selected limit. No
other policy field is exempted: monthly/per-request limits, model selection,
content safety and sandbox scope retain their original seals. Deploy the matching
controller before (or with) the increase; an older controller refuses it.

This changes only calendar capacity, not lifetime funding. A token top-up or
renewal does not itself raise the daily limit. Existing calendar usage, outstanding
liabilities and lifetime receipts are preserved; hot-reloaded policy applies the
new limit to subsequent requests without resetting counters or changing the UTC
reset boundary. Missing provider usage remains charged conservatively. A local
`budget_exceeded` 429 is not proof of exhausted Azure provider quota.

The Bridge Policies editor and daily-budget API reject direct edits to policies
with a controlling Kubernetes owner reference, including forced edits. Change the
owning Mission or other source configuration instead: its controller generates
the policy and would overwrite a direct edit. Standalone operator-authored and
Helm-managed policies without a controlling owner reference remain editable;
Helm management alone does not make a policy controller-generated. The inline
daily-budget editor displays the owning-resource rejection. On an authored policy,
setting the daily limit to zero clears only that limit, preserving independent
monthly and per-request limits. **Remove all token caps** explicitly clears the
whole policy `tokenBudget`, including monthly and per-request ceilings. The
existing budget PATCH accepts `{ "daily_tokens": 0, "clear_all": true }` for this
action; omitting `clear_all` preserves the daily-only behavior. Nonzero
`daily_tokens` cannot be combined with `clear_all`. This uses a version-fenced
merge patch, so limits owned by another apply manager are removed rather than
silently retained. It does not change funding authorization or reset accounting.

For adopted SRE, removing sealed token ceilings additionally requires its
validated registered funding grant to authorize `unboundedTokens: true`. Bounded
grants retain their sealed-limit checks. Deploy the matching controller before
removing these limits; older controllers reject the change and quarantine SRE.
Removing a ceiling is distinct from storing a zero per-request cap, which can
reject requests. Other adoption and identity checks remain enforced. The controller
handles both absent fields and the explicit JSON nulls produced by typed policy
serialization when validating authorized ceiling removals.

Core Helm renders a capless SRE policy when `global.installationProfile=kind-beta`
and `sre.firstInstallFunding` is exactly `{unboundedTokens: true}`. The finite
`sre.tokenBudget` and `sre.dailyTokens` values are not emitted in that mode; keep
originally sealed values unchanged on adopted upgrades. Removing ceilings from an
older finite policy requires completed adoption and the original unbounded grant.
Only the comparison copy restores its sealed ceilings; the emitted policy stays
capless, with funding, identities, safeguards and accounting preserved. An
originally capless install can be adopted unchanged. Other installation profiles
retain the configured finite policy defaults, even with unbounded lifetime funding.

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
broker authority, model contracts where finite ceilings require them, and provider
credentials; Kubernetes' default
one-second probe deadline is not an adequate budget for that multi-request
path under load. Slow or failed checks still make the Pod unready. Local
`/healthz` liveness and per-inference authorization checks are unchanged; no
successful authority result is cached or substituted for the live check.

Broker authority rejections retain the generic public `inference_budget_authority`
403 contract. Controller logs include fixed `authority_stage` labels for the
router-token, workload, privacy, binding and Task-lineage checks. In particular,
`task_lineage_not_ready` distinguishes a live governance-readiness rejection from
an identity mismatch; it does not authorize a retry or bypass. These diagnostics
log neither credentials, request bodies nor raw error details. API failures and
lineage contention retain their existing error categories. Correlate the stage
with current controller/Task state before attributing a denial to the provider.

Connecting GitHub Copilot through Bridge also registers its fixed endpoint and
own credential as an explicit named provider. The legacy default-route token is
retained, but a named route never borrows another provider's credentials.
For historical connections, explicitly saving the Copilot model selection
enrolls that route using the existing stored token; listing connections does not
mutate them. New or restarted sandbox routers receive the saved configuration.
This does not change the cluster default, enroll a model-bound budget contract,
or prove a successful provider request. Removing the connection retires both
named and legacy credential bindings.
Model discovery reuses the enrolled credential server-side after sign-in; it
does not ask the browser to recover or resubmit a stored token. Missing or
revoked store enrollment fails explicitly without attempting seat discovery.

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

New dispatch and calendar Begin/Read require a currently qualified privacy epoch
matching the router's projected binding. Lifetime settlement and calendar Finish
can cross a legitimate privacy migration or epoch rotation, but still perform
live Secret-denial, registration, admission and credential-guard checks. A failed
privacy check remains a denial. The original router identity, account binding,
accepted reservation and calendar writer/sequence checks remain required; this
does not authorize new inference or recover liabilities after that identity is gone.

A verified, still-pending SRE migration returns `503 inference_privacy_pending`,
not a permanent authorization denial. The shared router waits between lifetime
admission and calendar Read/Begin checks with the same broker request identity,
including the calendar writer and sequence, rereading its audience token and
revalidating live authority on every attempt. This applies to enforced and
record-only accounting. Settlement and calendar Finish do not enter this wait;
the existing idempotent calendar lost-acknowledgement retry remains unchanged.
No provider request is sent while waiting. Dropping the broker future ends its wait. Admission retains its existing
90-second worker deadline and cancellation-safe accounting handshake: disconnecting
the caller does not interrupt that handshake or authorize a provider send. Changed
epochs, revoked authority, malformed responses and ambiguous transport failures
do not enter this retry path. This
applies equally to Hermes and OpenClaw without replaying a model turn or tool call.

An unfinished call retains pending liability in the account. Replacement writers
are refused while another writer has pending work; terminating routers can still
settle against their original workload identity. Unknown transport outcomes,
cancelled streams and incomplete usage are never assigned zero tokens. When the
lifetime broker acknowledges a finalized, non-breaching uncertain settlement
charged at the exact dispatched quote maximum, the router can close the calendar
Begin with that conservative token charge. The incomplete request still fails;
this is not measured provider usage and does not refund lifetime charges.
A structured provider Content Safety refusal (HTTP 400/403 with `content_filter`
or `responsible_ai_policy_violation`) retains its original denial after both
accounts acknowledge that conservative maximum charge. This lets the existing
route classify the refusal instead of replacing it with a generic accounting
error. It does not assign zero usage, refund uncertainty, permit provider fallback,
or hide missing settlement acknowledgments, breaches or failed calendar commits.
For calendar-only dispatch, the same explicit refusal can be preserved when an
authenticated, current calendar read confirms `record-only` policy and pending
liability. The acknowledged Begin remains pending: no Finish, zero-token usage,
or estimated charge is recorded, and uncertainty remains latched. This exception
cannot hide a failed read or commit, a revoked policy, or any lifetime settlement
failure. Production `enforce` policy retains the accounting error.
Successful responses with incomplete usage still fail closed.
Provider HTTP 429 and 5xx responses also retain their original status, body and
retry headers after the same acknowledged conservative settlement or confirmed
record-only uncertainty. They remain failed provider requests, not successful
accounting results or free attempts. Existing reviewed retry/failover policy can
then classify the provider response; this exception does not add retries or widen
provider authority. Missing acknowledgments, breached settlements and failed
calendar commits still return accounting errors and cannot trigger failover.
Structured HTTP 400 parameter errors (with a nonempty string `error.param` and
an absent, null or string `error.code`) likewise preserve
the original error only after that same acknowledged settlement. This applies to
both buffered and streaming proxy requests. It exposes model/request incompatibility
to the caller without classifying the request as free, making it retryable, or
clearing pending liability. Malformed, oversized, or unrecognized errors still
fail closed when usage is missing.
For buffered responses without verifiable usage, the router logs the original
upstream HTTP status before settlement can replace it with an accounting error.
The diagnostic includes only an allowlisted error-parameter name (or `other` /
`unavailable`) and a structured Content Safety rejection boolean, never provider
messages or response bodies. An accounting 503 is
not evidence that the provider returned 503. These diagnostics do not authorize
retries, classify arbitrary validation errors as free, or alter any charges.
Once dispatch has acquired both guards, a dropped request or stream transfers them
to one settlement-only worker. The same worker owns normal settlement, so
cancellation while awaiting a broker receipt or calendar commit cannot split the
pair. It runs for at most 60
seconds (including calendar lock waits), never sends inference, and closes the
calendar only after the acknowledgement above when no complete provider usage is
available. Cancellation during settlement preserves any already-collected complete
usage; it does not replace it with an invented measurement or a second charge.
Missing/failed acknowledgements, compacted receipts without a charge, breached or
mismatched receipts, worker timeout, and runtime shutdown still retain unresolved
liability. Begin acquisition runs in a separate accounting-only worker, bounded to
90 seconds for the calendar and lifetime RPCs. Caller cancellation cannot abandon
a calendar Begin while the lifetime broker is still answering: a refused admission
closes the calendar at zero, while an abandoned grant follows the existing
conservative settlement path. The worker never sends a provider request. Timeout
or runtime shutdown can still leave pending liability; neither is evidence of
zero usage or permission to erase a reservation.
Calendar-only cancellations have no lifetime receipt and remain
fail-closed. There is no asynchronous assumption that lifetime settlement succeeded.
Specific structured model/API rejections before inference can close a Begin with
zero, preserving API negotiation without treating arbitrary provider failures as
free.

This is a forward repair only: deploying it does **not** reconcile already-stranded
legacy calendar Begins. The calendar ledger has per-writer sequences and pending
counts, not a durable correlation to individual lifetime attempts. Unresolved
legacy liability still needs trustworthy reconciliation, not deletion of the
account, changes to historical grants, refunds of uncertain charges, or creation
of a fresh balance. Automatic recovery of that state is not implemented.

#### Explicit conservative recovery of an orphaned standalone writer

`kars budget reconcile-calendar -f authorization.json` inspects a narrowly bounded
operator recovery; add `--apply` to persist it. Install the cancellation repair
first and let its rollout retire the old writer Pod. A Pod that still exists,
even if unready or terminating, prevents recovery. This command uses operator
Kubernetes credentials with read access to the pinned Sandbox, namespaces, Pods
and lifetime account, plus get/patch access to the sealed calendar ConfigMap.
It never reads router credentials, impersonates a router, sends inference, funds
an account or changes the lifetime ledger.

The authorization JSON contains exactly these fields (use observed IDs and bounds,
not the illustrative values below):

```json
{
  "namespace": "kars-system",
  "sandbox": "sre",
  "sandboxUid": "observed-sandbox-uid",
  "accountUid": "observed-lifetime-account-uid",
  "calendarUid": "observed-calendar-configmap-uid",
  "writer": "observed-calendar-writer-id",
  "sequence": 45,
  "attemptSequence": 13,
  "maximumTokens": 408192
}
```

Eligibility requires a standalone governed Sandbox root, unchanged cluster,
workspace/runtime/account identities and ownership, one orphaned pending Begin,
and exactly one retained lifetime attempt already finalized as `Uncertain` at
its quoted maximum. There must be no reservation, breach or other uncertainty.
The current-period calendar monthly total must equal **all settled lifetime
tokens**, so adding the existing uncertain maximum covers the entire standalone
lifetime total. This is a conservative aggregate bound, **not proof that the two
legacy receipt IDs identify the same provider request**. Ambiguous histories,
Team/shared accounts, missing coverage, cross-period accounting and inexact
amounts are refused; this is not a general ledger repair tool.

Application rechecks lifetime state and performs one calendar UID/resourceVersion
CAS. The closure uses the existing Finish representation and increments its writer
sequence, together with an atomic `kars.azure.com/calendar-reconciliations` audit
annotation. That audit retains the original writer/counters, lifetime attempt,
before/after hashes, authorization and explicit unknown-provider-usage status.
Other writers, ConfigMap data, historical grants and lifetime charges remain
unchanged. A consumed authorization or CAS conflict is never replayed automatically.
The annotation is required provenance: this is an **operator conservative closure**,
not a fabricated router acknowledgement or measured provider usage. Its maximum
may exceed actual usage; it does not refund or duplicate the lifetime charge.

##### Whole-account bound for a retired multi-attempt writer

The original authorization and eligibility rules above remain unchanged. A
separate explicit `calendar-account-bound/v1` authorization supports an orphan
with multiple retained attempts **without guessing its paired request**:

```json
{
  "mode": "calendar-account-bound/v1",
  "namespace": "kars-system",
  "sandbox": "sre",
  "sandboxUid": "observed-sandbox-uid",
  "accountUid": "observed-lifetime-account-uid",
  "accountResourceVersion": "observed-account-resource-version",
  "calendarUid": "observed-calendar-configmap-uid",
  "calendarResourceVersion": "observed-calendar-resource-version",
  "writer": "observed-calendar-writer-id",
  "sequence": 77,
  "podUid": "observed-retired-pod-uid",
  "podName": "observed-retired-pod-name",
  "maximumTokens": 20571527
}
```

Use the **observed** `settled.tokens + uncertain.tokens` as `maximumTokens`; the
number above is illustrative, not a configurable discount. Do not include an
`attemptSequence`. Inspect with `kars budget reconcile-calendar -f authorization.json`,
then use the identical command with `--apply` only after reviewing the audit and
installing compatible readers, including the updated controller, before writing
any new marker. Older controllers reject this durable representation; readers
that only support Begin tombstones also reject Finish tombstones. Existing Begin
markers remain valid and unchanged. The `calendar-account-bound/v1` proof and
schema are unchanged; accepting a last Finish does not fabricate a new Finish.

Proof requirements are deliberately stronger than the original mode:

- Bootstrap is sealed, with precisely the standalone root node and matching
  account/node cumulative meters. All original namespace, owner, root, scope,
  UID, Active phase and same-day/month checks still apply. Exactly one unfinalized
  writer must have `pending: 1`. Its last operation may be Begin (zero tokens) or
  Finish (zero or nonzero tokens already covered by the prior monthly counter).
  Interleaved Begins/Finishes can leave one request pending after a Finish; that
  last Finish is **not** identified as the unfinished request.
- The exact writer Pod UID and name identify a retained lifetime session with
  the same Sandbox/runtime/task identities. Every retained session is closed
  and every such Pod UID is absent, not merely terminating or unready. A new
  router Pod that has not enrolled a session may exist while calendar readiness
  blocks it. Session replay windows must be intact: every sequence strictly above
  `closedThrough` and below `nextSequence` must be retained. Below or at the
  watermark, only positive-sequence Uncertain tombstones from closed sessions
  are eligible; missing uncertainty, retained settled/expired rows there, and
  attempts at or beyond `nextSequence` are rejected.
- All retained attempts are terminal and belong to their retained sessions;
  reservations are zero, no breach exists, every uncertain attempt is charged
  at its quoted maximum, and their sum equals cumulative uncertainty. Retained
  settled charges cannot exceed cumulative settled meters. Compacted settled
  history is **not** reconstructed or discarded: its cumulative charge remains
  in the bound.
- Both resource versions must match the authorization. Application rechecks
  the full lifetime ledger/version/seals, Sandbox, runtime and absent session
  Pods, then atomically CAS-patches the calendar and audit. Contention aborts;
  it does not select a different attempt or retry with an unreviewed bound.

For the authenticated governed Pod, provider dispatch follows calendar Begin,
then lifetime reservation/InFlight acknowledgement, then send. An orphan before
lifetime acquisition cannot have sent inference. Otherwise, closing the session
leaves that send either settled, charged at its full uncertain maximum, or
expired before send. Lifetime compaction preserves cumulative settled charges
and every uncertainty tombstone. Consequently **the entire cumulative account
charge bounds any one orphan**, even when its individual request is unknowable.
No calendar usage is subtracted from this bound: doing so would assume a missing
correlation. This intentionally overcharges rather than restoring budget.
Calendar-only Pods with no matching lifetime session, shared accounts, missing
history, in-flight work, and cross-period evidence are not eligible.

The operation adds the full bound to both current daily and monthly counters.
It retains the exact original writer receipt: sequence, token field, event and
`pending: 1`, adding an explicit `orphanFinalization` tombstone. A prior Finish
charge is neither refunded nor subtracted from the full bound. Controller reads
require the matching atomic audit, including the original receipt and before/after
counters; the original token charge must be covered by the pre-recovery monthly
counter, not by the newly added bound. Other writers can proceed only after that validation; the retired
writer cannot replay Begin or send a late Finish to replace its tombstone. New
pending liability from another writer still blocks readers normally. Unknown
actual usage, uncertainty, all historical lifetime charges and all identities
remain unchanged. No router credential, model call, lifetime write or funding
operation is involved.

Replaying the identical authorization returns the retained audit with
`alreadyApplied: true` and makes no write, even after an unconfirmed client
response. Altered authorization or altered tombstone is rejected. Audit entries
and size limits remain enforced. This operation can exhaust daily/monthly caps;
those caps continue to block admission. Removing an orphan readiness blocker is
**not** permission to refund this conservative charge, raise a cap, or promise
new dispatch capacity.

##### Retired session that never accepted a reservation

`calendar-never-reserved/v1` uses the same version-pinned authorization fields
as whole-account recovery, but requires `maximumTokens: 0`. Install the compatible
controller calendar reader before applying this distinct marker version. The
router's calendar RPC still exchanges the unchanged `Read`, `Commit`, and `Usage`
representations; it does not deserialize the persisted operator tombstone.
It is not a discount option for an unknown attempt: all sealed standalone,
closed-session, absent-Pod, uncertainty coverage, and recheck/CAS requirements
above still apply. Additionally, the exact retired writer's lifetime session
must have `nextSequence: 1`, `closedThrough: 0`, and no attempts; its calendar
receipt must have zero tokens. Reservation acceptance advances that sequence
before provider dispatch, and compaction never rewinds it. Missing sessions,
accepted or compacted attempts, existing writer charges, and live work therefore
cannot use this mode.

The original receipt (including its pending count and event) is retained with an
operator-only tombstone and an atomic audit of the closed-session proof. No
router Finish is synthesized. Both calendar periods and counters remain exactly
unchanged, as do all settled/uncertain lifetime charges, historical conservative
calendar charges and funding. A past day/month is allowed only because this
proof establishes no additional dispatch liability for this session; no counter
is rolled forward or reset by recovery. Future or inconsistent periods fail
closed. Normal calendar admission performs its existing period rollover later.

Readers validate the corresponding audit and fence every replay from the retired
writer. Identical authorization replay is read-only; a changed proof, receipt,
UID or resource version fails closed. This mode does not claim measured provider
usage, increase funding, or promise capacity for the next request. Where this
exact never-reserved proof is unavailable, the existing conservative modes and
their same-period restrictions remain unchanged.

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

Failed reviewed phases may retain partial response text for diagnosis in either
runtime. The canonical output must exactly match the durable failed reply; it is
not a successful artifact and cannot authorize principal synthesis. Rejected
replies keep their canonical output empty.

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

### Built-in Hermes SRE and Bridge orchestrator

The source enrollment path also supports a `KarsSandbox` root for the built-in
`sre` (Hermes only) and `bridge-orchestrator`. It uses the **actual Sandbox UID**,
workspace namespace UID and cluster UID, not a synthetic Task or a display-name
account. Existing Task/Team account names, limits, histories, calendar counters
and pending-liability semantics are unchanged.

Operators explicitly configure `inferenceBudget.builtInFunding` on the Core
chart. It is stored as `built-in-funding.json` in the existing operator budget
ConfigMap, not in a Pod environment variable. Enrollment, recovery and dispatch
read the current declaration without changing the qualified controller template.
An unavailable or malformed configuration refuses new dispatch; it is not treated
as an empty declaration or permission to reset an account. Removing a declaration
blocks new dispatch, while already-recorded liabilities can still settle.
Bridge selects orchestrator funding from that same live catalog, using the
controller's literal `KARS_INFERENCE_BUDGET_CATALOG` setting (or its default).
It rejects the retired `KARS_BUILTIN_INFERENCE_FUNDING` environment declaration
rather than using it as a second authority. Before sending a model request it
rechecks the catalog identity/declaration and controller configuration; a failed
funded route never falls back to a direct endpoint or another sandbox.
After cold-install SRE adoption is sealed, an ordinary Core upgrade may add a
separately governed `bridge-orchestrator` declaration in the release namespace.
The SRE comparison preserves its immutable adoption receipt, original funding,
and every other actor declaration; it does not emit projected values or rewrite
history. This transition is not allowed during initial SRE adoption. Controller
enrollment still checks the orchestrator's live UID, fresh history, contracts,
and account before permitting inference.

Funding declarations require the broker to be enabled. Each entry has:

```yaml
root:
  kind: KarsSandbox
  resource:
    namespace: <source-workspace>
    name: sre # or bridge-orchestrator
    uid: <exact-sandbox-uid>
  workspaceUid: <exact-workspace-namespace-uid>
  clusterUid: <exact-kube-system-namespace-uid>
limits:
  tokens: 100000 # explicitly reviewed lifetime allocation, NOT a daily limit
expiresAt: <explicit-RFC3339-expiry>
```

`tokens` must be positive and fit a signed 64-bit integer. An optional
`usdMicros` maximum requires the same maintained provider-price contracts as
Task/Team accounts. A zero/omitted currency maximum is still unbounded money,
not free inference. No actor funding is supplied by default. Configuring
funding without the broker fails closed rather than selecting calendar-only
accounting. Finite effective ceilings require model/provider/endpoint bounds
already enrolled in the existing broker catalog; funding does not create or relax
those contracts. Explicit fully unbounded funding uses record-only accounting
without a per-model spending contract.

Enrollment requires either the existing **fresh namespace** proof (the
Sandbox-UID-bound marker, no sealed calendar and no Deployment), or a narrowly
verified **never-attempted sealed calendar**. The latter retains the existing
Sandbox, namespace and calendar-account UIDs. It requires authentic namespace
ownership, the sealed account UID, its Namespace controller owner, zero counters
and an empty retained writer-receipt map. Calendar writers are never pruned:
even a completed zero-token attempt prevents this enrollment, including after
month rollover. Missing history, a deleted workload, a telemetry snapshot or
zero calendar totals alone are not proof of no prior attempts.

The controller atomically sets `lifetimeOnly` using the calendar account's UID
and resourceVersion. A racing provider Begin either commits first (and prevents
enrollment), or loses to the fence (and is refused on retry). After fencing,
calendar commits require a server-verified router Pod whose lifetime binding
matches the live Sandbox, runtime namespace incarnation and privacy epoch.
Calendar-only routers cannot write through the fence. Interrupted enrollment
can resume while the same fenced calendar remains unused. No receipts or
counters are reset and no replacement account is synthesized.

This exception is not migration of used or unknown history. Such actors still
require complete prior charges, conservatively bounded unresolved liabilities
and a verified admission drain; that general migration is not implemented.
Explicit funding remains mandatory. Enrollment may roll the Pod, so operators
must preserve writable runtime state before activation: an unused calendar is
not evidence that files or mesh identity databases are disposable.

Before first execution, publish the exact funding intent in the shared operator
ConfigMap, then admit the fresh source. Existing namespace ownership,
SRE registration/privacy checks, budget admission policies, router-private
audience token and digest-qualified router image remain prerequisites. The
controller signs an account bootstrap, pins its API-generated account UID in
the protected Sandbox `spec.inferenceBudgetRef`, initializes the ledger and
registers the source authority. An interrupted pending bootstrap can resume;
missing/replaced/sealed-corrupt accounts cannot initialize a new zero balance.
Legacy wire fields named `taskUid`/`rootTaskUid` carry the real Sandbox UID for
this root kind; no Task resource or fictitious Task identity is created.

The existing router client then uses the same catalog → session → reserve →
begin → settle path for every supported final provider send. Root totals include
settled charges, uncertain charges and all outstanding request bounds across
Pod replacements and concurrent requests. A request whose bound cannot fit the
remaining lifetime allocation is refused before provider dispatch, even if a
daily counter is below its threshold. The calendar limits still apply
independently and are never raised to make funding work.

Broker authorization checks the live source and exact immutable funding intent.
Both reservation and begin also check the recorded allocation expiry inside the
ledger transition. Expired/removed/changed declarations, source reincarnations,
missing contracts required by finite ceilings, and account replacement deny new dispatch; recovery
closes invalid sources and retains their liabilities. Settlement from a verified
original Pod session remains possible after funding expiry. Changing a limit or
expiry is not an implicit renewal and cannot reset prior charges.

**Bridge escape boundary:** before any composer model send, Bridge reads the
Core controller's live funding selection and the standing Sandbox's persisted
account pin. Selected funding, including expired declarations, or any surviving
pin explicitly refuses both the direct-endpoint override and arbitrary-Sandbox
fallback. Removed declarations cannot downgrade a pinned source. Malformed,
indirect or unreadable funding state fails closed. These checks use the existing
`BRIDGE_CORE_NAMESPACE` setting; no second funding flag or budget protocol is
introduced. SRE-only funding does not select orchestrator funding.

Finite-selected compose instead resolves the exact declared `bridge-orchestrator`
Sandbox UID, protected account pin and current Active/Ready/LedgerValid durable
account. It verifies namespace incarnations, the single Ready Pod's
Pod→ReplicaSet→Deployment ownership chain, qualified router image, current
controller-authored binding and privacy epoch. Pod Ready alone is insufficient:
the exact Pod must also pass the governed `/readyz` broker/contract check. Bridge
rechecks the Pod UID/spec, source pin and account readiness before forwarding, using its existing
Kubernetes Pod-proxy chat/messages methods. The router, not Bridge, supplies its
private Pod identity and performs the maintained reservation/begin/settlement
protocol. Final broker authorization still occurs on every provider send; this
read-only resolver does not create a session, reserve tokens or grant authority.
Model/provider errors, empty responses and any failed verification are terminal:
no direct endpoint or other Sandbox is tried. The caller's model choice remains
subject to router policy, provider contracts and the account ceiling.

With no selected funding or account pin and readable state, legacy direct/fallback
behavior remains available, but is not described as finitely funded. Installation
requires BFF `get` permission for the exact Core `karsbudgetaccounts` resource,
as well as its existing workload, namespace and Pod-proxy reads. The opt-in
delegation below supplies only that exact-account read; neither maintained base
ClusterRole grants budget access. Missing permission explicitly refuses compose.
Installation also
requires draining previously admitted legacy model calls before activating
funding. No funding is installed, no live actors are migrated, and no customer
acceptance is claimed here.

For the presently reviewed standing-actor allowances, **SRE 202403 tokens and
orchestrator 202402 tokens cannot cover even one enrolled Foundry gpt-5.4-mini
reservation of 280192 tokens**. Enrollment alone therefore cannot make that
route runnable. This implementation neither lowers the conservative provider
bound nor changes those allowances or their `2026-10-13T12:19:35Z` expiry.
Any route/funding change needs separate authorization and qualification.

#### Bridge account-read delegation

After Core has enrolled and pinned the **existing, explicitly authorized**
orchestrator funding, configure the maintained Bridge chart:

```yaml
core:
  namespace: <existing-Core-namespace>
namespace: <existing-Bridge-namespace>
rbac:
  serviceAccountName: <existing-BFF-ServiceAccount-name>
  fundedOrchestratorAccount:
    enabled: true
    sandboxNamespace: <existing-Sandbox-workspace>
    sandboxUid: "<reviewed-current-Sandbox-UID>"
    accountUid: "<reviewed-current-budget-account-UID>"
```

These are operator-supplied identity pins, not funding declarations. Account name
is derived as `inference-budget-<sandboxUid>`; no arbitrary account-name override
or automatic discovery by list is supported. Empty `sandboxNamespace` selects
`core.namespace`. Enabling requires a cluster-connected Helm operation with
permission to read the exact Sandbox, account, namespace incarnations and any
existing delegation, and to create/bind the resulting RBAC. The BFF cannot grant
itself access. Offline rendering with this opt-in fails rather than trusting
unverified values; disabled/default rendering creates no account permission.

`templates/orchestrator-funding-read.yaml` verifies the source UID, persisted pin,
sealed account UID/root and workspace/cluster UIDs before emitting a **namespaced
Role and RoleBinding in Core**, owned by this Helm release and garbage-collected
with the selected account UID. The only rule is `get` on
`karsbudgetaccounts`, restricted by one `resourceNames` entry. The only subject
is the configured BFF ServiceAccount in the Bridge namespace. No list, watch,
write, status-subresource, SRE/Task/Team-account or budget-Secret access is added.
Neither account balances nor funding/provider contracts are modified.

The delegation is independent of `rbac.create`, so an externally maintained
base ServiceAccount/ClusterRole can use it with `rbac.create=false`.
`bridge/deploy/rbac.yaml` intentionally remains permission-equivalent to the
chart's base ClusterRole: raw-manifest installs need this additional, explicitly
selected delegation too. Operators using raw manifests can render/review only
`templates/orchestrator-funding-read.yaml` with Helm's `--show-only` and
`--dry-run=server`, retaining its release ownership metadata, then apply those
two documents through their existing authorized manifest workflow. They must
also remove those two documents on revocation; a render-only workflow has no
Helm release tracking to remove disabled resources. All normal chart ownership
and private-consumer upgrade guards remain in force; do not bypass them to
render an existing protected installation.

Re-rendering accepts only the same release/account-owned Role and binding with
unchanged exact rules and subject. Foreign ownership, changed account owners,
broadened rules or a changed subject fail rather than being adopted/overwritten.
Retire the old delegation explicitly before changing its account or subject.
For Helm-managed installs, disabling the opt-in or uninstalling removes the
delegation; no `keep` policy retains access.

Kubernetes RBAC binds **resource names**, not object UIDs: account ownership and
install-time UID checks do not turn GET into a UID-conditioned operation.
The BFF's live source/account/Pod UID checks and the router's existing broker
authorization remain mandatory. Account replacement must revoke the delegation
before reuse; garbage collection is not instantaneous. This wiring neither
promises automatic reconciliation/rotation nor requires a new Rust protocol.

Offline regression command (existing CLI dependencies and Helm required):

```bash
cd cli
node node_modules/vitest/vitest.mjs run src/testing/helm-orchestrator-funding-read.test.ts --maxWorkers=1
```

Focused native qualification (run serially by the build/install owner):

```bash
cargo test --package kars-controller inference_budget
cargo test --package kars-inference-router inference_budget
cargo test --manifest-path bridge/bff/Cargo.toml routes::compose::client::tests
```

These selectors cover the new `builtin_` ledger/source tests plus the maintained
store, authentication, calendar, recovery and dispatcher regressions, and
Bridge's exact funded-route chat/messages, stale account/identity/readiness
refusal, terminal router error and legacy-route compatibility tests. Rust
compilation and installed acceptance remain separate from offline Helm tests.

### Task and Team accounts

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

Team Pause stops scheduling new runs; it does not suspend already-authorized
work. Current runs, including their later reviewed phases, may finish with normal
reservations, settlement and recovery. Their unchanged account nodes and Pod
sessions stay open. New independent members register dormant while paused,
including beneath a dormant parent. Emergency stop, authority revocation,
credential holds, deletion and budget enforcement remain separate safety controls.
Let current work finish before editing members or permissions: changing authority
can still stop that work. Resume enables future scheduling and reactivates eligible
ancestry in parent-first order without resetting spending or reopening closed Pod
sessions. Pause is not process checkpointing or resumable compute hibernation. A halted or launch-disabled Team task prevents reactivation
of its closed node and closed descendants; archived runs do not generate account
writes merely because the Team resumes. Fresh pre-launch enrollment and persistent
principal/member bindings retain their existing behavior. Runtime enrollment and
each provider send still require their separate live admission checks.

### Mission STOP and reviewed-phase compute retirement

The existing Mission halt endpoint records a persistent `kars.azure.com/halted`
execution gate. The controller propagates it through same-namespace,
UID-owned Task descendants before using the ordinary owned Sandbox and
InferencePolicy cleanup path. STOP is asynchronous and retryable; reviewed
scheduling cannot reverse it, and new execution requires a new Mission run.
Blocked Missions remain stoppable while launched or still attached to execution.

A reviewed phase can also retire compute after a verified failed or rejected
terminal publication: matching run completion, current reviewed authority,
correlated terminal journal, and immutable canonical output/artifact custody
are required. This does not turn failure into a successful handoff or permit
synthesis. Missing, uncertain, mismatched or in-flight evidence retains compute
unless explicitly halted. Pending budget admission does not pin a verified
terminal phase's compute. Neither cleanup path deletes Tasks, publication
records, receipts or ledgers, refunds spending, or fabricates missing usage.
Finite-runtime admission remains controller-only.

For Team work, the same verified specialist failure also releases the exact work
claim when no principal output exists because synthesis could not run. The work
returns to pending with a failed attempt in its existing history. Existing
reviewer correction instructions remain attached to the work for the next fresh
run; the generic failure explanation stays in the failed attempt's history and
does not replace those instructions. Failed or cancelled revisions also retain
the last rejected delivery as the revision input, with its original UID, nonce,
output digest and artifact-custody checks; a later successful delivery supersedes
it. A latest failed synthesis also supplies its independently successful final-phase
specialist exports as separate `revision-unaccepted-rR-pP` diagnostic inputs after
verification of its failure receipt, sealed inputs and exact retained graph. These
never replace the rejected root's named bytes or count as accepted delivery. A
verified failure before synthesis adds no partial final-phase set; the earlier
rejected input remains available. Incomplete or changed synthesis evidence fails
closed. Diagnostic-bearing revisions store sealed source/hash references rather than
copying their full bytes into Kubernetes annotations. Reference annotations retain the
192-KiB limit; resolved revision, memory and current dependency documents share the
existing 152-file / 384-KiB serialized input budget and 64-KiB per-file limit. Every
referenced producer must still pass retained custody checks, even when its historical
file is not mounted for the current step. Prior delivery always remains mounted.
Original diagnostic role/phase names are bound to the retained plan; a historical
specialist file is superseded in a step's working set only by a verified current
**dependency** export matching role name, phase name and original filename. The full
historical manifest and original bytes remain retained and unaccepted; current
exports keep distinct aliases and producer identities. Unmatched files remain mounted,
including files from renamed roles/phases, and manifests without routing metadata
keep their prior all-history behavior. This avoids duplicating the complete old and
current specialist packages at QA/synthesis without changing any safety ceiling.
The 384-KiB bound protects serialized Mission/native input transport and retained
records, not inference funding; separate annotation (192 KiB), retained-record
(900 KiB) and Kubernetes ConfigMap (1 MiB) ceilings remain intact. An assignment
whose selected complete inputs exceed the aggregate budget still fails closed;
there is no size-driven truncation or eviction. Rejected-delivery-only revisions
retain the legacy inline 16-file / 192-KiB format. Cancellation supplies no additional
artifacts. The root run is idled without fabricating
a principal reply, approval, artifacts or usage. If the
controller restarts or root idling conflicts after recording history, it verifies
and reuses that history's exact failure digest, even if another parallel phase
has since failed. Changed or missing terminal evidence is not silently replaced.

## Reserve, authorize one send, settle

All ancestor meters and the root meter are updated in **one Kubernetes object
status PUT**, with the current UID/resourceVersion and bounded conflict retries.
No router-local cache is balance authority.

1. **Reserve:** for bounded work, hold the complete configured maximum input/context
   plus the request's enforced output maximum and corresponding maximum price.
   Fully uncapped work instead records the exact route and wire digest under a
   one-time identity with no spending reservation.
2. **BeginDispatch:** atomically consume the one-time dispatch permission. Only
   the first transition returns permission. A lost acknowledgement does not
   authorize retransmission of that attempt.
3. **Settle:** trustworthy complete provider usage reduces a bounded reservation
   or directly records uncapped consumption. Missing, malformed, incomplete,
   disconnected, cancelled, or uncertain bounded work is charged at the full
   reserved maximum. Record-only work instead retains an unknown-usage tombstone
   and counter, never a fabricated maximum or successful zero charge.

The boundary is every **actual final provider/model/transformed-wire send**,
including compatibility translations and each retry/fallback. A broker denial
is not a provider health failure or permission to try another provider.
Output filtering does not refund already-performed inference.

### Reservation refusal is not provider throttling

The private broker distinguishes `inference_budget_exhausted` (the full maximum
reservation cannot fit a root/ancestor token or configured-price ceiling) from
`inference_budget_capacity` (retained ledger identities, sessions, attempts or
replay-window capacity). The router preserves these codes from the broker's
bounded error envelope during session/reservation admission and returns an
OpenAI-compatible **HTTP 400**, `error.type: invalid_request_error`, the specific
`error.code`, an actionable message, and `x-should-retry: false`. These refusals
stop the request before provider dispatch; unchanged native SDK retries are not
a recovery action. Positive remaining tokens do not guarantee that the configured
maximum input plus enforced output reservation fits. Review the governing
limits/capacity before explicitly resubmitting; this response adds no funding,
changes no quotes, and resets no charges or replay fences.

Private broker wire statuses remain compatible (429 for exhausted ceilings, 503
for capacity); only an explicit recognized code with its matching status is
classified as an admission refusal. Broker transport errors, unrecovered contention,
unknown or malformed error envelopes retain unavailable/retryable HTTP semantics (503,
or 429 for an unclassified broker throttle), without granting provider fallback.
Actual provider 429 responses keep the existing provider handling and are never
reclassified from their body as a governed reservation refusal. Calendar budget
and post-dispatch settlement behavior are unchanged.

The private broker maps `StoreError::Contention` to
`503 inference_budget_contention`. This includes exhausted CAS retries and store
operation deadlines (potentially during reads, history work or writes); the code
alone does not diagnose which stage failed. Only `/v1/reserve` retries this explicit
response from a valid, at-most-8192-byte error envelope, waiting one second and
reusing the serialized request, including root/account, execution identity,
sequence, quote and wire digest. A timed-out write may have committed: an identical
reserve reads that existing reservation rather than charging twice. Each retry
rereads the private audience token and revalidates live authority. It does not
refresh the session sequence, re-quote the wire, or renew the reservation expiry.
The existing 90-second admission-worker deadline covers the entire handshake,
including all retries. Disconnecting the caller leaves that worker owning the
handshake until completion or deadline; an abandoned grant is conservatively
settled, never sent to the provider. Dropping the reserve RPC at the deadline
stops its retry loop, retaining any ambiguous liability for durable recovery.
Expiry, revocation, session closure, replay fences and capacity remain authoritative.
Wrong statuses/paths, unknown/malformed/oversized errors, transport failures,
Begin and settlement are not retried by this contention rule. Existing explicit
privacy-migration handling is unchanged.

Only provably **undispatched Reserved** attempts expire/refund (30-second maximum).
An InFlight attempt never refunds merely because its TTL, router, Pod, or Task
disappears. Recovery commits stale bounded InFlight maxima, or records explicit
unknown usage for record-only attempts, after the transport deadline and a margin.
Cancellation closes sessions/subtrees without erasing spend.
Quota held by active reservations, pending enrollment, and catalog/store outages
block **new admissions**, not revoke already funded executions. Team seats/runs
retain their Task UIDs and launch intent during such waits; the Task controller
does not tear down an existing execution solely for a budget-admission failure.
Explicit pause, removed authority, invalid policy and UID replacement retain
their normal fail-closed revocation paths. Every new send still needs the broker.
Normal terminal rows compact behind durable Pod sequence high-water marks.
Uncertain attempts retain their contract as tombstones: late over-bound usage
freezes the account without granting another refund. In a closed Pod session,
the sequence high-water mark advances past retained uncertainty tombstones so
later settled rows can be compacted. The tombstones, closed session identity,
and cumulative charges remain; replay cannot reopen that session. Live sessions
retain their contiguous replay-window checks. Normal broker recovery compacts
already-closed sessions without resetting accounts or enlarging capacity.
Anthropic streams require a valid final usage-bearing `message_delta` with a
recognized stop reason, followed by `message_stop` and clean transport completion.
Start-of-message usage or a terminal event alone never proves final output usage.
Missing, malformed, reset, decreasing or inconsistent final evidence commits the
complete bounded token and maximum-price reservation, or retains unknown usage
for a record-only attempt.

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
- **Blocked / AuthorityRevoked:** all enrolled task authorities are closed.
- **Blocked / NodeCapacityReached**, **SessionCapacityReached**, or
  **AttemptCapacityReached:** bounded accounting storage cannot enroll another
  task, runtime session, or attempt, respectively. These are not spending
  limits; increasing token allowances does not free storage. Eligible closed
  identities can move into immutable history (see resident capacity below), but
  neither identities nor replay fences may be discarded to unblock work.
  Already enrolled work remains subject to per-request admission;
  a blocked observation does not revoke it or alter historical accounting.
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

For multiple active Task authorities, each recovery scan reads a fresh namespaced
Task inventory instead of issuing one GET per authority. The inventory is bounded
to four pages of 128 Tasks with a consistent resourceVersion; only a completed
inventory proves absence. Any unresolved names after that bound receive fresh
individual GETs. No inventory or authorization result is reused across scans.

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

For Chat clients, the shared router uses its existing Responses translation when
current contracts cover Responses but not Chat Completions for the exact selected
provider, endpoint and model. This avoids an inadmissible Chat probe; it does not
add a Chat contract, switch providers, or bypass final Responses reservation and
settlement. Contract-derived routing is checked afresh rather than cached.

Match `outputField` to the actual wire operation, not just the model family:
`MaxTokens` selects `max_tokens`, `MaxCompletionTokens` selects
`max_completion_tokens`, and `MaxOutputTokens` selects `max_output_tokens`.
For Chat Completions, the shared router normalizes a single `max_tokens` or
`max_completion_tokens` cap to the selected contract's field without changing its
value. This supports loopback clients that cannot infer the upstream endpoint:
pinned Hermes selects `max_tokens` for Claude through the local router, while
OpenClaw's native Copilot Chat path uses `max_completion_tokens`. Both therefore
use the same `MaxCompletionTokens` contract. Supplying both fields (even with equal
values), a nonpositive/noninteger cap, or a cap above the contract maximum remains
rejected before reservation or provider send. The canonical wire bytes determine
the quote and dispatch binding. No aliases are introduced for Responses or
Anthropic Messages; the separate Anthropic Messages contract uses `MaxTokens`.
The local SDK transport label `azure-openai` does not identify the upstream:
use router observations to verify the selected provider. Correcting a reviewed
contract does not grant funding, reset usage, or change the default provider.

The initial closed operation families are text Chat Completions, Anthropic
Messages, and Responses. Contracts reject hosted tools, multimodal generation,
async/background work, stateful prior responses, multiple output candidates,
unknown request options, and incompatible output-limit fields. Responses accepts
Hermes's bounded `prompt_cache_key` (nonblank, at most 64 UTF-8 bytes) and
`include: []` or `include: ["reasoning.encrypted_content"]`. These response-selection
and cache-routing hints do not authorize provider-side conversation state, hosted
tools, or replay of opaque reasoning input. Replayed assistant messages may retain
`phase: "commentary"` or `phase: "final_answer"`; other phase values and phases on
non-assistant messages remain rejected. This preserves native multi-turn history
without authorizing server-side state. The final wire-size and token/price
bounds still apply without a cache discount. Embeddings,
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
Mediated HTTP egress follows the reviewed, controller-published Kars egress
policy, without a second global catalog grant for ordinary approved destinations.
Configured/catalog model hosts remain broker-only regardless of that policy.
The optional operator `nonInferenceEgressHosts` catalog still classifies
learning-mode destinations; learning alone cannot create such an exclusion.
Non-inference service costs remain outside governed inference, and opaque model
tunnels remain denied.
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

## Resident capacity and retained accounting history

**Implementation under validation; not yet installed acceptance.** The resident
ledger remains bounded to 128 Task identities, 256 Pod sessions, 256 detailed
attempts, a 64-attempt replay window per Pod, and 512 KiB. An effective
authorization snapshot remains capped at 64 KiB. These bounds protect the active
working set; they no longer impose a lifetime count of completed Team runs.

The broker moves closed sessions and eligible inactive leaf authorities into
immutable, account-UID-bound ConfigMaps in the accounting namespace. Each closed
session retains its replay fence and detailed attempts. Nodes retain their exact
authority and meters. Active authorities, open sessions, nonterminal attempts,
root Task/Sandbox authorities and token-top-up target nodes are not evicted.
An active working set that cannot fit still fails closed; this is a storage
condition, not spending exhaustion.

A content-addressed tree locates historical identities without loading the full
history. Its root and summary counters are committed with the resident ledger in
the existing account UID/resourceVersion CAS, after all referenced pages are
durable. Prepared pages alone grant no authority. Conflicts retry against the
current root; missing, changed, mutable or foreign-account pages fail closed.
A contextual operation restores the requested identity and its ancestors before
applying the existing authority, settlement and replay rules. Runtime callers
must authenticate their live account binding before restoration can write state.
Restoration is not new registration: closed Pod identities remain closed and
late usage is settled once, including after archival.

Existing sealed accounts migrate lazily during identity-aware transactions or
when a mutation needs resident capacity. Plain recovery transitions retain their
exact no-op and revocation semantics instead of triggering incidental archival.
Enrollment restores and protects the verified parent as well as the requested
Task, including when a new dormant child has no historical record yet. Migration
preserves the account UID, settled charges, uncertain liabilities and unknown-usage counts.
No funding receipt, usage counter or consumed execution identity is reset. Finite
production spending policy and explicitly unbounded beta allowances are unchanged.

Physical storage remains finite. Back up the account and its history ConfigMaps
together; restoring only the resident ledger is insufficient. Pages use label
`app.kubernetes.io/component=kars-budget-history` and annotation
`kars.azure.com/account-uid`; these are discovery metadata, not deletion criteria.
Copy-on-write and failed CAS attempts can retain unreachable pages. There is no
automatic garbage collection in this implementation: do not delete pages based
only on age, a single root snapshot, or account inactivity. Operators must monitor
Kubernetes/etcd storage and include these immutable records in retention planning.
The repair removes the fixed lifetime identity ceiling, not physical storage
limits, and never discards accounting evidence to make room.

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
