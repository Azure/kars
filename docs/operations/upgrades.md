<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Upgrades & rollback

This runbook covers moving a running kars cluster from one release to the next,
and reverting if something looks wrong. The CLI does the heavy lifting; this page
explains what happens, how to verify, and how to roll back.

> **Scope.** Production AKS clusters running kars `v1alpha1`. Local `kars dev`
> stacks "upgrade" by recreating the ephemeral container/kind cluster from the
> newer images — there is nothing stateful to migrate.

## SRE recovery during a protected controller upgrade

A repaired controller can reconcile the enrolled SRE while root-image migration
is finalizing. Finalization verifies the canonical SRE registration, original
Sandbox/runtime identities, current credential references, provider revision and
budget CA revision before accepting those specific rollout annotation changes.
If runtime images also reconcile, the named `agent` and `egress-guard` image
slots must match the reviewed controller identity and template: Hermes uses
`HERMES_RUNTIME_IMAGE`, OpenClaw uses its source Sandbox image override or
`SANDBOX_IMAGE`, and the egress guard always uses `SANDBOX_IMAGE`. A changed
`inference-router` image additionally requires an explicit projection binding to
the sealed controller's immutable `INFERENCE_ROUTER_IMAGE`; only that verified
binding permits historical Deployment, ReplicaSet and Pod comparison against the
original router image. The controller template, source identity and generation
are rechecked on resume. Unbound router changes, commands and other template
changes are not normalized away.
Replica intent comes from the source Sandbox's suspension and credential-rebind
hold, not from a degraded Deployment captured at zero replicas. Fresh successor
Pods must retain the qualified epoch and reviewed ownership chain. Other template
changes, stale credentials, foreign Pods and prematurely restored writers remain
refused.

If reconciliation interrupts finalization, resume `migrate-root-image finalize`
with the same migration journal. The CLI retains the preceding control transition
and recognizes its recorded seal to recover lost acknowledgements. Do not rerun
retirement or rotate TLS again, edit the journal, or manually scale companions to
force completion. Bridge companions and writer authority are restored only after
the migration's normal checks pass. Completed-migration review for a fenced Helm
publication uses that same verified replica intent and rejects subsequent drift.
When persisting reviewed runtime images into Helm, retain the original immutable
OpenClaw and Hermes references in `sre.runtimeImageBaseline`. If the sealed
`sandbox.image` includes a separate `digest`, update it to match the selected
`latest@sha256` tag. The upgrade comparison restores that field to the original
digest without allowing it to be added or removed from the sealed image shape.
The same rule applies to a separately stored `inferenceRouter.image.digest` when
using `sre.routerImageBaseline`: the selected tag, explicit digest and budget
verifier must agree, while the original adoption receipt remains unchanged.
For a UID/resourceVersion-fenced protected controller publication, Helm preserves
the live environment order only when the named declarations are identical after
Kubernetes' empty-value and `fieldRef.apiVersion: v1` defaulting. This avoids a
rollout caused solely by ordering; changed declarations still reach the strict
server-side no-op check and are rejected there.
Successful finalization is not proof of SRE
diagnosis or inference; verify those through the customer workflow separately.

A controller reconciliation receipt may change while companions return to Ready.
Finalization and completed-migration review accept that namespace annotation only
when the shared verifier binds it to the qualified epoch, reviewed Deployment
baseline, exact live template, source Sandbox identity and generation. Missing or
invalid receipts and annotations for unreviewed consumers still fail the inventory
fence; they are not ignored as arbitrary namespace metadata. This keeps a normal
credential refresh from preventing writer restoration without relaxing workload
admission or requiring a replacement migration journal.

## Image-only refresh of a non-private mission dispatcher

An existing Helm-owned `kars-mission-dispatcher` that is **not private-enrolled**
can receive one reviewed immutable image without publishing the Core release:

