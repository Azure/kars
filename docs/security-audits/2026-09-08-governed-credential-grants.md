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

### 2026-09-09 public-parent forward — qualified core code, lease released

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

### Required architecture decision: active-SRE observation privacy

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
Active-SRE observations, combined core Rust qualification and private BFF Rust/
TLS/API qualification remain blockers. This is not a completed feature sign-off.

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
- The issuer consumes the full strict `privacy_epoch` helper. Active-SRE
  observation issuance/reuse is now explicitly unavailable pending the
  architecture decision above; status is not treated as full live proof.
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

Latest release observation: 10.08 GiB available; no Cargo/rustc processes.
Minimum latest-batch free space: 9.98 GiB (earlier batch: 9.90 GiB). No new lease is implicitly acquired by
editing documentation, formatting source, or forwarding another parent.
