# Bridge governed credential adapter

Bridge source is being integrated into **Azure/kars:kars-bridge** as an optional
add-on. Source publication is not release qualification; images are not published
by these acceptance workflows. Existing audit gates and pending review evidence
remain required.

The BFF consumes `KarsCredentialGrant/workspace`; it cannot create or expand
that operator grant. Operators enroll the actual BFF ServiceAccount UID and
existing purpose-specific store UIDs using the core CLI. Bootstrap missing
stores as explicitly selected empty Opaque objects before enrollment, never by
adopting a racing existing object.

Enabled writers also require the core's reviewed private-consumption activation.
A direct grant CREATE with writers but without that activation is intentionally
denied. Use the same-source core CLI's `credentials grant preview` and
`credentials grant apply` workflow, including an explicit `--private-root` and
the actual `--private-controller-profile`; do not manufacture activation
metadata or patch a Ready condition to bypass enrollment.

Activation can retire approved private consumers and replace the root controller
Pod. Shared-controller and multi-workspace continuity remain under qualification:
this draft is not authorization to migrate an existing installation. The native
lane uses the locked public CLI, verifies the reviewed workspace/writer UIDs and
key scope, stores its metadata-only review privately, and waits for actual
controller readiness. Local orchestration fixtures are not native authority
evidence.

The native continuity case updates the sole active grant through the real
operator CLI, adds another workspace, and then updates that grant. It verifies
unchanged existing grant identities/specifications and private scope
receipts/epochs, actual writer capability reviews, and continued denial of
broad Secret listing. Ready conditions still come only from the controller;
fixture state changes are not substituted for these live checks.

The observer native case likewise adds its exact Sandbox UID through the
operator preview/apply path with an explicit runtime Deployment review. A raw
patch to `observationTargets` is not private-scope qualification. If existing
private consumers require separate retirement or recovery, that refusal remains
visible and the observer lane stays unqualified; the fixture does not fabricate
a qualified epoch or bypass the required operator lifecycle.

Set `core.namespace` independently from the chart's `namespace`. BFF/web
default workspace and provider operations use the configured core namespace;
the optional Teams Secret remains in the dedicated Bridge integration namespace.

The credential form now requires a target kind and workspace, and accepts an
explicit reviewed target UID. It stores a governed source without precreating
an agent namespace. Source key changes are UID/resourceVersion-fenced. Existing
legacy collections require metadata-only operator review before migration;
unsupported keys and ambiguous ownership are not silently dropped.
Agent key deletion records persistent metadata-only removal intent under the
same UID/resourceVersion fence as the value update. A new source or pending
legacy import therefore cannot reconnect a removed channel. Core applies those
tombstones after import; explicitly setting the key again removes its tombstone.

Workspace and Team channel views use observed key-name metadata and surface
permission/identity errors rather than reporting empty/disabled configuration.
Microsoft Teams configuration remains separate from agent channels; no tenant
secret is projected into an agent. The gateway stays at zero replicas until
its enrolled configuration is complete.

Provider, Foundry, GitHub App and dynamic provider updates use enrolled store
UIDs. Disconnect clears the owned key collection without deleting the enrolled
store. Core applies typed controller settings and Teams rollout/scale changes;
the BFF no longer patches Deployments. Learned-domain observations use only the
new declared read-only observation capability and its separate observer token.
There is no legacy admin/control-token or unauthenticated fallback. The BFF
verifies the target/namespace/Secret UID, recipient identity and Pod ownership
chain, pins the controller-issued TLS CA and UID hostname on port 9447, and
uses scope discovery followed by a scope-fenced read.

GitHub launch bindings additionally require an operator-reviewed connection
ConfigMap UID, App-store UID/App ID, owner subject, installation and repository/
write subset. The BFF attaches `githubBinding`; core alone issues the private
runtime App projection. An absent review is an explicit error, not a downgrade
to raw `GITHUB_TOKEN`. Existing bare-Sandbox credentials remain distinct from
repository-enforced keyless mode. Newly created keyless consumers are inactive
until an explicit empty or populated governed agent source is bound.

Tasks and Teams are created inactive, bound to their actual CREATE UID and
source selections, then activated. Team deletion is UID/RV-fenced and leaves
core ownership/retention in charge of sources and evidence instead of sweeping
other workspaces' same-name records.