```bash
kars dispatcher-image review --context <context> --namespace kars-system \
  --release kars --image '<registry>/dispatcher:latest@sha256:<digest>' \
  --receipt dispatcher-image.jsonl
kars dispatcher-image apply dispatcher-image.jsonl
# If the acknowledgement was lost or rollout was delayed (no second patch):
kars dispatcher-image verify dispatcher-image.jsonl
```

This is distinct from private-root companion migration: the latter requires
actual enrollment and private retirement/requalification. Never enroll the
dispatcher or fabricate private authority to obtain an image update. This command
supports only the existing single-container, zero-or-one replica, `Recreate`
mission-dispatcher Deployment, its dedicated ServiceAccount and its independent
mission identity Secret reference. Old and new images must be different digest
references. No rendered YAML or arbitrary patch is accepted. The target spec is
constructed from the live spec by changing only `mission-dispatcher.image`.

Review binds the explicit context, cluster and namespace identity, Deployment
UID/resourceVersion/full metadata/spec, mission ServiceAccount and Secret
metadata, private namespace evidence (including historical seals/retained parent
UIDs), enrolled workload specs, and grant identities/specs. Secret values are
never requested. Apply rereads that inventory, refuses enrollment or drift, then
uses a single JSON patch with UID, resourceVersion, full metadata and full-spec
CAS tests. Dispatcher reads explicitly retain `managedFields`: the default
kubectl JSON printer omits them, which would make the whole-metadata CAS reject
an unchanged server object. Missing fields and explicit nulls are not silently
normalized or removed from CAS. All other Deployment fields, Helm ownership, mission identity and
replica intent remain unchanged. It neither creates/relaunches a Mission nor
updates the private root, enrolled consumers, grants, TLS, spending/accounting,
schemas or Helm release records. Kubernetes recreates the dispatcher Pod using
its existing rollout strategy; the existing Mission remains the same execution.

