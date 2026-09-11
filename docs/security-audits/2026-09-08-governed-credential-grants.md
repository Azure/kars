# Governed credential grants — qualification record

Status: implementation candidate; **not a sign-off**. No author or independent
reviewer signatures are supplied. Existing audit gates remain required.

## Scope

Metadata-only operator grants, native Secret source authoring, UID-bound
Sandbox/Task/Team delivery, explicit workspace/Team/target precedence, legacy
preflight/import, purpose-bound operator stores, private read-only egress
observations, and a real App-store-to-router GitHub issuer. Private Bridge
adapts to the public core contract; it is not copied into
this repository.

## Enforced boundaries

- Operator-only grant authorship; no self-expansion by the Bridge ServiceAccount.
- Workspace/writer/source/target UID and source resourceVersion checks.
- No arbitrary source Secret reference, runtime namespace write by the
  credential adapter, or fallback to legacy values on revocation.
- Default ten-key v1 compatibility; explicit custom agent key grants with
  provider, identity and process-bootstrap exclusions.
- Full effective Task snapshot/digest includes credential references and key
  grants; credential delegation checks parent attenuation.
- Read-only live credential/GitHub enrollment preflight feeds ordinary Task
  Ready before execution and receipt issuance. Candidate Tasks can bootstrap
  without first requiring their own Ready status; ancestors still require it.
- Unready, still-launched governed execution is paused without deleting its
  namespace/state. Explicit unlaunch/deletion retains normal cleanup.
- Core-owned namespace/projection writes and typed provider/Teams reconciliation.
- Namespace admission limits the private adapter's remaining namespace create
  permission to its dedicated local-inference namespace.
- Enrolled-store UID/purpose admission, source-only Roles and no broad Secret
  or Deployment mutation rule in either private Bridge RBAC manifest.
- No raw credential values in the grant schema, metadata status, preview files
  or diagnostic messages.

## Current validation

### Shared-root workspace continuity candidate

Public Bridge native run 34638188545 passed the first actual operator-reviewed
enrollment and writer readiness, then rejected each additional workspace because
the persisted root-retirement binding included the first workspace's scope.
Ignoring that mismatch would not be a repair: rotating shared namespace epochs
would also invalidate prior grants.

The continuity candidate adds operator-owned root/scope qualification records
while retaining the original v1 retirement binding and recovery history. It
preserves existing qualified scope epochs and limits new enrollment to separately
verified scopes. Controller pending protection retains only independently
verified shared qualification. Existing private consumers, changed or deleted
shared evidence, and live multi-grant key rotation require explicit recovery
rather than silently resetting shared authority.

Ninety-eight provisional TypeScript cases, typecheck, scoped lint and Rust
formatting passed locally. The shared runner/parser cache differs from the
lockfile, and new Rust transport cases have not run locally. Exact hosted locked
CI, real multi-workspace native acceptance and an independent focused source
review are required; this record grants no approval or gate waiver.

### Native integration and source-gate follow-up (2026-09-11)

Core candidate `c73506bb` passed complete public technical CI. Downstream
Bridge native qualification at `99c84c0d` reached the real operator apply
path and rejected a live Pod/owner execution mismatch. Kubernetes 1.31's
`DefaultTolerationSeconds` admission adds two bounded tolerations to Pods
but not their parent templates. The comparison now recognizes only those
exact default entries where the reviewed template does not already cover
the taint/effect. Explicit/wildcard policies, changed durations, duplicates,
containers, identities and host authority remain enforced. Targeted CLI
regressions pass locally; hosted native qualification remains required.

The prototype-polluting test-fixture merge reported by CodeQL now rejects
`__proto__`, `constructor` and `prototype` recursively. The observer readiness
request already used HTTPS-only transport, a pinned CA, explicit address
resolution, no proxy and no redirects; the flagged formatted-URL construction
is replaced by a typed URL with a literal HTTPS scheme and fixed port/path.
Only the bounded observer hostname is variable. New endpoint assertions cover
host/userinfo/path/query/scheme injection. This is not a claim that the
previous probe sent plaintext, and no CodeQL alert has been dismissed or
suppressed. New Rust execution and CodeQL results are pending.

### Credential lifecycle repair (2026-09-10)

Downstream native acceptance exposed two remaining lifecycle failures at public
core `2582eade`: a disabled grant could leave its v2 Sandbox consumer running
beyond the 180-second acceptance window, and a Team credential rebind did not
resume after old consumers retired within the 240-second window. Neither
deadline has been raised and neither native failure is declared closed here.

The successful Sandbox reconciliation path previously gave only v1
`credentialsRef` a 30-second backstop. V2 `credentialBindings` and governed
`githubBinding` incorrectly used the unbound legacy five-minute interval,
without a grant-change watch. The shared refresh selector now gives all three
credential modes the existing 30-second backstop. Unbound legacy Sandboxes
remain at five minutes. Eight selection combinations cover this distinction;
reconciliation cadence is not a guaranteed consumer termination deadline.

The Task rebind path also unconditionally published PausingCredentials followed
by CredentialsPaused on every already-quiescent reconciliation. The new
controller-binary regression reproduced six unnecessary resourceVersion
increments across three passes (fixture RV 4 to 10), which can starve the
Team's independently checked UID/RV-fenced update.