The public workspace channel-write entrypoint preflights the complete consumer
plan **before creating or patching the source Secret**, not only before changing
consumer references. A later known consumer conflict therefore leaves source
values/UIDs and all consumers unchanged and sends no mutating API request.
New-source plans carry only a canonical name until the actual CREATE response
supplies its UID; no empty UID is synthesized and no missing/recreated source
is adopted. They do not require or attempt a Secret GET before CREATE:
uninventoried source names are intentionally unreadable to the BFF. After
complete consumer preflight, CREATE is exclusive; an existing object produces
409 and is preserved, without GET/adoption/PATCH fallback. A 403 is never
interpreted as absence and reader permissions are not widened. The BFF waits
for core enrollment/metadata acknowledgement before reading the new source.
After the source write and controller metadata acknowledgement,
current source UID/RV and all captured consumer UID/RV fences are rechecked
before binding. Concurrent changes after preflight remain explicit CAS errors;
this is not a Kubernetes multi-object transaction or rollback promise.

`POST /api/operator/credentials` preserves a typed Kubernetes 409 as HTTP 409
with error code `conflict`, rather than reporting a generic 502. This includes
a status-only resourceVersion conflict between the final target read and PATCH:
the captured version is not silently refreshed. The handler does not retry a
partly written transaction, adopt a colliding/replaced source, or delete a source
after an ambiguous CREATE/PATCH acknowledgement. A source may already be stored;
refresh the operator-reviewed target and core source metadata before explicitly
resubmitting through the existing form. Preflight conflicts still perform zero
mutations, and the existing retention/cleanup contract is unchanged. Other API
failures and transport/serialization failures remain errors, not conflicts or
successes. Responses and logs omit Kubernetes messages and credential values.

## Explicit metadata review and continuation

The credential form on **Console -> Agent capabilities** uses
`POST /api/operator/credentials/review` before storing. This operator-only read
returns metadata and an authenticated review ticket, never Secret values. It
captures target UID/generation/resourceVersion and a complete spec/identity
fingerprint, grant UID/generation/version/policy and legacy-review fingerprints,
workspace UID, key scope, and the canonical source's acknowledged UID/version
and metadata fingerprint. An uninventoried name remains CREATE-only: review
does not attempt a Secret GET to discover or adopt it.

The existing credential POST accepts the ticket in `review`. It rechecks the
captured metadata before any source mutation and retains the reviewed target RV
at binding; it never automatically retries or rebases that PATCH. Legacy
callers without a ticket keep their existing contract and cannot obtain a
continuation receipt.

Core enrollment may attach ownership metadata after exclusive source CREATE.
The adapter accepts that version change only when controller-owned grant status
provides `ownershipFromResourceVersion` matching its acknowledged write, with
the same source UID and target and the exact current successor version. It
rechecks live metadata and the reviewed authority before retaining the successor
acknowledgement. No source value is read or rewritten, and the target's reviewed
RV is still enforced immediately before binding. Missing, stale or unrelated
transition evidence remains a conflict; there is no compatibility fallback
that guesses a version change was harmless.

A reviewed 409 can carry one of two signed outcomes:

- `no-write-attempted`: no source mutation was attempted. Explicit re-review
  may advance status-only target/grant versions, but the entire source review
  must remain identical.
- `source-stored`: this handler obtained the real source write's UID/version.
  Re-review waits for core acknowledgement, reads only source metadata, and
  rejects any source UID/version/metadata/key-scope change. A confirmed resume
  binds this source without another source CREATE/PATCH or value readback.

Both outcomes require the same operator, target UID/generation/full intent,
grant policy/identity, workspace, key, and submitted value. Tickets use a
domain-separated signing key derived from the existing principal secret;
signed operator sessions are required, with no alternate credential fallback.
Only a keyed signature commitment to the value is carried, not the value or an
unkeyed value hash. Tickets expire after five minutes, never extend that expiry
on refresh, and authorize at most three separately confirmed submissions.
Each reviewed read/write operation has a 30-second bound.

The browser does not resubmit in a catch block. It displays the outcome, requires
an explicit **Refresh and review current metadata**, displays the new versions
and unchanged intent fingerprints, and requires another confirmation and
re-entry of the same value. Changes invalidate the review. CREATE collisions,
403/422, missing receipts, tampering, expiry, lost acknowledgements and transport
failures do not authorize recovery. A completed binding and the retained source
are verified before reporting success; the operation is still not an atomic
multi-object Kubernetes transaction, and no deletion/rollback is invented.

