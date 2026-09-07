# Security Audit - Sandbox namespace ownership

Date: 2026-09-07
Review status: Implementation evidence prepared; independent review and genuine
author/reviewer sign-offs are pending.

## Scope

This bounded slice follows the inference foundation in Azure/kars#547. It adds
claim-v1 namespace ownership, guarded namespace cleanup, read-only migration
preflight, explicit administrator adoption, and compatible CLI credential
prestaging. The candidate integrates parent `45cfa009` at `0c28a49f`; subsequent
review repairs also cover local-development and built-in SRE creation paths,
plus the existing handoff credential writer in `inference-router/src/spawn/`.

Affected surfaces are `controller/src/reconciler/namespace_ownership.rs`,
Sandbox reconciliation, egress-approval target access, router-token reads,
`cli/src/lib/namespace-ownership.ts`, the namespace/add/upgrade commands, and the
controller namespace RBAC rule. The CLI command changes are capability-gated
paths. The migration guide is `docs/how-to/namespace-ownership.md`.

This slice does not implement generic credential sources, shared task budgets,
Bridge permissions, or new runtime execution. It does not rename namespaces,
broaden existing credential propagation, alter default images, or introduce Helm
ownership of customer Sandbox namespaces. No live cluster migration is claimed.

## T1: New capability / attack surface? YES

- Namespace claims bind the source workspace, Sandbox name and exact Sandbox
  UID. A backlink binds the Sandbox to the exact Namespace UID.
- `kars namespace preflight` reads Sandbox, Namespace and Deployment metadata
  across the cluster. It does not read Secrets or mutate resources.
- `kars namespace adopt` is a privileged, metadata-only administrator operation.
  It requires explicitly reviewed Sandbox and Namespace UIDs, a unique live
  Sandbox, and compare-and-swap writes. Foreign or partial claims, terminating
  resources and stale identities are rejected, not overwritten.
- CLI credential prestaging in both `add` and local-Kubernetes `dev` uses a
  two-way reservation and Namespace UID backlink. The controller consumes the
  reservation for one Sandbox incarnation before touching workloads.

Claim annotations are controller/namespace-administrator authority, not tenant
assertions. Kubernetes authorization remains the boundary against changing that
authority. This design does not protect against a cluster administrator rewriting
claims or bypassing lifecycle controls.

## T2: Security-control change? YES

- Namespace creation is atomic. Existing resources are not adopted through
  force apply merely because their names match.
- Same-name Sandboxes in different source workspaces cannot claim or clean up
  each other's runtime namespaces.
- Cleanup rechecks the live claim and uses Namespace UID/resourceVersion
  preconditions. Non-404 failures retain the Sandbox cleanup finalizer for retry;
  unrelated finalizers are preserved.
- Egress-approval target writes/cleanup and router admin-token reads use the
  shared read-only ownership guard. Policy confirmation retains the originating
  workspace instead of treating the bare Sandbox name as authority.
- Legacy adoption requires unique live identity, existing status/finalizer,
  controller-managed namespace/deployment evidence, matching selectors, and a
  Deployment created strictly after the Sandbox incarnation. Names, labels or
  status alone are not sufficient.
- Adoption preserves Namespace UID, data and pod templates. It does not copy
  another Sandbox incarnation's credentials or trigger a workload rollout.
- Existing handoff credential propagation uses the created Sandbox's actual
  workspace and UID, waits for a matching claim/backlink, and rejects changed or
  terminating targets. A metadata-only Secret anchor is established before a
  fresh ownership check; credential values are written with Secret
  UID/resourceVersion fencing rather than blind apply. Metadata-only responses
  and sanitized errors avoid exposing credential values in logs. Existing
  credential sources and RBAC are unchanged.

## T3: Availability / fail-open risk? MIXED, migration-gated

Controller claim conflicts leave target resources untouched and report
`NamespaceOwnershipConflict`. They are not treated as a successful empty
namespace. API/RBAC/transport/malformed-response failures remain errors.