Current, already-retracted PausingCredentials/CredentialsPaused status is now
stable. Every status patch, including an unchanged one, retains exact UID/RV
preconditions and explicit nulling of the envelope digest. Kubernetes preserves
resourceVersion for an unchanged patch; the regression fixture models that
behavior without bypassing its preconditions. A stale acknowledged snapshot
must conflict before any pause side effects. Every pass still
retracts the old receipt and verifies the owned hold, scale-down and all
remaining Pods, including terminating or unlabelled consumers. A late consumer
restores PausingCredentials and blocks the Team update; no quiescence,
attestation, ownership or admission requirement is removed.

The final CAS-preserving repair passed **90 controller-binary credential cases**
and strict paired all-target Clippy under the existing offline/locked
shared-target disk guard, with minimum free space **9.01 GiB**. The tests include
the stale-snapshot rejection, stable paused and
waiting states, late consumers, existing full Task/Team UID/data/receipt
continuity, and the refresh selector. Bounded independent review of the final
five Rust files found no significant issues and confirmed the initial CAS
remains before pause side effects; that reviewer did not rerun the tests.
This is local source qualification, not fresh native BFF/lifecycle, active-SRE
or CNI acceptance. Exact-head hosted qualification and genuine human signoffs
remain separate gates.

### Observation network baseline label consistency

A subsequent downstream native case reached Running Task/Sandbox state but
could not issue private observations: its existing-isolation preflight supplied
only the Sandbox-name label, while the actual baseline NetworkPolicy selects
`kars.azure.com/component=sandbox`. The generated runtime Pod already has that
label and its Workload Identity label.

The unchanged three runtime Pod labels now come from one pure helper shared by
Pod generation, verifier baseline checks and approved sender-egress evaluation.
No actual Pod label, NetworkPolicy rule, namespace selector, port, grant,
privacy proof or identity boundary is changed. Observer-created policies still
cannot establish their own baseline, and foreign selectors remain rejected.

The follow-up passes **92 controller-binary credential cases** and strict paired
all-target Clippy under the existing guard, with minimum free space **8.95 GiB**.
New cases cover the real component selector, incomplete/foreign labels,
observer-only policy exclusion and component-plus-name sender selection.
Bounded independent review of the three Rust files found no significant issues
and confirmed the generated labels and policy restrictions are unchanged;
the reviewer did not rerun the tests.
This is source-level consistency evidence, not native observation issuance,
TLS/authentication or CNI-traffic qualification. Fresh downstream acceptance
and genuine human approvals remain required.

### Existing workspace contract and observer lineage permission

The publication scope is the existing product: `/sandbox` remains an
`emptyDir`, not a newly introduced persistent workspace. The corrected native
acceptance explicitly verifies that volume mode and preserves the existing
Task/Sandbox/namespace identity, namespace-owned data, receipt, credential and
old-consumer retirement assertions. The historical filesystem-persistence
failure is not relabeled; the corrected contract passed in a fresh run.

The next native run also completed the normal UID-fenced unlaunch of the
finished Team fixture, releasing its CPU reservation without relaxing
scheduling or policy. The independent observer then scheduled, exposing the
actual controller failure: GET requests for its ReplicaSet lineage returned
403. Existing `observer_runtime.rs` already requires that read to verify the
ReplicaSet UID and Deployment owner.

The credential controller ClusterRole now adds only `get` on
`apps/replicasets`. Its binding remains solely the core `kars-controller`
ServiceAccount; no agent/operator/BFF binding or list/watch/write verb is
added. The lineage and privacy checks are unchanged. Helm lint passes and a
focused rendered-role contract is added. The locked local Vitest runner is
unavailable, so the existing hosted CLI job must supply that result; a nearby
cache with a different locked version was not silently substituted.
Fresh observer issuance/TLS/CNI acceptance remains required.

### Native admission and generated-schema repair

The composed SRE bootstrap separately demonstrated that the built-in Deployment
controller could create ordinary ReplicaSets (201) but was denied private SRE
ReplicaSets (403): it has cluster-wide ReplicaSet-create authority, not
cluster-wide Pod-create or SRE registrar authority. The missing handoff repair
is forwarded exactly from `c08465a5f7e3957b3594c5b37088c56d00290449`.
Only the `apps/replicasets` workload predicate recognizes that existing
cluster-wide capability. No RBAC grant, Pod/CronJob permission, tenant bypass
or other workload-kind exception is introduced.

The resulting entire SRE consumer-policy template is byte-identical to
`203e2322ad22512f0889e1f512ed36ac278b5a42`, whose full native Kind run passed
161 cases. The same forward includes actual private Deployment-to-ReplicaSet-
to-unscheduled-Pod UID-chain assertions and tenant-denial coverage. All 67
Python harness tests and Helm lint pass on this target. That prerequisite
evidence does not by itself prove this composed stack's readiness; fresh
exact-head native acceptance and genuine audit signatures remain required.

The full hosted run at `80cffb63` exposed additional issues: creation of
`kars-credential-source-writes` failed CEL compilation; the new grant lacked
its standard CRD label/CEL coverage; Task/Team drift checks parsed unrendered
Helm includes rather than the shipped schema.