The native `create_delivery` case follows the same review -> submit -> explicit
re-review -> resubmit protocol rather than requiring first-attempt 200 or
retrying arbitrary 409/502 responses. It emits only fixed stage/status/boolean
facts. Its original real projection, runtime-value, ownership and subsequent
revocation assertions remain mandatory. Local orchestration tests are not
native delivery evidence:

```bash
node --experimental-strip-types --test web/tests/credential-review.test.mjs
PYTHONDONTWRITEBYTECODE=1 PYTHONPATH=tests/native-credentials python3 -m unittest discover -s tests/native-credentials -p test_credential_review.py
cargo test --locked --manifest-path bff/Cargo.toml --lib credential
```

Supported v1 `credentialsRef` Sandboxes and existing unbounded
standalone Sandboxes are not implicitly migrated; fresh or already opted-in v2
Sandboxes remain eligible. Foreign/mixed internal references and conflicting
grant/source identities fail explicitly before conversion. Team-owned Tasks
follow core's non-destructive rebind protocol. Widening an active standalone
Task's key authority requires explicit governed rebinding, not a workspace-save
side effect. Each applied plan entry remains UID/resourceVersion fenced.

Templates supply the new parent-map defaults themselves: old reused release
values keep observations off and core workspace `kars-system`, even if `core` or
`networkPolicy.observations` is absent. Regression tests replace chart defaults
with the exact BASE105 values and exercise real Helm `lookup` against a local
read-only API fixture. The private Next 16.3.3 manifests and verified lock remain
unchanged by these credential repairs.

Both Helm and standalone RBAC remove broad Secret and Deployment mutation
permissions. Credential access comes from core-generated, purpose-bound Roles.
Bridge uninstall removes only add-on resources, not the grants, core workloads,
legacy stores or operator configuration. Writer retirement is separate from
source delivery: core publishes `WriterReady=False`, revokes owned read Roles,
and retains valid source/GitHub consumers. Enrolled ServiceAccount/namespace
name holds prevent reuse until revocation is complete. An operator may review
`spec.writers: []` without disabling the grant. The core controller must remain
running during add-on uninstall; do not force-remove its guards. Continuity and
revocation under uninstall/reinstall still need actual Kubernetes qualification,
not just retained-object Helm tests.

For an already egress-isolated BFF, explicitly configure:

```yaml
networkPolicy:
  observations:
    enabled: true
    existingIsolationConfirmed: true
    targetNamespaces: [kars-reviewed-agent]
```

Leave this off for an unrestricted BFF; enabling an Egress policy there would
introduce isolation. Preserve the existing Kubernetes/provider/OIDC/GitHub
baseline policy. The additive rule opens only TCP 9447 to reviewed runtime
namespaces and Sandbox Pods. It is installed in the configured **Bridge**
namespace, not the core workspace. Core checks actual sender Pods and selected
NetworkPolicies before issuance and reports a clear unavailable state when the
approved path is absent.

The candidate needs coordinated Rust and actual Kubernetes permission/lifecycle
qualification before it is ready for use. Source/configuration checks have not
been waived.

The approved active-SRE verifier now runs in the existing core controller.
Enable core chart `observationPrivacyRpc.enabled=true`; its private TLS port is
9448, separate from metrics. Every router observation obtains a fresh,
target/version/identity/recipient/scope/nonce-bound proof from that controller,
which validates the current canonical observer Secret and executes the full
privacy helper. No raw Secret inventory permissions, broad private Kubernetes
credential, additional sidecar, full control token or App key is given to Bridge.

The BFF requires an unexpired observation binding with the declared verifier
capability and checks the TLS scope response's `privacy_verifier` marker. Old
controllers/routers are explicitly unavailable; there is no admin or unauthenticated
fallback. Core uses revision-selected controller Pods, pinned CA/UID hostnames
and per-target network policies. The BFF still uses only its existing private
9447 observation path, never the controller RPC as a general API.

Core RPC/TLS/API regressions pass locally. The new BFF compatibility assertions
remain subject to the separate private Rust plan, and real Kind/CNI plus private
adapter TLS/API lifecycle acceptance are still required. Native Kubernetes GET
is name-authorized RBAC; no complete raw-GET UID-bound claim is made.

