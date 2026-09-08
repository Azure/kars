# Governed credential grants — qualification record

Status: implementation candidate; **not a sign-off**. No author or independent
reviewer signatures are supplied. Existing audit gates remain required.

## Scope

Metadata-only operator grants, native Secret source authoring, UID-bound
Sandbox/Task/Team delivery, explicit workspace/Team/target precedence, legacy
preflight/import, purpose-bound operator stores, and separate egress operator
access. Private Bridge adapts to the public core contract; it is not copied into
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

Source formatting/parser checks and Helm lint have run without Cargo. Six
operator CLI preflight tests pass using the existing verified cache; CLI and
private web typechecks pass. Private add-on/packaging tests pass. No dependency
installation, Docker build, live cluster call, H100/cloud action or image push
was performed.

Rust test and strict Clippy qualification require the separately coordinated
existing target lease. Real Kubernetes tests must demonstrate admission
type-checking, actual ServiceAccount permissions, first binding, source and
grant recreation, concurrent CAS, legacy migration, revocation, namespace
reuse, Team lifecycle and optional Teams bootstrap. Offline rendering and mocked
API tests alone cannot qualify those claims.

Any author waiver on earlier publication PRs does not apply to this change.