Kubernetes 1.31 and 1.34 declare `NamespaceMetadata.UID` in the CEL type but
convert the runtime namespace to JSON with `metadata.uid`. The three affected
policies now select `dyn(namespaceObject.metadata).uid`, preserving the exact
native UID equality without a fallback. Merely changing the selector to
uppercase would leave runtime evaluation broken.

The grant now carries the standard application label and a root CEL rule
requiring its canonical `workspace` name. Generated Task/Team credential and
GitHub binding schemas match the existing bounded Helm schema. Drift checks
render templates that use includes and still compare the complete canonical
CRD; no fields or assertions are excluded.

Local qualification passed all 30 Helm drift cases, 17 CNCF criteria cases,
84 controller credential cases, strict paired all-target Clippy, and 16 CLI
credential/observer contract cases. At `f8d641f6`, native job `102629811011`
subsequently passed source-writes 201/403/201, privacy-material 201/422/201 and
privacy-Pod 201/403/201 cases, canonical/noncanonical grant cases and cleanup.
The unchanged all-policy controller Pod bootstrap also passed. These are native
expression checks using explicit impersonation of actual ServiceAccount UIDs,
not bearer authentication or complete BFF/grant/CNI acceptance.

The broader native API suite then exposed three additional type-check issues:
the enrolled-store predicate combined byte-valued and string-valued maps,
the rebind predicate compared a statically declared string with its nullable
wire value, and the cross-kind exposure policy referenced kind-specific fields.
The candidate combines only Secret key lists, preserves the exact nullable
digest comparison using `dyn`, and keeps kind-specific field access behind the
existing kind guards. Store UID/purpose/key restrictions, current paused
generation and Ready=False requirements, exposure resource/namespace selectors,
denial reasons, and Fail/Deny enforcement remain unchanged.

Seventeen CLI contract cases and Helm rendering pass for these additional
repairs. Native job `102672351688` at `9caf91ed` passed all 42 policy evidence
records, including accepted and forbidden Secret representations, nullable
paused-authority cases and exposure checks. Full Rust and CodeQL passed at
`f8d641f6`. Isolated controller compilation subsequently passed at `4b24d9c5`;
the three observer-body cases passed on equivalent source at `df4c5932`.
Complete SRE migration, benchmark performance and full BFF/CNI lifecycle remain
separate gates. No result here supplies a human audit signature.

The full-chart native BFF run then exposed an omitted-field error on primary
grant creation: Kubernetes omits empty `request.subResource`. Admission now
normalizes only that absence to the empty primary-resource name, retaining
explicit status, token and finalize handling. The native policy probe now also
installs the actual grant-authority policy before exercising primary creation,
metadata updates and status updates; it does not pre-seed around admission.
All 75 unit/harness and 17 CLI contract cases pass. The expanded native proof
and real delegated-controller lifecycle remain pending, not waived.

Separately, the owner explicitly approved false-positive disposition of only
CodeQL alert 804. Its sink is test-only local fixture path injection; production
opens the fixed mounted configuration path. The reported source is server-owned
Axum State. Evidence is recorded in PR554 comment `5607765226`; no query,
security check, audit-signature requirement or other alert was waived.

### Cross-layer repair core qualification — passed, lease released

Immutable qualified code head:
`45ccc89996707ca69aaf42da5b76b8ba2844fa01`.
The reviewed logic remains the `24646e1b` repair checkpoint. Parent-approved
mechanical corrections are isolated in `71a42f6b`, `cdb8ba78` and `45ccc899`:

- explicit `kars_task_rebind/tests.rs` and `tests/suspension.rs` module paths;
- relocation of the unchanged `hold_credential_runtime` function from an
  accidental nested block to its intended module scope;
- the missing `ListParams` import;
- equivalent test-only CAS conditional syntax and lexical MutexGuard scope
  for strict Clippy. Test assertions and production behavioral bodies are
  unchanged; no lint waiver was added.

The initial frozen check exposed the wiring errors before test execution.
After correction, all targeted tests passed; there was no test-behavior
failure to suppress or redesign during review.

| Guarded paired/default-feature/offline/locked validation | Result |
| --- | ---: |
| `check --tests` | Pass |
| `credential` | 108 (83 controller, 24 router unit, 1 router integration) |
| `kars_team_reconciler` | 32 |
| `kars_task_execution` | 16 |
| `kars_task_reconciler` | 12 |
| `privacy_rpc` | 11 |
| `observation` | 16 |
| `governed_services::continuity_tests` | 4 |
| `github` | 43 |
| Final re-run of `kars_task_reconciler::rebind` | 3 |
| Strict paired all-target Clippy, `-D warnings` | Pass |

Filters overlap. The lease is **released**; no Cargo/rustc process remained.
Minimum free space under the renewed guard was **9.06 GiB**, above the
**8.50 GiB** floor; release-time free space was **9.07 GiB**. No broad cleanup,
new target, dependency installation, private BFF Cargo, Docker, deployment,
H100/cloud operation or public push occurred.

Private `3e571ea` was untouched during this core batch. Its Rust compilation/
tests remain the parent's hosted PR31 responsibility. D033's bounded independent
review and actual admission/CNI acceptance remain required; passing core tests
does not supply a human sign-off or a UID-aware native Secret GET guarantee.