The native observer enablement failure records `observationReadiness` alongside
`metadataAtFailure` in `native.json`. Collection is read-only and bounded to
the core controller and observation-target routers. Only the fixed core
readiness stage vocabulary, numeric HTTP status (`0` means none recorded),
timeout/connect booleans, bounded `observer_target_client` configuration/progress
booleans, and collection-availability booleans survive parsing.
Raw logs, span fields, exception text, tokens, identities, and response bodies
are never written to this evidence. Collection cannot qualify any assertion.
TLS negatives, 9447/9448 paths, CNI peer denial, and credential rotation remain
required unchanged.

An operator template-drift refusal also records `enrollmentTemplateDrift`.
It compares the fixture's pre-preview runtime, controller and BFF Deployment
snapshots with the existing CLI review and current objects, using the shipped
CLI's `templateDigest` (including its epoch normalization). Only fixed actor
labels and comparison booleans are emitted; templates, values and hashes stay
private. `baselineMatchesReview` must be true before attributing differences
to changes after review. Each current Deployment is UID/RV-rechecked; missing,
ambiguous or changing evidence is explicitly unavailable. This failure-only
diagnostic neither retries enrollment, refreshes approval nor qualifies a test.

`observer_target_client` optionally carries an atomic group of five booleans:
`transport_debug_observable`, `transport_trace_observable`,
`tcp_connect_started`, `tcp_connected`, and `http_handshake_complete`.
All five must be present and boolean if any is present; a partial or mistyped
group discards the record. Older cores can omit the entire group: the collector
preserves that absence as unknown, never synthesizing `false`. Extra upstream
fields are discarded. Connection observations are positive-only, request-poll
local facts, not wire/socket correlation: pooling can bypass events and spawned
futures need not inherit the subscriber. Neither `false` nor compiled-level
availability proves/excludes connectivity, TLS completion, or CNI denial.

Core normalizes the default HTTPS port for `endpoint_environment_matches`:
locked kube-client 3.1.0 omits explicit `:443` in its in-cluster URI. Older
diagnostics therefore reported a false mismatch even for the matching API
endpoint. This repairs diagnostic interpretation only. The native `Prepared`
observer deadline's root cause remains unknown; no timeout fix or qualification
is claimed. See the core [observer diagnostic contract](../../docs/how-to/governed-credential-grants.md)
for HTTP-setup and spawned-work limitations.

Labels and ServiceAccount names are only prefilters, never diagnostic
provenance. The collector anchors the canonical controller Deployment and the
specific native observation target's published namespace/Deployment UIDs,
checks actual ReplicaSet and Pod controller-owner UID chains, and requires
the current observer version on the runtime template and Pod. It rechecks
source, namespace, Deployment, ReplicaSet, and Pod UID/resourceVersion after
reading bounded logs; changes discard the sample. Missing identities,
unlinked Pods, read failures, and empty/unrecognized records remain explicitly
`available: false`. The aggregate is available only when both expected
components supply verified records; this still conveys no readiness verdict.

This collector needs the reviewed core readiness-diagnostic candidate. Update
all existing core pins together only after core source review; an older core
can yield empty stage records, not a successful readiness verdict. The source
review should examine ordinary observer metadata API egress separately from
SRE-only egress and the disposable Cilium fixture's BFF-only API entity rule.
Neither network policy nor a publication/workflow pin is changed here.

Run the dependency-free projection tests with:
`PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests/native-credentials -p 'test_observation_diagnostics.py'`.

### Failure-only observer API reachability experiment

Only after the original observer-capability deadline has failed and that failure
has been saved, the disposable private `bridge-native` job may collect
`observerApiReachability`. It snapshots the installed runtime NetworkPolicies,
exact `default/kubernetes` Service/Endpoints identities and address/port pairs,
and the current UID-proven observer Pod set. Namespace ownership, Deployment and
ReplicaSet lineage, observer version, ServiceAccount UID, configured router UID
1001, container identity and restart count are checked. Unknown selector consumers
or changed provenance prevent or abort the experiment.

The diagnostic's final Sandbox read permits status-only Prepared/Ready progress
and its resourceVersion/status-managedFields bookkeeping. Full spec, generation,
metadata (including owners, labels and annotations), and all observer binding
fields except phase/reason remain frozen, both within and across snapshots.
The final status is retained so an in-flight Ready transition is not discarded.
Other snapshot anchors and the shared fresh RPC/audit provenance readers retain
their strict UID/resourceVersion checks.

Before policy creation, the redacted, minified kubeconfig must select the unique
`kind-bridge-native` context/cluster/user binding and an HTTPS literal-loopback
origin matching both the captured setup cluster and the actual admin client's
host/port. Proxies, TLS overrides, non-loopback endpoints and changed origins are
refused. Raw kubeconfig and credentials are never emitted; cleanup remains bound
to the same validated admin origin.

