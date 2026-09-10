# Security audit — registered SRE credential authority

Status: **source audit approved under explicit maintainer delegation**.
Integration remains conditional on every required exact-head technical check.
This approval does not authorize a customer deployment or a change to `main`.

## Current approval and review scope (2026-09-10)

The maintainer approved the author audit at
`203e2322ad22512f0889e1f512ed36ac278b5a42` and explicitly authorized Copilot to
complete subsequent publication sign-offs after additional focused reviews.
That instruction is recorded in
[maintainer authorization](https://github.com/Azure/kars/pull/551#issuecomment-5615522306).
The second attestation below is **delegated AI review, not a claim that a
second human reviewed or signed this change**. This is the disclosed
maintainer-authorized exception for this audit, not a silent change to the
repository's normal two-person process.

Focused independent-context reviews covered:

- Controller and Helm enrollment, private authority, UID/RV ownership,
  admission and retirement. The identified ReplicationController template
  omission is repaired in `a02de2b9`; final security review of the assembled
  repair reported no remaining vulnerabilities in its assigned delta.
- Private proxy TLS, bearer authentication, credential renewal, live
  authorization, routes and response redaction at `203e2322`. No
  high-confidence exploitable issue was identified in that assigned scope.
- Installation/staging/removal compatibility. The three identified defects
  are repaired in `141657f9`: Helm 3/4 staging readiness, the exact historical
  action-CRD repair before policies, and Azure resource-group teardown bound
  to its complete, subscription-pinned target inventory. Final focused review
  found no remaining significant issue after the native fixture correction.

The reviewed source is assembled at
`3c87deff2bdafac55c1f85db9984f480423e4f17`, with test-only correction
`b37c91e93b68216876827c2a1e58413b7a112e39`. Kubernetes requires an RC Pod
template even at zero replicas; the native case now requires the precise
422/Invalid/spec.template/FieldValueRequired rejection instead of accepting
arbitrary errors. A final test-only change reads and checks temporary
kubeconfig permissions through the same open file descriptor, addressing a
CodeQL filesystem-race warning without suppressing the query or changing
production behavior.

All 104 Python harness tests and Helm lint passed on the assembled source.
The five affected CLI suites passed 111 cases with type checking and targeted
lint using the existing compatible local cache (Vitest 4.1.10); this is not a
claim of exact-lockfile local installation. Hosted locked CLI qualification
passed on `3c87deff`. Final hosted native/schema, full Kind, CodeQL, and all
other required checks must still pass on the final landing head. The earlier
161-case full Kind success at `203e2322` is prerequisite evidence, not a
substitute for the changed candidate's qualification.

These were AI-performed static review rounds; the reviewers did not themselves
rerun the native tests. No universal security, CNI-enforcement or complete
Bridge-readiness claim is made. Any subsequently identified blocking finding
reopens this approval; no failed check, timeout or policy is waived.

The full native run at `8618e9f45d71f3223f35788c6233ebcf56ca5587`
([job 102880906111](https://github.com/Azure/kars/actions/runs/34478247382/job/102880906111))
completed legacy and fresh SRE migration, the RC CREATE/UPDATE denial matrix,
diagnostic compatibility and retirement. Its final result was 164 passed and
one failed: an unrelated smoke assertion read the NetworkPolicy immediately
after namespace creation, before asynchronous reconciliation reached that
resource. The same run subsequently observed its required ingress policy.
Both initial NetworkPolicy/ServiceAccount smoke checks now use the existing
30-second exact-resource wait helper; missing resources still fail, and policy
content checks remain unchanged. This fixture correction requires a new
exact-head run; the prior failed job is not reclassified.

Signed-off-by: pallakatos (maintainer authorization recorded above) <lakatos.toth.pal@gmail.com>
Signed-off-by: GitHub Copilot (delegated AI audit, not an independent human) <223556219+Copilot@users.noreply.github.com>

The sections below preserve historical qualification and repair evidence.
Their earlier pending/not-a-sign-off statements describe those checkpoints,
not the current scoped approval above.

## Scope and trust root

This prerequisite introduces cluster-scoped `KarsSRERegistration/canonical`.
Only explicitly delegated registrars can author it; the controller can read,
use, and reconcile status. Namespace occupancy, SRE labels, account names, and
Helm-looking metadata are not privilege delegation.

## ReplicationController template boundary repair

A focused source review found that the private workload-template policy omitted
core/v1 ReplicationControllers. The candidate adds their CREATE/UPDATE operations
to the existing policy without broadening its authority exemptions. The
ReplicaSet-controller exception remains specific to `apps/replicasets`.
Selector-only ReplicationControllers without a Pod template are excluded from
template inspection; any supplied template remains checked.

Regression coverage extends the image-free admission cases and the actual
enrolled-SRE lane. The latter checks a tenant with namespaced ReplicationController
creation/update and Pod-log access, but no cluster-wide Pod-create or Secret-get
authority. Ordinary requests must succeed; private Secret volumes, projected
Secrets, environment references, init-container references and the private
ServiceAccount must receive the intended policy denial on CREATE and UPDATE.
Only a zero-replica ordinary fixture is stored. Private variants are dry runs;
all templates are nonexecuting, and no credential-reading or exfiltration
payload is used. Cleanup retains UID/resourceVersion fences.

The 99 Python harness tests and Helm lint pass locally. Native compilation and
admission results plus focused security re-review remain required before this
finding can be considered closed. This record is not a sign-off.

## Kubernetes 1.31 controller-manager compatibility

Real Kubernetes v1.31.0 evidence showed that the controller Pod was not rejected:
Pod dry-run and Deployment creation returned 201, but kube-controller-manager
repeatedly exited with a nil-pointer panic while the VAP status controller
converted the SRE action CRD's OpenAPI schema. The trigger was the boolean
`additionalProperties: true` representation of `spec.action.params`.

The repair keeps `type: object` and arbitrary nested JSON values, using
`x-kubernetes-preserve-unknown-fields: true` for that field only. Rust schema
generation and the Helm CRD use the same representation. No admission policy,
approval requirement, namespace boundary or other field constraint is removed.

The hosted A/B proof at
https://github.com/Azure/kars/actions/runs/34262068112/job/102182569705
keeps the original failing step fatal. The separate schema-only candidate
recovered controller-manager, observed all 22 policies, created a real
Deployment/ReplicaSet/Pod, and passed nine ordinary/private admission and
Pending-action JSON round-trip cases. Workload scheduling, image execution and
application readiness were deliberately not claimed by that admission probe.

The successful shipped-schema probe now runs those admission cases too.
Local Python discovery covers all 35 diagnostic/harness cases; its imports match
the actual full-harness discovery command. The new Rust schema/wire regressions,
existing Helm/Rust drift test and full fatal SRE migration remain required before
readiness. A CI run, not the unbuilt local repair, supplies that next evidence.

## Subtractive binding authorization: real API evidence and local repair

The image-free hosted Kind experiment at
https://github.com/Azure/kars/actions/runs/34280933428/job/102245425114
ran against immutable head `c1d14fad4bbaeeda6832d278943ba9bba4e9130e`.
Artifact `sre-crd-schema-34280933428` contains
`bootstrap-binding-retirement.json`; no bearer tokens or response bodies are
included. SelfSubjectReview verified the short-lived controller ServiceAccount
bearer's actual UID, with no admin client certificate or impersonation fallback.

The controller already had ordinary ClusterRoleBinding PATCH and canonical
registrar `use`, but lacked named `bind` on the historical `kars-sre-reader`.
The exact reviewed UID/resourceVersion-fenced subtractive PATCH returned an
explicit Kubernetes RBAC permissions-not-held **403**. Adding only a disposable
`bind` grant for that one ClusterRole changed the identical dry-run to **200**;
removing the test grant restored **403**. A separately reviewed custom role
remained **403** throughout. Wrong UID produced the specific immutable-UID
**422** validation error, and stale resourceVersion produced **409 Conflict**.
Every persistent binding, unrelated subject, role and consumer remained
unchanged; the temporary permission was removed. All fourteen SRE policies
remained observed, warning-free and Deny-bound, and all nine existing
ordinary/private/Pending-only admission cases passed. No workload image was
executed and no full migration readiness was claimed.

The local production candidate adds only `kars-sre-reader` to the controller's
existing three-name ClusterRole `bind` allowlist, yielding four exact names.
It introduces no wildcard, `escalate`, cluster-admin, default agent grant, or automatic custom
role permission. Retirement prepares immutable patches, dry-runs **all** of them
through the API server, then applies those same patches only if every preflight
succeeds. An initial permission, admission or validation failure is reported
with safe `Preflight` context before any binding retirement or consumer stop.
Custom-role failures require explicit operator remediation and fresh reviews
where needed; the controller never self-grants the missing permission.

This is not a multi-resource transaction. A later real write can fail if
permissions or versions change after preflight. Prior completed retirements can
remain, errors stop further progress, and retries recognize retired bindings
without restoring old grants. Rust HTTP fixtures now distinguish `dryRun=All`
from real PATCHes and do not mutate persistent fixture state on dry-run.
New regressions cover both binding kinds, a forbidden second preflight,
preserved consumers/custom roles, UID/RV fences, ordering, idempotence and
post-preflight failures. The updated fast API probe requires the shipped reader
bind to work before and after its now-redundant test grant; it does not describe
that new baseline as another 403/200/403 experiment.

The new Rust changes have not been compiled or executed by this task because
the shared Cargo lease belongs to another qualification run. Source review and
parent approval remain required before public production push; Rust
qualification and the complete fatal migration remain required before claiming
readiness. This section is evidence and candidate documentation, not audit
sign-off or permission to deploy.

Local checks for this candidate passed all 53 Python harness tests, all five
targeted CLI/Helm authority tests (including the exact four-name bind list and
unchanged default private-grant assertions), standalone scoped rustfmt, shell
syntax and whitespace checks. The six new Rust regression functions are in
`sre_authority::retirement_tests`; parent qualification should also run the
existing `sre_authority::tests` and `sre_authority::privacy_tests` because they
share the corrected HTTP fixture. No Cargo invocation, dependency change,
public production push or new audit signature was performed.

## Boundaries implemented

- Exact source, controller/release, and runtime namespace UIDs are checked live.
- Reviewed legacy grants retire with UID/resourceVersion preconditions after
  all exact server-side retirement dry-runs pass. Unrelated subjects/resources
  are preserved; group/unreviewed ambiguity or custom-role preflight denial
  blocks before initial migration mutations.
- Shared live authorization reviews require Secret get/list/watch denial in
  namespace and cluster scope, including protected name-restricted grants,
  before issuance and proxy forwarding. Old status booleans are insufficient.
- The Pod's Azure identity remains unchanged. A separate private Kubernetes
  identity renews its short-lived Secret-bound token without ambient fallback.
- Old Hermes clients use a real loopback TLS endpoint through their standard
  token/CA/namespace paths, with an opaque non-Kubernetes credential.
- Secret projection omits values and annotation copies, retaining key names.
  Reads/logs/metrics and Pending-only proposals use a strict route/query/media
  allowlist; exec/proxy/token/write escapes are denied.
- Admission gates protect reserved sources, identities, grants, and material.
  Retained policies prevent insecure legacy grant restoration on rollback.
- Owned potentially exposed control credentials rotate and their consumers
  restart. No governed-service feature is disabled merely because SRE exists.
- Arbitrary-name legacy SA-token Secrets for the private identity are denied
  on CREATE and old/new UPDATE transitions, without a token-controller or
  registrar exception. Metadata-only prestage scans catch populated aliases,
  annotation changes and old/current SA UIDs before identity/grant issuance.
  Unsafe discovery after Ready revokes owned authority and retires owned
  credentials; unknown Secrets, replacement SA UIDs and unrelated subjects
  are preserved. Recovery rotates the bound Secret UID and consumer.
- Privacy revision `kars.azure.com/sre-privacy/v2` and the shared Rust wire
  helper are used by controller issuance, the privacy-epoch accessor, and the
  router's current-authority check. Bound TokenRequest renewal, pinned Hermes
  HTTPS clients, and the Pod's Azure ServiceAccount identity are unchanged.

## Qualification

Tests cover registrar/UID checks, reviewed grant CAS and unrelated subjects,
legacy denial, projection/WI/pinned-image invariants, real TLS API filtering,
private token renewal, direct agent-credential rejection, and the unchanged
Python Hermes client. CLI tests cover explicit enrollment and racing CREATE.

Measured local results:

- 37 controller SRE tests passed, including live UID/claim rejection,
  real SubjectAccessReview request handling, legacy binding CAS, private
  Secret-bound TokenRequests, TLS rotation/idempotency, owned retirement,
  admission-status rejection, terminating old control-token consumers despite
  converged rollout counters, metadata-only legacy-token alias scans,
  watch-only/wildcard grant rejection, post-Ready owned revocation, replacement
  UID preservation, token-anchor recovery, and existing writer/egress regressions.
- Nine router tests passed, including real loopback TLS, direct fake-token
  rejection by the test Kubernetes API, Secret/list redaction, logs/metrics/
  Pending proposals, token renewal, the unchanged `sre_kube.py` HTTPS client,
  and the actual unchanged `sre.py` proposal builder including its labels.
  The new negative matrix covers old Ready evidence, watch-only authorization,
  arbitrary-name/current-UID aliases, and metadata API errors before forwarding.
- The earlier baseline passed 158 CLI/Helm tests. The two HIGH closures reran
  116 affected CLI/Helm tests, all passing, plus TypeScript typecheck. CLI lint
  reports zero errors and 29 existing warnings outside the new helpers.
- CLI dependencies came from an existing verified local cache (Vitest 4.1.10);
  no npm install/ci. Standard TLS additions resolved `rcgen 0.13.2` and
  `yasna 0.5.2`; no custom cryptography.

- Final strict controller/router all-targets Clippy passed with `-D warnings`.
  All 46 Rust SRE tests passed after both HIGH closures, including the original
  readiness-capacity, proposal-builder compatibility, and spawner invariants.
  Scoped rustfmt, whitespace checks, current file caps, and source headers pass.

Parent dependency hygiene restored unrelated pre-existing `windows-sys` and
`oauth2`/`base64` resolution edges. The lockfile now adds only the required
direct TLS references and the new `rcgen`/`yasna` packages. Cargo accepted that
graph with `--offline --locked`; all 35 SRE tests and combined strict Clippy
passed again. An active disk guard protected that rerun; 18.35 GiB remained.

Reproducible final Clippy command (both packages together, default features;
no `--features` or `--no-default-features` flags):

```sh
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:?Set the existing shared Cargo target first}" \
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 \
cargo clippy --offline --locked \
  -p kars-controller -p kars-inference-router --all-targets -- -D warnings
```

The final bounded Clippy run passed in 24.58 seconds with 17.91 GiB free before and
after. It used the same combined package/feature selection as the completed
Rust test command, without rerunning tests or changing feature variants:

```sh
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:?Set the existing shared Cargo target first}" \
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 \
cargo test --offline --locked \
  -p kars-controller -p kars-inference-router sre_ --quiet
```

Resource note: an earlier combined-package Cargo feature-unification run
unexpectedly expanded the shared target from 8.3 to 6.0 GiB free. Cargo was
stopped after that completed command; no shared artifacts were deleted by
this implementation task. After disk capacity recovered to 19.92 GiB, final
qualification used the existing exclusive root target and an active 8.5 GiB
process-stop threshold. The HIGH follow-up finished with 17.91 GiB free and
no Cargo processes. Parent's lockfile minimization was preserved byte-for-byte
(SHA-256 `8be5db8d96793bf2aaa73f14ab51463fb922a07011b5cfb53c341ae46a1f1ccc`);
no dependency resolution or feature-selection changes were performed.
The final CLI/chart state also preserves live SRE source UID/resourceVersion
during explicit Helm staging and retains the registration CRD on uninstall.
The existing Helm legacy-floor suite passed another ten tests, enabled/default
Helm lint passed, and no new Rust/TypeScript module exceeds 800 lines.

The tests use real local TLS plus an HTTP test Kubernetes API, not a real
Kubernetes authorizer/admission server. Parent-owned API/E2E proof must verify
CEL type checking, the full staged/legacy migration and grant issuance,
tenant denial (including workload-template and connect escapes), token/TLS
rotation, and retirement on an actual supported Kubernetes release.
Later Azure/kars#550 must call the privacy-epoch helper before operator-secret issuance
and incorporate its epoch into cached-token rotation. No public image or
release is claimed qualified by this audit; no human signatures are supplied.

The completed Kind acceptance harness now stages immutable historical fixtures
before the new admission policies, then exercises delegated registration,
legacy token-controller aliases, watch-only grants, reviewed migration ordering,
filtered TLS access through the unchanged Hermes client, and retirement/fresh
re-enrollment. Ten pure harness tests and shell syntax pass; actual cluster
execution remains pending.

Harness construction exposed a CLI mismatch: a merge update could retain an
omitted retired `legacyConsumer`. Re-enrollment now tests the registration UID
and resourceVersion and replaces the complete reviewed spec with JSON Patch.
Absent and present consumer reviews plus stale registration identity are covered
by targeted regressions. The persisted-spec assertion in the real harness
remains strict.

The shared Rust privacy helper is included in capability-audit, no-stub,
no-custom-crypto and runtime-affecting Kind path classification. Its location
outside the individual crates is not a security-gate exception.

## Full-CI follow-up

The initial full run exposed omitted registration labels, printer columns and
standard conditions. The repair adds real Ready/Progressing/Degraded condition
updates using the existing transition-time helpers, along with the schema and
display metadata; it does not exempt this CRD from conformance. The existing
17-criterion conformance suite passes against the corrected schema.

Secret-type mutations are tested separately from validating admission:
Kubernetes rejects immutable `type` changes with a specific 422 cause before
the VAP runs. Schema-valid CREATE and annotation updates still require the
intended policy-specific 403. Neither arbitrary errors nor HTTP 200 watches
count as denial.

Four Rust CodeQL alerts were investigated without suppression or product
rewriting. Their reported flows start at the readiness handler's injected Axum
State. Current evidence identifies fixed credential filenames under the
production mount and a controller-configured Kubernetes origin, rather than an
HTTP-selected location. The independent assessment and the approved
per-alert dispositions are recorded below.

### Confirm-boundary-first evidence

Independent source review of SARIF analysis `1739818791` recommends classifying
alerts 780/781 (path injection) and 782/783 (request forgery) as false positives
for the reported HTTP-input flows. Each flow starts at `get(ready)` and treats
Axum `State<Proxy>` as request data. Pinned Axum 0.8.9 instead clones the supplied
server state and ignores request parts. The sole production constructor uses
`/etc/kars/sre-api`; filenames are literals, and Kubernetes origin/namespace
come from the controller-generated private configuration.

Regression-only coverage now sends eight hostile header/query/body scenarios
through the actual readiness handler. It forces projected-file rereads, token
renewal and metadata inventory: each scenario records 24 calls to the selected
Kubernetes endpoint, while alternate HTTP/HTTPS servers receive none. The
alternate credential files are not selected. Startup-constant provenance is
also checked. The flagged production files and lockfile remain unchanged.

All 50 focused SRE tests and strict combined controller/router Clippy pass,
including the now correctly registered condition tests. This evidence assumes
trusted controller/kubelet configuration and private-volume integrity; it does
not excuse privileged configuration tampering. The user explicitly approved
false-positive dispositions for only alerts 780, 781, 782 and 783; each GitHub
alert now carries its specific evidence comment. No query or source path was
excluded, and no other alert was dismissed. This is not author/reviewer audit
sign-off, PR approval, or permission to deploy.

The first full CI execution ran the new boundary case in an isolated process
and exposed missing test-local Rustls provider initialization. Other tests had
initialized it during the earlier grouped run. The case now explicitly selects
the existing AWS-LC provider before any TLS setup and passes when executed
alone. Production proxy code and the reported input boundaries remain unchanged;
hosted Kubernetes migration qualification is still required.
