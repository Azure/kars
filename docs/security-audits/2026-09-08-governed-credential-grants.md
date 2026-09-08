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
- Core-owned namespace/projection writes and typed provider/Teams reconciliation.
- Namespace admission limits the private adapter's remaining namespace create
  permission to its dedicated local-inference namespace.
- Enrolled-store UID/purpose admission, source-only Roles and no broad Secret
  or Deployment mutation rule in either private Bridge RBAC manifest.
- No raw credential values in the grant schema, metadata status, preview files
  or diagnostic messages.

## Current validation

Rust parser checks and Helm lint have run without Cargo. Nineteen
operator CLI/schema/v1 compatibility tests pass using the existing verified
cache; CLI typecheck passes. Eighteen private add-on/packaging tests and the
gateway lint pass. Newly added Rust observer-route, purpose-issuer and GitHub
configuration tests have **not run**. Full formatting and Rust type/Clippy
qualification are pending. No dependency
installation, Docker build, live cluster call, H100/cloud action or image push
was performed.

Rust test and strict Clippy qualification require the separately coordinated
existing target lease. Real Kubernetes tests must demonstrate admission
type-checking, actual ServiceAccount permissions, first binding, source and
grant recreation, concurrent CAS, legacy migration, revocation, namespace
reuse, Team lifecycle and optional Teams bootstrap. Offline rendering and mocked
API tests alone cannot qualify those claims.

Any author waiver on earlier publication PRs does not apply to this change.

## Explicit open blockers

- No Cargo lease was assigned to this candidate; neither core nor private BFF
  has been compiled or Rust-tested.
- The exact GitHub runtime schema was read at `d3dc3ce8`; that consumer has not
  been forward-integrated/qualified here. Its optional mount must be reconciled
  with the source issuer's owned projection, not duplicated on merge.
- The issuer consumes the full strict `privacy_epoch` helper from `7dc72810`.
  The observation RPC currently rechecks registration status and real legacy
  GET/LIST/WATCH denials, but not the full admission/private-token-alias scan.
  Status alone is not equivalent to that full live proof.
- Native Secret GET Roles and RoleBinding subjects are name-bound. The
  observer endpoint additionally rejects stale recipient UIDs, but raw agent/
  integration-store reads cannot acquire UID semantics through that endpoint.
  ServiceAccount recreation needs an enforceable admission/lifecycle closure
  before declaring the complete contract satisfied.
- Uninstall retains core data and sources, but a deleted enrolled writer can
  block opted-in source consumers. Source continuity versus writer revocation
  requires closure and real lifecycle tests.
- Private TLS hostname/CA/Pod-lineage success, migration, grant/source/SA/
  namespace replacement and admission enforcement need real API qualification.
- Existing BFF egress isolation must explicitly permit runtime TCP 9447.
  Core adds receiver-scoped ingress, not a new policy that isolates the BFF
  and breaks its pre-existing API/provider traffic. Shared-namespace egress
  enrollment/preflight remains to be completed and qualified.

These are not waived and the candidate is not ready for publication or rollout.

## Pending leased Rust selectors

Only after a direct parent lease, using the existing shared target,
`CARGO_BUILD_JOBS=2`, `CARGO_INCREMENTAL=0`, offline/locked mode and the active
8.5 GiB stop guard:

```sh
cargo test --offline --locked -p kars-controller -p kars-inference-router credential
cargo test --offline --locked -p kars-controller -p kars-inference-router observation
cargo clippy --offline --locked -p kars-controller -p kars-inference-router --all-targets -- -D warnings
```

The private BFF is a separate workspace/dependency variant and requires explicit
coordination before using that target:

```sh
cargo test --offline --locked --manifest-path bff/Cargo.toml credential
cargo clippy --offline --locked --manifest-path bff/Cargo.toml --all-targets -- -D warnings
```

Last read-only disk observation: 9.7 GiB available; no cargo/rustc processes
observed. No lease was acquired or implicitly transferred.