### Earlier d033 repairs — source/fast qualification before this core lease

The independent review identified seven semantic/lifecycle blockers. The
`ec1ecf54` results below **do not qualify these subsequent repairs**.
Implementation and regressions now cover:

1. **Ordered attenuation:** retained keys must keep the parent's effective
   declared final source/owner identity. Later absent-value masks are authority,
   not permission to reveal lower-priority values. Tests exercise the actual
   selection projection for both present overrides and absent masks, plus
   dropped keys, retained masks and reordering.
2. **Non-destructive Team rebinding:** the existing pending marker now drives
   Ready/digest invalidation, owned runtime holds and scale-to-zero, including
   waiting for terminating Pods. Team principal/member/taskforce credential
   drift is separated from other authority revocations. Binding changes retain
   Task/Sandbox/namespace identities; current Team/Task constraints and the new
   current receipt gate hold release. A UID/RV-fenced Deployment apply prevents
   stale work from undoing the hold; explicit Sandbox suspension is preserved.
   A full Task reconciliation regression exercises pause, quiescence, binding
   update, fresh authority/receipt, real fenced Deployment apply and explicit
   unlaunch cleanup—not a mocked teardown bypass.
3. **Persistent removal intent:** source value changes and
   `kars.azure.com/credential-removed-keys` update under the same UID/RV fence.
   Core applies tombstones after legacy import and keeps them across retries.
   Tests cover fresh sources, existing pending-import values and explicit re-set.
4. **Local legacy lifecycle handling:** unrelated terminating targets and
   stores/namespaces no longer invalidate global inventory. Secret existence
   precedes namespace validation; transport errors still propagate. Selected
   owners and reviewed source identities remain fail-closed. Related terminating
   source inventory entries are localized rather than revoking unrelated grants.
5. **Private reused values:** templates, not only `values.yaml`, default absent
   new maps. The exact BASE105 values fixture has Git blob
   `09ea1c58f5f6ae9e9705b031aa35386fff7ee35c`. Tests replace current chart defaults
   and exercise actual Helm server-side `lookup` against a local read-only API;
   no real cluster or deployment was used.
6. **Supported v1 consumers:** complete workspace consumer plans are validated
   before consumer changes. Valid v1 and existing unbounded standalone consumers
   are preserved, never mixed with v2 implicitly. Fresh/opted-in v2 updates are
   exercised. Conflicting late entries fail before earlier conversion, and
   malformed private/internal references are not grandfathered. Every write
   remains UID/RV-fenced.
7. **Referenced credential rollout revision:** settings reconcile validates the
   actual enrolled Secret UID/type/key/purpose before a fast path, and hashes
   referenced UID/RV metadata into the rollout version. Tests cover stable
   settings with rotated tokens, wrong UID/missing key/type and no-op/status-RV
   changes. Neither revisions nor patches emit credential values.

Fast validation currently passes **47 core CLI/schema/RPC tests + CLI types**
and **23 private chart/upgrade/packaging tests + gateway lint/types**. Both Helm
lints pass. All owned private changed Rust files were formatted with the private
default configuration (edition 2024), resolving the earlier format-only gate.
Private Next **16.3.3** manifests and its verified lock artifact are unchanged.

No Cargo has run for this repair batch: no core lease is currently held, and
private Cargo remains prohibited pending its separate hosted plan. Rust
regressions are present but **unexecuted**; source parsing is not semantic
qualification. Required core selectors after an explicit paired/default-feature,
offline/locked, existing-target guarded lease:

```sh
cargo check --offline --locked -p kars-controller -p kars-inference-router --tests
cargo test --offline --locked -p kars-controller -p kars-inference-router credential
cargo test --offline --locked -p kars-controller -p kars-inference-router kars_team_reconciler
cargo test --offline --locked -p kars-controller -p kars-inference-router kars_task_execution
cargo test --offline --locked -p kars-controller -p kars-inference-router kars_task_reconciler
cargo test --offline --locked -p kars-controller -p kars-inference-router privacy_rpc
cargo test --offline --locked -p kars-controller -p kars-inference-router observation
cargo clippy --offline --locked -p kars-controller -p kars-inference-router --all-targets -- -D warnings
```

Private qualification must include `credential` and `observation` tests and its
existing format/type/Clippy gates on the private workspace/hosted CI, not the
core target. A bounded independent d033 re-review and real admission/CNI
acceptance still follow qualification. No earlier human waiver applies.

### 2026-09-09 approved core privacy RPC — implemented and core-qualified

The user selected `observation_verifier=core-privacy-rpc`. The former active-SRE
architecture blocker is **closed in code**, without widening Secret-read
permissions or adding another Kubernetes credential/sidecar/proxy.

The existing controller now has a separate authenticated TLS listener on 9448,
with exactly one read-only verification operation. It derives canonical target
lookups, validates the current observer Secret/token/UID/resourceVersion and
full identity/purpose/expiry, rechecks declared recipient/workspace/runtime/
Sandbox/grant identities and name holds, and executes the full `privacy_epoch`
contract for each request. It also checks current registration/private-SA
identity and actual denied access to the verifier's private TLS material by
legacy SRE, declared recipients and the runtime agent identity.