`kars upgrade`, `kars up --upgrade`, and SRE installation against an existing
controller run the read-only preflight before controller replacement.
Direct Helm/GitOps upgrades must run it explicitly.
Ambiguous legacy installations require an administrator decision before upgrade:
same-second creation timestamps, older field-manager formats, overlay-only
deployments and unfinished unmarked prestaging are not automatically trusted.
Existing running pods are not stopped by a rejected adoption, but successful
controller reconciliation must not be claimed until ownership is resolved.

Pre-CR namespaces cannot be inventoried through Sandbox CRs alone. Operators must
inventory unfinished reservations separately and update provisioning clients to
claim-v1 before further credential prestaging. The migration guide records this
limitation rather than silently broadening Secret permissions.

The cleanup finalizer waits for accepted namespace deletion, not namespace
disappearance, avoiding a deadlock when the Sandbox CR resides inside its own
runtime namespace. Terminating namespaces retain their claims. Rollback to older
controllers removes these guards because older binaries ignore claim-v1;
provisioning/deletion/name reuse must be paused during such a rollback.

## Verification

The candidate integrates parent `45cfa009` at `0c28a49f`. The complete local
repair batch has the following evidence:

- 42 focused controller tests passed, covering namespace ownership, SRE writer
  materialization and actual reconciler-entry finalization races.
- 32 spawn tests passed, including 11 credential-claim regressions. They cover
  normal propagation and zero credential-value writes for foreign workspaces,
  recreated CR/Namespace/Secret UIDs, missing claims/backlinks, terminating
  targets and API failures.
- 161 combined CLI/Helm/dev tests passed, including first-party creation,
  preflight, true absence versus failed discovery, and Helm-compatible dotted
  release names with the 53-character limit.
- Strict all-target Clippy passed for both crates; CLI typecheck/scoped lint,
  formatting and existing LOC/crypto/no-stub gates passed.

Independent automated review found three blockers in `e5870631`: first-party
local-development/SRE namespace prestagers, the existing handoff credential
writer bypassing claims, and namespace GC deadlocking an unclaimed self-hosted
legacy Sandbox. Bounded follow-ups identified SRE failed-discovery handling,
dotted Helm names, and cancellation during v1 reservation binding. The original
legacy-GC repair, SRE writer companion and dotted-release repair have automated
closure; final closure of the reservation-cancellation follow-up and handoff
credential writer is pending. Automated technical review is not human approval.

Actual Helm rendering against an isolated read-only API fixture covers
legacy Namespace/account retention, disabled SRE, foreign releases, missing
resources and lookup errors. Fresh SRE namespaces are left to the controller;
old exact-release Namespace manifests remain present with retention metadata.
Client-only rendering cannot discover prior ownership: the migration guide
requires retaining legacy namespaces when moving to pruning GitOps.

The SRE discovery repair validates Helm's JSON inventory and controller
identity, treats only a successful empty `--ignore-not-found` response as
absence, bounds reads to 30 seconds, and propagates failures without installing.
Existing compatible Helm writer accounts are left unchanged; conflicting data
or ownership is not force-overwritten.

The GC regression models namespace deletion beginning after CR finalizer
creation but before the namespace claim patch. The first patch conflicts; the
next real reconciler entry recognizes the valid two-way prestage, completes
normal guarded cleanup and preserves other finalizers. Foreign/partial
reservations and UID mismatches remain rejected. The legacy-only shortcut still
releases just its own CR finalizer without adopting an unproven namespace.

The existing disposable Kind harness now includes SRE installation, exact
Sandbox/Namespace UID binding, writer-account materialization with automount
disabled, and controller cleanup preserving the core namespace. Shell syntax
has been checked; execution of that new lifecycle gate awaits hosted CI.

Local TypeScript checks use the existing authorized dependency cache, not a
lockfile-exact installation; hosted CI must supply exact-lockfile evidence.
No live customer or dedicated test-cluster upgrade was performed.

## Verdict

Pending independent review and migration-compatibility closure. This document is
not an approval or a sign-off. Genuine author and independent reviewer sign-offs
must be supplied before the capability-audit gate can pass.