The private mode-0600 receipt contains the exact reviewed Deployment, so retain
it as operator evidence. It is append-only, hash-chained, fsynced and locked;
existing files are never overwritten. An attempt is durably recorded **before**
its one patch. Applied and failed records are retained, including uncertain
acknowledgements. Subprocess failures retain allowlisted operation, resource kind,
Kubernetes reason and exit status in `commandFailure` and the CLI error. The
shared diagnostic projection uses `Unscoped`; it confers no private authority.
Raw stderr/stdout, command arguments, templates and credentials are not copied
into diagnostics. Previous failure records survive later read-only verification.
An attempted receipt cannot be applied again. `verify` only
reads the cluster and appends evidence: it requires the actual Deployment to
have the exact target spec/identity, unchanged protected inventory and completed
rollout (using the Deployment's existing progress deadline during apply).
Deployment readiness is not customer acceptance; continue the existing Mission
and inspect its real deliverable separately. A failed pre-write attempt needs a
fresh review/file only after inspection; preserve the old receipt. A stale local
lock is not broken automatically.

This is an image-only operator transaction, **not** a full Core publication,
Helm reconciliation, release-history repair or a bypass of its guard. A failed
Core revision remains failed. The Helm stored manifest intentionally retains its
old image until a separately authorized Core publication reconciles it. The
usual full-publication guard still applies. Inventory checks are client-side;
Kubernetes cannot atomically CAS the Deployment together with other resources.
Avoid concurrent enrollment/publication: drift detected after the image patch
is recorded as failure, never as success, and is not automatically rolled back.

## Private-qualified roots: controller changes are currently blocked

The upgrade procedures below are for roots without private qualification or
retirement evidence. A completed private qualification seals the controller
template; keeping the Deployment UID does not make an image, env or rollout
annotation change compatible. There is not yet a supported root-template
upgrade/requalification transaction.

Controller-changing operations stop with `KARS_PRIVATE_ROOT_UPGRADE_BLOCKED`
before image publication, schema writes, Helm mutation or restarts when the
target root has private evidence. This includes Pending/restoring attempts,
completed proofs, retained epochs and malformed or unknown private receipts.
Unreadable or malformed API evidence produces `KARS_PRIVATE_ROOT_CHECK_UNAVAILABLE`,
not an assumption that the root is unqualified. Diagnostics do not print private
proofs, templates or command stderr.

The guard covers `upgrade`/rollback, `up`/fast upgrade, controller-affecting
`push --apply`, shared schema-apply entrypoints and controller rollout helpers.
Full `up` checks an existing AKS before provisioning, using its existing
discovery/connection operations. Only an authoritative absent-cluster result
selects fresh installation; permission and transport failures stop the operation.
Context-bound `push` also checks publication without `--apply` when its mutable
tag is used by a root container. It connects to the saved deployment rather than
inspecting an unrelated ambient Kubernetes context.

Read-only previews, including `up --upgrade --dry-run` and
`upgrade --rollback --dry-run`, remain non-mutating. Original
`credentials grant preview`/`apply` recovery is not intercepted. Finishing that
recovery does **not** authorize a later root-template change. Do not clear proofs,
delete grants or use force/rollback flags to bypass the refusal.

Bridge-only and sandbox-only operations that do not touch the root remain
available only when they also preserve enrolled private consumers. The Bridge
chart refuses an ordinary upgrade of an enrolled BFF or Teams gateway with
`KARS_PRIVATE_CONSUMER_UPGRADE_BLOCKED`. It checks both the live template epoch
and retained namespace parent-UID evidence. A non-consuming enrolled consumer
has no required epoch but still has a sealed template binding; neither its
absent epoch nor an epoch lost in an earlier upgrade is permission. Offline `helm template` does
not query the cluster and is not upgrade authorization. Do not restamp an old
epoch onto a changed template or rewrite its sealed proof to bypass this check.
In particular, `push --only sandbox --apply` is **not** root-unchanged:
it updates the controller's `SANDBOX_IMAGE` configuration and restarts it.
Unrelated image publication, and publication of a tag from which the root is
already digest-pinned, do not themselves authorize or trigger a root restart.
Standalone publication without a saved cluster target retains its existing
behavior; it is not proof that no other deployment consumes that registry.

This is a client-side refusal guard, not the missing migration protocol or an
atomic lock against concurrent operators. No root/grant metadata, controller
policy or admission permissions are changed by the check.

The controller also compares the actual live root Pod template with its
reviewed qualification before accepting a private namespace epoch. Image,
environment, label and rollout-annotation changes invalidate that qualification;
only the separately managed private epoch annotation is excluded from the
template digest. This fail-closed check does not authorize a root upgrade or
provide a migration transaction.

Writer authority validation also rechecks every explicitly reviewed consumer's
live UID, template digest and retained parent binding. This includes enrolled
consumers that do not consume private material and therefore need no epoch.
Private-authority consumers additionally require their current template epoch.
Missing, terminating, replaced or changed consumers make writer authority
unavailable through the existing revocation path; delivery authority and the
grant itself are not reset. Status-only resource-version changes remain valid.

Private retirement checks root Deployment descendants by their actual
ReplicaSet ownership throughout capture, draining and the final retirement
barrier, including controller-profile and budget-TLS recovery, not just by
their current ServiceAccount or mounted material.
Newly observed or terminating old-root Pods cannot be skipped when those
credentials differ; an unreviewed descendant preserves Pending protection
without publishing a fresh epoch. An exactly reviewed root Pod that consumes
no private authority is preserved without pausing or restarting its Deployment.

Operators can prepare a read-only root migration inventory from an exact,
locally rendered Helm release:

```bash
kars credentials grant review-root-migration rendered-release.yaml \
  --private-root kars-system --release kars --context <context>
```

This checks the sealed qualification, all affiliated namespaces (including
retained namespaces without grants), current grant identities and specs, and
immutable old/target root images. Server-side dry-run projections check the
target without persisting it. Changes to the root ServiceAccount, selector,
replica intent, budget TLS identity or other reviewed private consumers are
refused. A second complete inventory detects concurrent changes. Output contains
identity and digest bindings, not Secret values or the rendered Pod template.
Before a governed router image change, new reviews require the target controller
to retain `KARS_INFERENCE_BUDGET_FIRST_INSTALL_ROUTER_DIGEST`. Preserve an existing
baseline; when introducing it, use the original governed router digest, not the
new image digest. Helm renders this from `sre.routerImageBaseline`. The live
`KARS_INFERENCE_BUDGET_ROUTER_DIGEST` still follows the new router image. Omitting
the baseline changes verification of retained first-install funding and can leave
SRE unable to start. Review rejects that omission before withdrawing writers or
retiring workloads. Historical receipts remain readable; this preflight does not
repair an already-started migration or authorize editing its sealed target.

If an older migration has already sealed the new controller qualification but
omitted that baseline, use the narrowly scoped successor lifecycle:

```bash
kars credentials grant migrate-root-image restart-router-baseline failed.jsonl \
  --receipt corrected.jsonl --context <context>
```

This requires the exact failed target to be sealed, all reviewed companions
stopped, acknowledged withdrawn writers, unchanged authority and a real rotated
budget key. It only introduces the original verifier proven by the failed
receipt; it does not accept another image or arbitrary configuration. The new
private journal retains the failed receipt, original grant and replica intent,
and all captured Pods and exposed keys. The old journal is preserved and marked
superseded. The maintained retirement path stops current execution and creates a
fresh retirement attempt. Resume interruptions with `resume-retirement` on the
**new** receipt, rotate the budget key and public CA again, then `finalize` that
receipt. Never edit the old target or reuse its already-exposed key.

During restoration, budget-governed companions receive only a stopped epoch
update from the operator. Their real controller must restore nonzero replicas;
source-derived reconciliation is verified before writers return. Native
admission continues to deny operator-driven nonzero budget workload writes.

For an already-rolled immutable image-only change, the explicit `--sealed-root`
option accepts a retained Deployment JSON matching the historical seal. That
recovery review additionally includes the historical and observed Deployment
objects: keep it private. It verifies old authority, not current execution, and
requires the projected target to retain exactly the already-observed image delta.
Actual current Pods must retire and the budget key must rotate before fresh
qualification; it does not roll back images or repair annotations in place.
The review is **not cutover authorization**. The separately reviewed,
[restartable image migration](../how-to/governed-credential-grants.md#migrating-protected-controller-and-runtime-images)
supports one root Deployment with already-enrolled Deployment companions across
its qualified namespaces, and one affiliated workspace grant in the root
namespace. Grant writers and budget TLS remain in the root namespace. Core and
Bridge can therefore migrate while retaining an enrolled `kars-sre` scope.
Every namespace/consumer identity, consumer template and replica intent (including
zero) is retained; all actual old consumers retire before fresh qualification
and writer restoration. An explicit `--companion-image <deployment>` review
permits an immutable, image-only update of an enrolled root-namespace companion,
including the Bridge web Deployment, while leaving
the root image and all other consumer configuration unchanged. It still retires
the root and all enrolled companions and requires fresh post-retirement budget
TLS rotation before requalification and restoration of retained grant intent.
It neither applies the remaining rendered resources nor advances Helm history.
Use this lifecycle for a qualified companion; an ordinary Bridge Helm upgrade
or direct image patch is not a substitute.

During fresh qualification, the controller can rotate an enrolled runtime's
control token and update its `services-credential-version` marker. Finalize
handles this transition before or after the qualification seal: it requires the
same namespace, sandbox, Deployment and control-Secret identities, a derived
`control-v1` hash matching the fresh epoch's material, zero runtime replicas and
Pods, and acknowledged withdrawal of grant writers. Only that marker may change;
all other template and execution intent remains fenced. The journal retains the
original retirement and seal (or pre-seal qualification), then durably records
the replacement binding before its namespace CAS. Lost acknowledgements resume
that transition, without rerotating keys, replaying retirement, changing replica
intent, or weakening the ordinary template digest checks. Unsupported material
layouts and unexplained changes remain refused.

For an activation-bound legacy budget TLS Secret missing Helm ownership, the
migration library's `prepareRootBudgetTlsRotation` prepares a single atomic
ownership-and-key-rotation JSON patch. It requires the validated migration receipt,
retired consumers, the exact zero/Pending root, unchanged original TLS revision,
and the original release-owned root and budget catalog. Conflicting release or
manager metadata and other owner references are refused. The patch tests the full
original metadata, UID, resourceVersion, type and data, retains unrelated fields,
and can add only this release's ownership while rotating to an unexposed matching
key. Persist the prepared patch and apply it once through the existing guarded
rotation path; never relabel a Qualified Secret or refresh a failed CAS's version.
Normal finalize binds the resulting TLS revision. A Pending full-chart render
still requires genuine publication ownership; for image-only migration, a
post-finalize server dry-run can validate the restored Qualified state without
claiming publication ownership or advancing Helm history.

Ordinary protected **full-Core upgrades remain refused**, including unchanged
controller templates and TLS bytes: Helm SSA can change the bound TLS Secret's
resourceVersion. The refusal examines desired **and historical** resources.
Image migration or same-key recovery alone does not authorize that write.

The bounded Kind `migrate-root-image publish-core` command now uses the maintained
`installLocalCoreChart` caller inside the existing migration journal. It is an
optional step **after retirement and fresh in-place TLS rotation, before
finalize**; see the [command sequence](../how-to/governed-credential-grants.md#protected-full-core-publication-on-kind).
It requires Helm 4.1.3 with Secret release storage, the same reviewed one-root,
multi-namespace topology, complete successful history (or explicitly fenced retained failures as
specified below), an explicitly SSA latest predecessor
(`apply_method: "ssa"` in the stored release), existing resource identities, and an
exact server-projected Core render. CSA, missing and unknown predecessor apply
methods are rejected before any publisher ownership/adoption write or Helm
launch. Helm's CSA-to-SSA managedFields conversion would advance live versions
separately from the pinned target; this path does not attempt that migration.
Check this prerequisite before choosing retirement or manual TLS rotation.
Root-namespace companions and all credentials, TLS, policy, schemas and
accounting bodies must remain unchanged; the root stays
at zero with precisely its reviewed target spec. This is not arbitrary chart,
policy, schema or topology migration. New resources, removals, unrelated adoption,
executable hooks, rollback, pruning and automatic cleanup are refused.

The maintained chart's existing `kars-default` ToolPolicy is the sole static-hook
exception: an unchanged successful release predecessor and identical live policy
are required. If its live release-name/namespace annotations are missing, a
journaled, UID/resourceVersion/full-annotation-tested CAS adds only those two
ownership annotations before snapshot freezing and launch ownership. Existing
conflicting ownership is never adopted; `--take-ownership` is never used. The
resulting live ownership and unchanged body are validated and journaled. A crash
before this metadata-only CAS or after it commits can resume the exact intent:
once its new version is observed, delayed copies of the original CAS are stale.
The policy becomes a UID/version-fenced ordinary resource, keeping its UID and
policy contents; no hook deletion, recreation or execution occurs. Prior hook
records remain in immutable release history. Other hooks, including dependency
Jobs, require a separately supported upgrade profile; they are never silently
skipped and governance must not be disabled to evade the gate.

**Datapath writer maintenance:** when the maintained witness is enabled, protected
publication journals its live Deployment UID, complete execution spec and original
replica intent, then pauses it with a UID/version/spec compare-and-swap. It requires
observed zero replicas in the Deployment and its owned ReplicaSets, no matching
Pods, and no HPA targeting the writer. The namespace is taken from the rendered
inventory, not from a cluster-specific setting. After quiescence, an acknowledged
version fence on the evidence ConfigMap invalidates already-sent witness PUTs;
existing runtime evidence is preserved. The immutable Helm snapshot keeps the
writer at zero through publication and postflight. Only durable `verified`
publication restores its original replica count, preserving the newly published
template. Resume handles lost pause, evidence-fence and restore acknowledgements;
finalize refuses an unfinished restoration. An ambiguous Helm outcome keeps the
writer paused and does **not** authorize retry or recovery.

**Publication and terminal proof:** a namespace ownership marker and durable
`publishing` journal entry precede the only Helm launch. Launch ownership is
consumed by an API compare-and-swap from `preparing` to `publishing`, so copied
preparing/prepared receipts cannot share the local lock's authority. The exact
private chart snapshot includes live UID/resourceVersion preconditions for every
resource; only the acknowledged ownership CAS's namespace metadata/version is
refreshed and journaled in a replacement snapshot before launch.
After normal Helm completion, the published manifest must match the complete,
duplicate-free snapshot inventory keyed by API version/kind/namespace/name,
with exact canonical object equality (including UID/resourceVersion). Helm's
resource sorting may change document order, not objects. Exact history/resource
checks and acknowledged version-advancing CAS patches fence all resource writes.
Every old and new Helm release Secret is also CAS-marked **immutable**: storage UPDATEs may omit
resourceVersion, so a version fence alone is insufficient. Secret immutability
rejects delayed data changes and attempts to remove immutability. Only after all
fences and rechecks succeed is `succeeded` durably journaled. Owner release records
`verified`; existing finalize then binds the final TLS revision and requires
fresh epochs, genuinely new Ready execution, retained grant restoration and
controller acknowledgement.

Historical release Secrets are append-only retained evidence. After their first
fence, future Helm attempts to supersede their status may fail, leaving older
immutable records labeled `deployed`; the newest successful revision is still
required. No record, UID, grant, exposed-key history or accounting liability is
removed to make publication succeed. This path does not authorize ordinary Helm
rollback, uninstall or history cleanup.

**Remaining interruption boundary:** Helm's
[`handleContext` and `releasingUpgrade`](https://github.com/helm/helm/blob/v4.1.3/pkg/action/upgrade.go)
run separately; cancellation or failed/pending history does not prove API-write
cessation. `preparing` can resume read-only preparation, namespace ownership
acquisition and the narrowly journaled static-policy metadata adoption;
`prepared` can launch only its unchanged snapshot and live preconditions;
`succeeded` can finish postflight and owner release without relaunch. Any error,
cancellation, missing fence acknowledgement or lost terminal journal save after
launch leaves **`publishing` ambiguous and refused**. Neither a delay nor process
exit alone permits recovery. Preserve its receipt, snapshot, history and namespace
owner; do not clear pending records, rotate again or qualify via another receipt.
No automatic unknown-writer recovery is provided.

**Explicit failed-publication recovery:** `publish-core --recover-failed-publication`
accepts only the original `publishing` receipt with an exact terminal **failed**
or **deployed** revision: the complete manifest must match its immutable snapshot,
with SSA and no hooks, and all prior release UIDs and contents must match the
recorded history. A deployed revision additionally requires the stopped witness
and every recorded resource UID and projected body to match before any recovery
write. Pending, absent, changed or incomplete records remain refused. Terminal
status alone never authorizes another launch or establishes publication success.

Before fencing, the same append-only journal records a fresh successor identity
and request. A namespace UID/version/annotation CAS transfers the refusal marker
to that successor without clearing Pending or the retirement protections; a copied
old receipt cannot reserve another successor. Every frozen resource must still
have its original UID and Helm ownership. Acknowledged resource-version fences
invalidate **all** old snapshot requests, and every release Secret, including the
failed revision, is made immutable. Complete history, snapshot, resource fences and
retired authority are rechecked before archiving the failed attempt in the journal
and preparing the successor. No old snapshot is replayed and no failed record is
rewritten or removed. The next attempt uses newly captured versions and the normal
writer pause, projection, launch and verification path. Original witness replica
intent survives a failure even if Helm already updated its stopped template.

Admission resource versions remain part of private authority. If publication or
its fences advance them, the publisher proves the complete frozen bundle's old
revision, unchanged UIDs and policy bodies, current observation/type-checking and
owned fences. It journals the actual new revision before an exact namespace CAS
updates only the bundle revision and corresponding retirement binding. The same
retirement attempt, captured Pods, exposed-key history, TLS baseline, scopes and
replica intent remain intact. Lost acknowledgements resume that recorded
transition; policy/spec/identity changes and foreign metadata remain refused.
Normal successful publication uses the same transition before ownership release,
and finalize requires the recorded current revision rather than fabricating old
resource versions. Installed Kind recovery completed this transition and retained
the failed release while preparing its successor; successor publication and full
customer acceptance remain unproven.

Frozen publication values are stored as chart defaults, not Helm's explicit
release overrides. Monitoring bootstrap provenance may read those defaults only
from the exact hook-free, dependency-free static publication chart, with the
single maintained file-rendering template and a resource file matching the stored
release manifest. Changed values, executable templates or unmatched manifests
remain refused; bootstrap Jobs are never rerun. Installed CLI31 proved this
provenance path and launched the hook-free successor. Revision 4 then failed on
Helm's readiness wait: Pending admission intentionally refused the retired root's
new ReplicaSet even though the Deployment had zero replicas.

Protected publication now uses hook-only Helm waiting, retains normal completion
and immutable storage/resource fences, then checks non-root rollout, service,
volume and CRD readiness before releasing ownership. The exact reviewed zero-root
spec is exempt only at this publication boundary; finalize still requires fresh
qualified Ready execution before restoring writers. This exemption checks the
live Deployment UID and complete server-defaulted spec against the reviewed
projection, while requiring the frozen manifest to retain its UID and zero
replicas. It does not compare the raw template's omitted API defaults against the
projected spec or permit actual spec drift. Readiness failures leave the
journal at `succeeded` and can resume checks without another Helm launch. Normal
installation waiting is unchanged. Installed CLI32 completed hook-free revision 5,
but post-Helm verification stopped before fencing/readiness: Kubernetes advanced
the stopped witness Deployment's revision annotation from 4 to 5. The other 248
resource projections matched. Verification now permits only that captured writer's
monotonic controller revision annotation change, checking its original recorded
value against the unchanged projection digest; all other metadata, spec and UID
checks remain intact. Explicit recovery fences this terminal, fully projected
revision before a fresh successor, retaining the original deployed release bytes
rather than replaying or relabelling it. Installed CLI34 durably published and
fenced revision 6, then exposed the raw-template/server-defaulted-spec readiness
comparison defect. CLI35 passed 418 affected migration/publication tests and
resumed the same revision and attempt to `verified`, without another Helm launch.
Maintained finalize then completed fresh execution qualification and grant
restoration. These installed migration results do not establish credential-use
or full Kind customer acceptance.

After an image-only migration completes, `reviewCompletedRootMigration` rechecks
its full live qualification, retained replica intent, target specs and restored
grant authority without cluster writes or journal changes. It returns current
Deployment UID/resourceVersion fences for persisting already-qualified images in
Helm. A protected Bridge no-op upgrade must supply these as
`bff.privateContinuity.{uid,resourceVersion,replicas}` (and the equivalent Teams
fields only when Teams is protected), while selecting the already-qualified
image. The chart preserves the exact live template and epoch; stale fences or
execution changes refuse publication. This review does not authorize another
migration, TLS rotation, scaling change, or replay of a consumed attempt.

Lost fence/ownership acknowledgements resume only that recorded successor and
request; missing or conflicting proof stays owned and refused. This option cannot
be combined with `--replace-preparing-request`. Neither source tests nor a single
successful recovery transition establish full Kind customer acceptance.

---

## 1. Two ways to move forward

| Command | What it does | Use when |
|---|---|---|
| **`kars upgrade`** | Full, failsafe release migration: detect version → record rollback point → import the target release's signed images into your ACR → `helm upgrade --atomic` → rolling restart → verify. | You want to move to a **published GitHub release** (the normal path). |
| **`kars up --upgrade`** | Fast Helm-only re-run against an existing cluster. Assumes your ACR **already holds** the target images; re-applies the chart + RBAC. | You manage images yourself (custom ACR pipeline) and just need to re-apply the chart. |

For almost everyone, **`kars upgrade`** is the right command — it owns the image
import and the rollback point, so the cluster never lands half-migrated.

---

## 2. Upgrade to the latest release

```bash
# Preview first — shows from→to and exactly which images would be imported
kars upgrade --dry-run

# Apply
kars upgrade
```

What happens, step by step:

1. **Connect & sanity-check.** Pulls AKS credentials from the cached context
   (`~/.kars/context.json`) and confirms a `kars` Helm release exists.
2. **Resolve target.** Defaults to the latest GitHub release; override with
   `--to <tag>`.
3. **Version guard.** If you're already at the target, it exits cleanly (use
   `--force` to re-run). If the cluster is **newer** than the target, it refuses
   to downgrade unless you pass `--force`.
4. **Import images** from `ghcr.io/azure` into your ACR — both the immutable
   version tag (e.g. `:v0.1.18`) and `:latest`. If a **required** image fails to
   import, the command aborts **before** any cluster change is made.
5. **`helm upgrade --atomic`** — on any failure Helm auto-rolls-back the release,
   so you can't end up half-upgraded.
6. **Rolling restart** of the controller, router, and sandbox workloads onto the
   new images.
7. **Verify** workload health and print a from→to summary.

Pin a specific release for reproducibility:

```bash
kars upgrade --to v0.1.18
```

Skip the seven multi-runtime adapter images for a faster control-plane-only
upgrade (OpenClaw + BYO remain runnable):

```bash
kars upgrade --skip-runtime-images
```

---

## 3. Verify

```bash
kars status                     # control plane + sandbox health at a glance
kubectl get pods -n kars-system # controller + router rollout status
kubectl get karssandbox -A      # per-sandbox phase
```

The upgrade prints `Cluster healthy on the new release` when the controller,
router, and sandbox deployments report Ready. If it instead warns that some
workloads aren't Ready yet, give them a moment and re-check `kars status` — image
pulls on fresh nodes can lag the Helm completion.

---

## 4. Roll back

Every `kars upgrade` records the prior Helm revision, so reverting is one command:

```bash
kars upgrade --rollback
```

This runs `helm rollback` to the previous revision, restarts the workloads, and
re-verifies health. Use it when an upgrade changed **chart templates, Helm
values, or CRDs** and you need to undo that.

> ⚠️ **What rollback does and doesn't revert.** kars workloads pin the
> **`:latest`** image tag (a deliberate convention — the controller always
> tracks `:latest`). `kars upgrade` imports the new release over `:latest` in
> your ACR, so a `helm rollback` reverts the **release revision** (chart, values,
> CRDs) but the rolling restart still pulls `:latest`, which now points at the
> upgraded images. To revert the **running image bits** to an earlier version,
> re-import that version explicitly:
>
> ```bash
> kars upgrade --to v0.1.17   # re-imports v0.1.17 over :latest and rolls workloads to it
> ```
>
> `kars upgrade` imports each release's immutable version tag alongside `:latest`,
> so the exact prior bits remain available in your ACR to roll back (or forward)
> to by version.

> **Data note.** Rollback reverts the **control plane** (controller, router,
> CRDs, chart config). It does not undo data-plane side effects that
> already happened (audit-chain entries, Memory Store writes, mesh messages) —
> those are append-only by design.

---

## 5. CRD / schema changes across releases

The CRD surface is served at `v1alpha1` and may change between minor releases.
`kars upgrade` applies the chart's CRDs as part of the Helm upgrade. When a
release changes a CRD schema, the change is called out in
[`CHANGELOG.md`](../../CHANGELOG.md); additive fields are backward-compatible, and
any field with a breaking change is flagged there. Review the changelog entry for
the target release before upgrading a cluster with hand-authored CRs.

---

## 6. See also

- [`kars upgrade` CLI reference](../cli-reference.md#kars-upgrade)
- [Image versioning](image-versioning.md) — the `:latest` vs pinned-tag model
- [Helm packaging](helm-packaging.md) — chart layout and release
- [GitOps](gitops.md) — reconciling the same changes declaratively with Argo/Flux