The router pins a live core-owned descriptor, Service/namespace/controller
identity, CA and UID hostname. It makes a fresh bounded TLS request with a
256-bit nonce and request digest, and accepts only a matching operation,
target, scope, version and epoch proof. It neither caches positive proofs nor
reuses an HTTP connection across RPCs. A local scope reset during verification
rejects the old proof. Generic denials reveal no aliases, values, token, key or
backend diagnostic; arbitrary Secret/URL, mutation and token-mint surfaces do
not exist.

Core issues its own TLS material with the existing provider, advertises only
running revision-qualified Pods, and revision-selects the canonical Service.
Recreated/rotated TLS material changes the binding even with identical content.
Observer credentials are explicitly expiring and renew ahead of expiry without
a rollout every reconciliation. Only verifier-backed `Scope` discovery is
allowed during `Prepared`; learned data requires `Ready`. A real TLS
Pod→ReplicaSet→Deployment capability probe checks for the new router marker.
Pending readiness is not handled as destructive revocation.

The chart is opt-in and old/reused values default safely to disabled. Scoped
runtime/controller policies require existing isolation and do not introduce a
blanket BFF egress policy. Metrics 9091 remains separate and does not receive
the bearer. Native Secret GET remains name-authorized RBAC; the name-hold
protocol is retained, not represented as a UID-aware native authorizer.

No new package versions were introduced: the controller now directly consumes
the already-locked workspace `tokio-rustls` and `rustls-pemfile` used by the
router. `Cargo.lock` only adds those existing dependency edges. The existing
TLS transport and constant-time equality implementation are shared, with the
SRE/handoff public entry points preserved.

Core qualification under the explicit existing-target guard passed:

| Filter/check | Result |
| --- | ---: |
| Paired `cargo check --offline --locked ... --tests` | Pass |
| `privacy_rpc` (real TLS + canonical API/full-helper/lifecycle cases) | 11 |
| `observation` (fresh RPC client, purpose and local-scope fences) | 16 |
| `credential` | 97 |
| `github` | 43 |
| `sre_proxy::` | 11 |
| `sre_authority::` | 29 |
| `governed_services::continuity_tests` | 4 |
| `constant_time` | 3 |
| Paired strict Clippy, all targets, `-D warnings` | Pass |
| CLI/schema/Helm regressions + CLI types | 46 tests + typecheck pass |

Filters overlap. Tests include healthy active SRE; alias/admission/UID/epoch/
version/recipient loss; expiry; qualified `None`; nonce/scope/target/purpose
replay; no mutation/arbitrary-Secret endpoint; body/concurrency/deadline bounds;
TLS CA/hostname rejection; material recreation; namespace isolation preflight;
and old capability unavailability. Kind/CNI was **not** run.

The core Cargo lease is **released**, with no remaining Cargo/rustc process.
Minimum observed free space was **8.76 GiB**, above the **8.50 GiB** floor;
release-time free space was **10.03 GiB**. No cleanup of the shared target,
new target/feature variant, network install, image/Docker, cloud/H100, private
BFF Rust or public push occurred.

The private BFF source now requires the new verifier marker and unexpired
binding. Its additional Rust tests are recorded but **not executed** under this
core lease. After release, its Rust source syntax, 19 existing private chart/
packaging tests, gateway lint and Helm lint pass; those checks are not a private
Rust type/test qualification. Parent-coordinated private Rust/API qualification, real Kind/CNI
acceptance and independent review remain required before publication or rollout.

RPC implementation files:

```text
shared/observation_privacy.rs
shared/private_tls.rs
shared/constant_time.rs
controller/src/privacy_rpc.rs
controller/src/privacy_rpc/{authority,discovery,identity,publication}.rs
controller/src/privacy_rpc/tests.rs
controller/src/privacy_rpc/tests/{fixture,lifecycle,boundaries}.rs
controller/src/credential_grants/observer_runtime.rs
inference-router/src/observation_privacy_client.rs
inference-router/src/observation_privacy_client/tests.rs
deploy/helm/kars/templates/observation-privacy.yaml
cli/src/testing/observation-privacy-contract.test.ts
```

Existing controller startup, observer issuer/metadata/network paths, read-only
service identity helper, shared observer contract, router authorization, chart
deployment/values and related tests are wired to these modules. The private
adapter changes are confined to `operator_credentials.rs`,
`observation_credential_tests.rs` and its governed-credentials documentation;
all prior owner edits remain preserved.

### Earlier public-parent forward — qualified core code, lease released

Local checkpoint `45939f6b` preserves the credential closure and its first Rust
qualification. Local merge `330113a0272ca5d12d9fd0e4e3eb40889289399d` then
normally forwards public 550 at
`2d85d5a8bcb1896095fe3431110d6bd87b75e53f`: the real SRE reader-binding/
retirement preflight, fixtures, diagnostics and js-yaml 4.3.2 dependency patch.
The merge was conflict-free. No private implementation was copied, no SRE
worktree was edited, and neither local commit was pushed.

The renewed core-only lease is **released**. The same guarded shared target,
default features, both packages and offline/locked settings were used.

| Combined-source validation | Result |
| --- | --- |
| Check with `--tests` | Pass |
| `credential` | 96 passing tests |
| `observation` | 12 passing tests, including real TLS |
| `github` | 43 passing tests |
| `sre_authority::` | 29 passing tests |
| `governed_services::continuity_tests` | 4 passing tests |
| Strict Clippy, `--all-targets -- -D warnings` | Pass |