The current experiment does **not** repeat the IP-only exception. Cilium 1.18.5
[excludes in-cluster node identities from CIDR selectors by default](https://github.com/cilium/cilium/blob/v1.18.5/Documentation/security/policy/language.rst#L443-L458);
`policyCIDRMatchMode: nodes` is an agent/Helm setting, not a per-policy option.
The preceding exact-IP experiment's lack of progress therefore does not
exonerate CNI egress.

The reader captures allowlisted fields from the installed `cilium-config` and
the actual UID/DaemonSet-linked Cilium agent on the observer's node. Fixed
JSONPath projections of read-only `cilium-dbg config --read-only` and
`endpoint get` return only CIDR mode/policy enablement, endpoint/security
identities, workload names, and desired/realized policy revisions. The
CiliumEndpoint CR must belong to the exact Pod UID and match its Pod/node
addresses. Agent recreation, restart, endpoint rebinding or configuration
changes abort the comparison. Contradictory installed/effective configuration
prevents policy creation rather than guessing how a non-default cluster works.
Only baseline namespace CNP identities/spec digests are retained, not arbitrary
policy descriptions or status messages. Baseline KNPs and CNPs remain unchanged;
CNP status bookkeeping may advance without changing identity or authority.

If the baseline cannot complete, `baselineStoppingStage` and `baselineSnapshot`
identify the exact read, identity check, CLI execution/framing, field parse or
pin comparison that stopped it. Only fixed stage names, allowlisted shape
classes, actual HTTP/exit status, bounded counts and boolean comparisons are
retained. Already validated network/Pod facts survive a later Cilium failure,
but are explicitly historical: `networkValidated: true` does not imply
`complete: true` or authorize an intervention. Unknown exception text, API
bodies, CLI stderr and raw configuration/projection values are not retained.

The mode-format witness distinguishes JSON `null`/`[]`, empty output,
`<nil>`/`<no value>`, and malformed values without converting unknown/missing
output into a default. A credential-free offline control exercises the existing
kubectl JSONPath engine when available; it is not execution of the pinned
Cilium CLI or proof of the failed native run's cause. No policy is created
unless the complete, unchanged provenance/configuration fences succeed.

The tagged implementation distinguishes map presence from rendering:
[`evalField`](https://github.com/cilium/cilium/blob/v1.18.5/vendor/k8s.io/client-go/util/jsonpath/jsonpath.go#L392-L430)
retains a valid map entry even when its value is nil, but produces no result
for a missing key under `AllowMissingKeys(true)`. For the declared config-map
value types, [`PrintResults` and `evalToText`](https://github.com/cilium/cilium/blob/v1.18.5/vendor/k8s.io/client-go/util/jsonpath/jsonpath.go#L145-L182)
render a nil interface as `null` (the explicit scalar-printer branch at lines
570–578) and an empty slice as `[]`; this is not a claim that the outer Cilium
printer JSON-marshals every scalar. The offline control also omits each required
field with missing-key tolerance enabled: delimiters remain, but the missing
field's token is empty and is rejected. Bare `<nil>`, empty tokens and unknown
values remain unaccepted; `requiredTokensPresent` is a format fact, not a
substitute for the fixed-field schema or the agent/config identity fences.

One CREATE-only, Deployment-owned **namespace-scoped CiliumNetworkPolicy**
permits only `toEntities: [kube-apiserver]` and the observed TCP HTTPS ports
443/6443, as described by the
[tagged entity semantics](https://github.com/cilium/cilium/blob/v1.18.5/Documentation/security/policy/language.rst#L247-L288).
It uses the actual Sandbox and Pod-template-hash labels, never an empty/global
selector, `host`, `remote-node`, `cluster`, `world`, `toServices`, or a global
Cilium configuration change. An observer without existing matching Kubernetes
egress isolation gets no new policy. A 60-second observation deadline bounds
the loop; API/agent reads and cleanup retain their own transport bounds.
The existing metadata-audit projector observes only the same Pod's first
Sandbox GET. Any observed API response, including 403, is diagnostic transport
progress; observer readiness is not required. The policy is removed with
its captured UID and current resourceVersion, and absence is verified. Replaced
namespaces or policy objects are not deleted; unverified cleanup is explicit.

This is correlation evidence, not a production fix or CNI acceptance. Policy
revision observations alone are not proof that a particular rule was realized,
and Kubernetes object removal is not a claim about datapath convergence. No RBAC, TLS,
iptables, admission or existing policy is changed. Raw bodies, environment
values, headers, query URLs and arbitrary log data are not retained. The original
case remains failed and its dependent TLS/CNI/rotation cases remain blocked,
even if the same Pod advances during this probe. The original acceptance
deadline and core pin are unchanged.

Run the dependency-free provenance, fencing and cleanup tests with:
`PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests/native-credentials -p 'test_*.py'`.

### Actor-scoped native API outcomes

Only the two failing native cases collect `actorApiOutcomes`: credential
delivery selects the BFF writer and its captured Sandbox CREATE UID; observer
enablement selects the actual observer router and captured target UID. Both
reuse Deployment→ReplicaSet→Pod ownership checks and additionally bind the
current ServiceAccount UID. All source UID/resourceVersions are checked again
after collection; a change discards the evidence.

The reader consumes at most the last 4 MiB of the already configured,
Metadata-only disposable audit file. It does not change audit policy or save
the raw tail. It retains only case-window `ResponseComplete` events whose
ServiceAccount name/UID **and bound Pod UID** match the resolved actor.
Impersonated requests, other Pods sharing the account, missing Pod binding,
foreign targets, subresources, and non-allowlisted GVRs/verbs are excluded.
For the router, only its first Sandbox GET is selected. For the BFF, only the
captured delivery workspace's grant/namespace reads, target GET/PATCH, and
canonical source CREATE/GET/PATCH are selected. A CREATE denied before its
name is decoded may yield an unnamed Secret CREATE outcome in that workspace;
that is not evidence of source creation, source UID, or persisted values.

Output is limited to actor/category/availability, fixed GVR and verb, actual
numeric HTTP status, status category, and occurrence count. Request/response
bodies, headers, tokens, raw URLs, actor/resource names and UIDs, and diagnostic
messages are not exported. `source-unavailable`, `audit-unavailable`, and
`no-matching-evidence` remain distinct from retained successful or denied
responses. A bounded tail, missing authentication identity, or missing bound
Pod claim can hide events; no-match is **not** proof of network denial or of
HTTP success. These facts cannot qualify a test or replace TLS/CNI negatives.

### Bounded reachability conclusion at core `cb38649`

The native artifact identifies a current observer Pod/Deployment with Ready
containers, and repeated router failure/cancellation at the first Sandbox
metadata GET. It records no HTTP status for that GET. Health probes do not
exercise this metadata path.

* `controller/src/reconciler/mod.rs` configures the router as UID 1001 with the
  runtime `sandbox` ServiceAccount. The guard in `reconciler/pod_spec.rs` applies
  its redirect/drop rules only to UID 1000. Consequently, the generated guard
  does not redirect the router's Kubernetes client through agent port 8444.
  Actual installed rule state and process identity were not retained.
* `inference-router/src/service_observation.rs` builds the Kubernetes client
  using `kube::Config::incluster()`. This metadata request uses Kubernetes
  in-cluster authentication, not the separate observation bearer used on
  private 9447/9448. Reaching the GET stage proves local observer binding checks
  and client construction completed, not successful API TLS/token acceptance.
* The ordinary runtime's pod-level NetworkPolicy permits external HTTPS while
  excluding private ranges. Exact API Service/endpoint egress is added only
  when `sre_projection.is_some()`. Observation-specific additions in
  `credential_grants/observer_metadata.rs` permit 9447/9448, not API HTTPS.
  The native fixture uses Cilium 1.18.5 with normal kube-proxy; its explicit
  `kube-apiserver` entity rule selects the **BFF**, not the runtime.
* Source therefore does not establish an ordinary observer API path. The
  retained artifact nevertheless contains neither effective CNI policy/flow
  verdicts nor installed UID-rule state, and cannot prove which layer caused
  the GET failure/cancellation. Actor-bound API outcomes can establish that a
  specific request reached and completed at the API server; their absence
  cannot isolate networking from pre-HTTP credential/client failure.

No egress, TLS, audience, readiness, or authorization exception is introduced.
The separate BFF credential POST failure occurs before observation enablement
installs its BFF Cilium rule, so that later rule cannot explain the earlier
502. The operator handler maps upstream errors to 502; the new scoped outcomes
are intended to distinguish actual API failures without exporting error bodies.