Filters overlap. Minimum free space during this forward batch was **9.98 GiB**
against the **8.50 GiB** floor; release-time free space was **10.08 GiB**.
No Cargo/rustc process remained. No private BFF Rust, dependency installation,
new target, target cleanup, image build, cloud operation or public push occurred.
The dependency patch is forwarded source/lock evidence, not a newly built image
claim. The earlier fast CLI/private checks below are not relabeled as fresh
image or private Rust qualification.

**Remaining decision and qualification:** the active-SRE observer verifier
architecture remains an explicit decision (options below); private BFF Rust/
API integration and real Kubernetes admission/lifecycle/CNI tests remain open.
Native Secret GET remains name-authorized Kubernetes RBAC. The name-hold
protocol does not turn it into UID-aware authorization, and this record makes
no end-to-end raw-GET UID-bound security claim.

### Earlier 2026-09-09 bounded core Rust qualification — lease released

The explicit core-only lease has completed and is **released**. Every Cargo
command ran through the parent-provided `files/run-cargo-guard.py`, with this
owned core worktree as `--cwd`, the existing shared target, both packages,
default features, offline/locked mode, two jobs and no incremental compilation.
Minimum free space across the batch was **9.90 GiB**, above the **8.50 GiB**
floor; release-time free space was **10.21 GiB**. No Cargo/rustc process remained
at release. There was no target cleanup, new target, dependency resolution,
installation, private BFF Rust, Docker, cloud operation, commit or push.

Passed:

| Guarded command / test filter | Passing tests |
| --- | ---: |
| `cargo check --offline --locked -p kars-controller -p kars-inference-router --tests` | Typecheck |
| `credential` | 96 (71 controller, 24 router unit, 1 router integration) |
| `observation` | 12 (1 controller, 9 router unit, 2 router integration) |
| `github` | 43 (11 controller, 32 router unit) |
| `governed_services::continuity_tests` | 4 |
| `kars_task::authorization_tests` | 9 |
| `kars_task_execution::api_tests` | 8 |
| `kars_receipt::launch_package::tests` | 7 |
| `cargo clippy --offline --locked -p kars-controller -p kars-inference-router --all-targets -- -D warnings` | No warnings/errors |

Filters overlap; these are not 179 distinct tests. Initial file-name-based
fixture filters selected zero tests and were replaced with the actual Rust
module paths above. The real TLS regression is registered at module scope and
passed: a pinned certificate/UID hostname succeeds, wrong CA/UID hostnames
fail, and the actual accepted TCP peer reaches the existing origin check.
This is local transport evidence, **not** private BFF or real cluster identity/
admission/CNI qualification.

The batch fixed previously uncompiled candidate defects: the TLS listener's
Axum `Connected` adapter, optional NetworkPolicy selectors, missing legacy
fixture fields for the extended blueprint/projection types, and unused shared
constants. Mock Merge Patch now removes null metadata keys like Kubernetes;
the v1 reference-schema test reads its static contract rather than trying to
parse unrelated unrendered Helm includes. Privacy tests require all fourteen
distinct policies **and bindings before issuance**, while permitting repeated
live rechecks. Strict Clippy fixes use borrowed owner slices, a grouped auxiliary
status argument and equivalent conditional syntax—no lint waivers.

An initial shared-target export lookup failure disappeared after rebuilding
the owned library export roots; no missing-export fallback or target cleanup
was introduced. The newer optional privacy forward `5f4f278e` was not merged
during this bounded batch. The qualified source remains the uncommitted
candidate on `93690ba7`; forwarding other work requires requalification.

Additional core files changed during this lease:

```text
controller/src/credential_grants/sources.rs
controller/src/kars_receipt_launch.rs
controller/src/kars_task_authorization_tests.rs
controller/src/kars_task_execution_tests.rs
controller/src/reconciler/credential_source_tests.rs
controller/src/reconciler/governed_services/credential_tests.rs
controller/src/reconciler/governed_services/credentials.rs
inference-router/src/lib.rs
inference-router/src/routes/mod.rs
inference-router/src/service_observation_tls.rs
inference-router/src/service_observation_tls_tests.rs
```

Remaining blockers: the active-SRE observer architecture decision below;
real Kubernetes admission, writer/namespace lifecycle and CNI qualification;
private BFF Rust and private-adapter TLS/API integration; independent review.
The earlier sign-off waivers do not apply.

### Earlier 2026-09-09 continuation (before the Cargo lease)

Owned core baseline remains `93690ba71c62e5efc067580260f2d4125e321c0d`.
All existing private owner changes on `105052141c779af65f52bc55cdaa78951593b886`
were preserved. No publication, visibility change, commit, Cargo execution,
image, customer, H100 or cloud action was performed.

Implemented candidate changes:

- `credential_grants/writers.rs` and its `guards`, `permissions`, and `tests`
  modules: enrolled SA/namespace name holds, exact controller UID checks,
  effective-group permission reviews, owned read-Role absence checks before
  release, and independent writer authority. No Secret/source deletion.
- `credential-reader-admission.yaml`: scoped guard protection including
  namespace status/finalize, the native namespace finalizer, and schema-specific
  fences against reader Role/RoleBinding aliases.
  Native RBAC is still name-bound; the enforceable lifecycle is the proposed
  continuity mechanism, not an endpoint UID check.
- `credential_grants.rs`, writer/observer RBAC, grant admission/schema,
  readiness regressions and CLI review: `WriterReady` is independent from
  valid delivery; explicitly empty writer lists retire authoring rights.
- `observation_network.rs`: read-only live sender egress preflight. The private
  add-on has an off-by-default, explicitly confirmed additive TCP 9447 policy
  for reviewed target namespace names. No core-generated sender isolation.
- Issuer/shared observer/runtime guard and regression: nonempty SRE epochs
  cannot be issued/reused via the incomplete status-only observer path.
  Pending migration retains its existing non-destructive behavior.

Fast validation: **42 core tests**, CLI typecheck, **19 private chart/packaging
tests**, private gateway lint/typecheck, and both Helm lints pass. The 16 changed/
new Rust modules pass direct rustfmt checks and are each at most 400 functional
lines, but no Rust test or type/Clippy qualification has run. Core validation
used existing read-only cached packages after the missing-runner failure:
Vitest 4.1.10, Vite 8.2.1, TypeScript 5.9.3 (the first two differ from the lock's
4.1.8/8.0.16). This is fast source evidence, not locked dependency qualification.
No cache links are to be staged.

Exact fast commands, from the respective `cli` and private `teams-gateway`
directories after using existing cached dependencies:

```sh
node node_modules/vitest/vitest.mjs run \
  src/commands/credential-grants.test.ts \
  src/testing/credential-grant-contract.test.ts src/lib/credential-source.test.ts
node node_modules/typescript/bin/tsc --noEmit

node node_modules/vitest/vitest.mjs run tests/chart.test.ts tests/packaging.test.ts
./node_modules/.bin/oxlint src/ tests/
node node_modules/typescript/bin/tsc --noEmit
```

Core continuation file inventory (all relative to the owned core worktree):

```text
cli/src/commands/credential-grants.ts
cli/src/commands/credential-grants.test.ts
cli/src/testing/credential-grant-contract.test.ts
controller/src/credential_grants.rs
controller/src/credential_grants/admission.rs
controller/src/credential_grants/observation_network.rs
controller/src/credential_grants/observer_rbac.rs
controller/src/credential_grants/operator.rs
controller/src/credential_grants/rbac.rs
controller/src/credential_grants/readiness/tests.rs
controller/src/credential_grants/writers.rs
controller/src/credential_grants/writers/guards.rs
controller/src/credential_grants/writers/permissions.rs
controller/src/credential_grants/writers/tests.rs
deploy/helm/kars/templates/crd-karscredentialgrant.yaml
deploy/helm/kars/templates/credential-grant-admission.yaml
deploy/helm/kars/templates/credential-grant-rbac.yaml
deploy/helm/kars/templates/credential-reader-admission.yaml
inference-router/src/routes/observation_tests.rs
inference-router/src/routes/observation_privacy_tests.rs
inference-router/src/routes/observations.rs
inference-router/src/service_observation.rs
shared/service_observer.rs
docs/how-to/governed-credential-grants.md
docs/security-audits/2026-09-08-governed-credential-grants.md
```

Private continuation edits are limited to `docs/governed-credentials.md`,
`deploy/helm/kars-bridge/values.yaml`, the new
`deploy/helm/kars-bridge/templates/observation-egress.yaml`, and
`teams-gateway/tests/chart.test.ts`. All other preexisting private owner changes
remain in place and still require the separate BFF Rust plan.

The reader hold must still be qualified against real API admission, ordinary
Helm SA deletion, namespace `/status` and `/finalize`, RoleBinding User/Group
aliases, delayed Role deletion, leadership transition, and UID reuse. Existing
controller leadership serializes the grant loop (new writer issuance rejects
the disabled-leadership mode); asynchronous revocation alone
is not claimed to provide UID-bound GET. A missing/replaced controller identity
or preexisting reader Role without pinned provenance requires explicit operator
recovery rather than silently adopting it.

### Historical architecture decision (now implemented above)

`privacy_epoch` performs a live private-SA token-alias Secret metadata inventory.
The BFF/ordinary router identity cannot receive native Secret `list` permission
for that inventory: content negotiation is not an RBAC boundary and would
expose full private values. Reusing the SRE backend's full control credential is
also not an acceptable substitute. The candidate therefore reports unavailable
for active SRE instead of returning a false privacy-qualified observation.

Safe bounded choices for approval are:

1. **Purpose-only core privacy RPC (preferred):** core invokes the existing full
   helper per request and returns only current purpose/target/grant/epoch proof
   to the exact observer; no Secret values or general API proxy.
2. **Dedicated private metadata-verifier identity:** separate protected
   credential and admission/lifecycle guards, never mounted into BFF/agent,
   with explicit review of its unavoidable raw-list authority and revocation.

Neither new authority path has been silently designed into this candidate.
At that checkpoint active-SRE observations and combined core qualification were
blocked. The approved RPC and core qualification above supersede those two
blockers; private BFF Rust/TLS/API and real cluster acceptance remain open.

Rust parser checks and Helm lint have run without Cargo. Nineteen
operator CLI/schema/v1 compatibility tests pass using the existing verified
cache; CLI typecheck passes. Eighteen private add-on/packaging tests and the
gateway lint pass. After the exact GitHub forward merge, the 19 core fast
tests and CLI typecheck pass again, along with all seven GitHub client
regressions. Merged Rust files pass syntax parsing and Helm lint remains
successful. Forwarding `068ae160` also passes all 11 existing read-only Python
bootstrap fixtures and the 19 credential CLI/schema regressions. Newly added
Rust observer-route, purpose-issuer and GitHub
configuration tests have **not run**. Full formatting and Rust type/Clippy
qualification are pending. No dependency
installation, Docker build, live cluster call, H100/cloud action or image push
was performed.

After wiring live Task readiness, state-preserving pause and GitHub retirement,
the strengthened fast suite passes 20 tests and CLI typecheck. All changed Rust
files pass syntax parsing and their functional modules remain below the
existing caps. The new Rust behavior tests are still unrun; no Cargo lease was
implicitly reacquired.

Rust test and strict Clippy qualification require the separately coordinated
existing target lease. Real Kubernetes tests must demonstrate admission
type-checking, actual ServiceAccount permissions, first binding, source and
grant recreation, concurrent CAS, legacy migration, revocation, namespace
reuse, Team lifecycle and optional Teams bootstrap. Offline rendering and mocked
API tests alone cannot qualify those claims.

Any author waiver on earlier publication PRs does not apply to this change.

## Explicit open blockers

- The first direct Cargo lease was released unused because the newly required
  privacy closure had not yet been forwarded. The exact
  `068ae16041ecf7bd2b8321dfeb22e381ebbd587b` closure is now integrated without
  dependency changes. The later core-only lease and passing results are recorded
  above. Private BFF Rust remains unexecuted and requires its separate plan.
- The exact GitHub consumer `d3dc3ce85b72869497a8f0a32815609e48a26c62`
  is forward-integrated after the local `b3f6ca83` issuer checkpoint. Its
  reviewed projection helper is reused once: optional for legacy standalone
  configuration, required for a successfully issued governed binding.
  The combined issuer/consumer candidate now passes the targeted core Rust
  qualification above;
  the parent's separate 33 Rust tests/strict Clippy and seven Node tests do
  not qualify the additional issuer or observation code.
  Passing regressions cover identical JSON under a changed source
  revision, retirement of old cached consumers, typed Pending-privacy
  non-issuance, and canonical App IDs without changing customer store values.
  Further passing regressions cover pre-Ready source checks, ordinary Ready
  revocation, self-bootstrap versus ancestor readiness, UID-owned pause without
  data deletion, and retirement that cannot re-enable the legacy GitHub mount.
- The approved RPC invokes the full strict `privacy_epoch` helper for active-SRE
  observations; status is not treated as full live proof. Real cluster and
  private BFF integration qualification remain required.
- Native Secret GET Roles and RoleBinding subjects are name-bound. The
  observer endpoint additionally rejects stale recipient UIDs, but raw agent/
  integration-store reads cannot acquire UID semantics through that endpoint.
  The new scoped name-hold admission/lifecycle candidate passes its Rust tests,
  but still needs real API qualification before declaring the boundary satisfied.
- Deleted writers no longer invalidate delivery verification; new tests cover
  source continuity and selected-source revocation. Those Rust tests pass;
  real uninstall/reinstall lifecycle qualification is still required.
- Private TLS hostname/CA/Pod-lineage success, migration, grant/source/SA/
  namespace replacement and admission enforcement need real API qualification.
- Existing BFF egress isolation must explicitly permit runtime TCP 9447.
  Core adds receiver-scoped ingress, not a new policy that isolates the BFF
  and breaks its pre-existing API/provider traffic. Shared-namespace egress
  enrollment/preflight is implemented with explicit private chart opt-in and
  remains subject to real CNI/API qualification.

These are not waived and the candidate is not ready for publication or rollout.

## Guarded Rust command record and pending private plan

The core commands above ran under the direct parent lease, using the existing shared target,
`CARGO_BUILD_JOBS=2`, `CARGO_INCREMENTAL=0`, offline/locked mode and the active
8.5 GiB stop guard:

```sh
cargo check --offline --locked -p kars-controller -p kars-inference-router --tests
cargo test --offline --locked -p kars-controller -p kars-inference-router credential
cargo test --offline --locked -p kars-controller -p kars-inference-router observation
cargo test --offline --locked -p kars-controller -p kars-inference-router github
cargo test --offline --locked -p kars-controller -p kars-inference-router governed_services::continuity_tests
cargo clippy --offline --locked -p kars-controller -p kars-inference-router --all-targets -- -D warnings
```

The core lease is released. The private BFF is a separate workspace/dependency variant and requires explicit
coordination before using that target:

```sh
cargo test --offline --locked --manifest-path bff/Cargo.toml credential
cargo clippy --offline --locked --manifest-path bff/Cargo.toml --all-targets -- -D warnings
```

Latest release observation: 10.03 GiB available; no Cargo/rustc processes.
Minimum latest-batch free space: 8.76 GiB. No new lease is implicitly acquired by
editing documentation, formatting source, or forwarding another parent.
