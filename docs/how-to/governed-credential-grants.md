<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Governed credential sources and operator stores

This additive contract does not require Bridge. Direct credentials and the
existing ten-key `credentialsRef` v1 flow remain unchanged unless explicitly
selected for migration.

## Mission delivery receipts

The Core mission dispatcher does not need credential-use or projection authority
to acknowledge a credential-bound Mission. Its admission exception permits only
`run-ack` and `run-completed` annotations for the already-requested run, with the
Task spec, status, other annotations, labels, ownership and finalizers unchanged.
It cannot create a credential-bound Task, change bindings, launch work, alter the
run request or remove receipts through this exception. Other principals cannot
use the exception. Existing controller-only bundle and GitHub enrollment checks remain.

A retained `uncertain` attempt after an acknowledgement failure is not successful
execution or permission to resend the assignment. Preserve it, repair the reported
admission or transport failure, and use a fresh reviewed workflow attempt.

## Recovering an image-only root regression

`kars credentials grant recover-root-image preview --private-root <namespace>
--image <repository>:latest@sha256:<digest>` reviews a narrowly scoped rollback.
The supplied immutable image must reconstruct the **entire** template digest
in the existing sealed qualification. The live root, controller identities,
privacy bundle, namespace proof, budget TLS and grants must still match; the
only protected scope and consumer must be that root. Unknown private executions,
additional affiliated scopes, mutable images and other template changes refuse
recovery.

Save the metadata-only preview and pass it to
`kars credentials grant recover-root-image apply <reviewed-recovery>`.
Apply repeats the review, server-dry-runs and UID/resourceVersion/spec-tests the
image replacement, then verifies the actual restored rollout and private
continuity. It does not rewrite grants, epochs, credentials or qualification
proofs, and it does not roll back Helm release history. This is not recovery from
suspected credential compromise or authorization for a new controller version:
ordinary protected-root upgrades remain blocked.
If the controller quarantined that same intact proof by changing only
`private-state` to `Pending`, native admission blocks replacement startup.
Recovery therefore temporarily scales the root to zero, retaining Pending until
every old privileged Pod is gone. It verifies the original sealed proof, exact
restored template and original keys against that genuinely retired execution,
then UID/RV-tests restoration of the startup marker. Only then does it restore
the sealed replica count and verify actual ready execution and full continuity.
Missing proofs, altered epochs, changed keys or any other mismatch still refuse.
A fresh preview resumes an interrupted image, retirement, marker or replica
restoration. Image-only predecessor Pods must have the same actual root lineage
and otherwise identical execution; they are approved only for retirement, never
as replacements. A stopped root remains stopped if a proof or race check fails.

## Migrating protected controller and runtime images

`recover-root-image` is not an upgrade mechanism. The separate bounded
`migrate-root-image` path supports **one root Deployment, already-enrolled
Deployment companions across its qualified namespaces, and one affiliated
workspace grant in the root namespace**. This includes Core and Bridge in
`kars-system` with an enrolled SRE Deployment in `kars-sre`, without unenrolling
SRE or removing Bridge. All grant writers and the budget TLS identity must remain
in the root namespace. The complete cluster namespace/grant inventory and every
enrolled namespace/consumer identity are retained; additional affiliated grants
or non-Deployment consumers are rejected, not omitted. The historical
`--sealed-root` recovery variant remains limited to one namespace and its root.
Companions are captured from the existing qualification, including proven scopes
enrolled after the original root seal, never added by migration. Their full specs and replica intent are durably bound:
writers are withdrawn, all reviewed execution is genuinely retired, and all
Deployments remain at zero through image cutover and fresh budget rotation.
Finalize restores each companion's captured replica count (including zero) in
the fresh epoch and verifies new Ready Pods before restoring writers. Companion
configuration and identities cannot change, apart from fresh epoch metadata and
restoration of captured replica intent. An explicitly selected root-namespace
companion may refresh only existing container/init-container image fields, with
immutable old and new `@sha256` references. Pass repeatable `--companion-image <deployment>`
to `review-root-migration`; the Deployment must already be enrolled in the root
namespace and present exactly once in the render. The review displays container
names and old/new references, plus original and target full-spec/template digests.
Unselected consumers cannot change, including replica-only deltas. Commands,
environment, service accounts, volumes, selectors, replicas, and container names
or ordering are retained exactly. A companion-only refresh may keep the root spec
entirely unchanged, but still requires genuine retirement, fresh TLS rotation and
qualification of the root and all companions before restoring retained grants.
The receipt binds original companion objects and target specs; each image CAS has
a durable updating/updated record and UID/resourceVersion/full-spec tests, with
scope, zero-Pod, root and TLS drift checks before and after the write. Target
companion template digests enter the new retirement/activation binding, never the
old qualification. Lost image/pause/restore acknowledgments resume from the same
receipt without repeating rotation.

A runtime that reissues epoch-bound control material can also change its
`kars.azure.com/services-credential-version` template annotation after the fresh
root seal is written. This is a real template change, not an ignored epoch field.
The current migration cannot settle that control-material transition: finalize
fails closed and keeps grant writers withdrawn, including for a zero-replica SRE
Deployment. Repeating finalize while the sealed digest differs will not repair
it. Preserve the receipt, seal and rotated material; do not restore an obsolete
version annotation, edit the receipt, or reseal an unreviewed live template.
Such a transition requires separately guarded recovery before this topology can
be considered qualified through the migration flow.

Existing root-only receipts remain supported; historical already-rolled recovery
with `--sealed-root` remains restricted to the root consumer alone. The server-projected target spec may change
the controller container's immutable image and/or the literal values of its
**existing** `HERMES_RUNTIME_IMAGE`, `SANDBOX_IMAGE`,
`INFERENCE_ROUTER_IMAGE`, and `KARS_CONFORMANCE_RUNNER_IMAGE` environment entries.
Controller and runtime references must remain immutable `@sha256` references;
changing the runner likewise requires immutable old and new references. An untouched
runner entry (including the public adoption default) is preserved during other
image migrations. Evaluation runner-only changes use this same retirement, TLS
rotation, protected publication, and fresh activation protocol; they do not change
workload images. When publishing
Core values, set `evals.runnerImage` and retain the exact original
`evals.runnerImageBaseline`: the chart still verifies that baseline against sealed
adoption and rejects unrelated settings or funding changes. A governed router
image change must also update the **existing** `KARS_INFERENCE_BUDGET_ROUTER_DIGEST` literal to that
image's digest; both old and new pins must match their respective router images.
A missing, stale, independent or indirectly sourced pin is rejected before
writes. This does not authorize changing broker enablement, funding or contracts.
Runtime- or runner-only cutover may leave the controller
image unchanged, but still retires and freshly qualifies that controller
execution through the same migration path. A first adopted-SRE router upgrade
may introduce `KARS_INFERENCE_BUDGET_FIRST_INSTALL_ROUTER_DIGEST` only by copying
the original qualified root's unique literal governed router pin, matched to its
immutable router image. Subsequent upgrades must preserve that baseline unchanged;
immutable first-install intent and adoption verification still apply. Other
environment additions, deletions, renames, reordering, duplicates, `valueFrom`
image changes, and unrelated environment changes are rejected. Commands, funding settings, replicas,
checksum annotations, selectors, accounts and other containers cannot
change through this path. Image retirement/finalize alone does not apply the rest
of the rendered release or advance Helm history. The optional protected Kind
`publish-core` step below supplies bounded full-Core publication between them.
Server-side **dry-run only** projections use `--force-conflicts` so image-field
ownership retained by a legitimate `kubectl-patch` recovery does not block
review. This does not transfer live field ownership or relax the review.
Actual migration writes never force an apply: they retain exact
UID/resourceVersion/full-spec JSON Patch tests.

Operator sequence (run only against the intended cluster, after maintenance
approval; these commands interrupt controller and enrolled companion availability):

```bash
kars credentials grant review-root-migration rendered-core.yaml \
  --private-root kars-system --release kars --context <context> > root-review.json
kars credentials grant migrate-root-image retire root-review.json rendered-core.yaml \
  --receipt root-migration.jsonl --context <context>
# If interrupted, retain the receipt and resume the same operation:
kars credentials grant migrate-root-image resume-retirement root-migration.jsonl \
  --context <context>
# After the required operator-managed TLS rotation described below:
kars credentials grant migrate-root-image finalize root-migration.jsonl \
  --context <context>
```

For an already-enrolled dispatcher image-only refresh, review the exact render
with the explicit companion selection before the same retire/rotation/finalize
sequence (use the actual enrolled Deployment name):

```bash
kars credentials grant review-root-migration rendered-core.yaml \
  --private-root kars-system --release kars --context <context> \
  --companion-image kars-mission-dispatcher > root-review.json
```

This does not publish a Core release, advance Helm history, or authorize recovery
of an unrelated historical failed Helm revision. Full-Core publication guards
remain unchanged. Existing receipts without companion targets still permit only
unchanged companions; changing the render or adding image authority requires a
fresh review before retirement, not editing a receipt. Companion selection cannot
be combined with historical `--sealed-root` recovery.

If an image-only rollout already invalidated the sealed root, the same migration
can retire the **actual current execution without reverting images**. Supply an
operator-retained root Deployment JSON whose UID and template digest match the
existing seal:

```bash
kars credentials grant review-root-migration rendered-current-core.yaml \
  --private-root kars-system --release kars --context <context> \
  --sealed-root retained-qualified-controller.json > root-review.json
```

The witness is bounded to 1 MiB and does not establish current qualification.
Recovery can review the retained seal after the controller has revoked the
namespace to `Pending`; all other sealed namespace bindings must still match.
Ordinary qualification continues to require `Qualified`. Recovery never restores
reader annotations to bypass revocation. Namespace version-only reconciliation
can continue during review: the inventory binds namespace identity, annotations,
labels, owners, finalizers, and spec, while writes retain fresh resource-version
conflict checks. A grant may record a newer budget namespace resource version
than the sealed predecessor; this alone is not a TLS identity change. Budget
namespace name/UID, Secret name/UID/resource version, and key digest must still
match. Changed authority or configuration still requires re-review.
Only the existing immutable controller/runtime-image delta is permitted; the
server-projected render must exactly retain the observed current Deployment
spec. Additional configuration, topology or image changes require a later
migration. Recovery reviews include historical and observed Deployment objects;
protect these files like rendered configuration. Use the same retire/resume/
rotation/finalize sequence above. Historical authority is verified separately
from actual Pod ownership, zero-replica retirement and fresh qualification.
No reader annotation, key, seal or epoch is borrowed to authorize changed code.

Retire creates an exclusive, mode-0600, append-only, fsynced local operation
receipt **before withdrawing writers**. It retains the original seal, exact
grant intent, complete scope/grant inventory, projected target and actual
retired Pod identities/ownership/template evidence. Keep this file and its
backups private and durable; do not edit it or start a second migration. A local
lock rejects concurrent writers; after a process crash, remove a stale
`<receipt>.lock` only after verifying that its recorded process has stopped.
An incomplete/torn journal, changed identity, unexpected inventory or failed
compare-and-swap is an explicit recovery boundary, not permission to bypass
qualification.

Writer withdrawal reuses ordinary grant retirement: it waits for the controller
to acknowledge the exact zero-writer spec, verifies all owned Roles/bindings are
absent, and verifies removal of the captured writer guards. The still-sealed
old root and its captured companions are UID/resourceVersion/full-spec fenced
to zero. Every private Pod must have an actual reviewed owner; all reviewed
execution, including temporarily non-consuming companion Pods, must finish
retirement within a bounded wait. The original seal is verified against that genuinely retired execution
before Pending is installed. The target spec is applied only at zero, while
Pending forbids replacement startup. A new v1 retirement retains previous
captured/exposed-key history and records the post-retirement baseline; it is
**not** a qualification or permission to run new code with an old seal.

When the budget broker is enabled, rotate its TLS private key **after** this
baseline using the operator's approved key-generation process. Update the
existing TLS Secret and `kars-inference-budget-contracts` public `ca.crt` in
place, preserving both UIDs and all other contract data. The public CA must
verify the new certificate. Do not reuse a previously exposed key, borrow an
existing key, change accounts/charges/caps, or perform a Helm update that changes
the paused Deployment's reviewed checksum/template. Image-only retirement and
finalize never create, read, copy or write TLS private-key bytes. The optional
full-Core publisher privately snapshots rendered, release-owned Secrets. An
external budget TLS Secret remains operator-managed and is not imported into the
Helm snapshot. A no-budget installation has no TLS rotation prerequisite.

Finalize checks the new key and public CA, reuses ordinary private activation
to mint fresh epochs in every enrolled namespace and a new complete seal on the
actual new execution, then restores
the original grant spec with only its `privateActivation` updated. It requires
the original replica count of genuinely new, reviewed, Ready Pods and final
controller acknowledgement of writer intent. Inventory checks fence each
activation/publication write. Interrupted activation, rollout, grant publication
or final receipt persistence resumes with the same `finalize` command. No
readiness timeout is treated as acceptance.

During qualification, a new controller may finish deleting an unrelated terminal
sandbox namespace. Finalize can resume after that namespace disappears only when
its sealed identity is absent from every reviewed activation, grant and writer
guard, and its sealed metadata carries no private fields, reader guards or
metadata finalizers. The original journal inventory is retained. Terminating
namespaces, missing protected scopes, additions, UID replacements and changes to
surviving namespaces still fail closed; this does not authorize namespace cleanup
or alter failed execution records.

Before the new v1 retirement replaces the old seal, exact-image recovery may
still restore the original image under its existing strict checks; the original
grant intent remains in the migration receipt, and must not be reconstructed
from an empty writer list. After replacement, recovery must complete ordinary
fresh-key activation, not reinstall or edit the retained old seal. Unexpected
authority changes require explicit operator recovery rather than automatic
rollback. CLI unit/type/lint validation is not deployed verification or full
Kind customer acceptance; ordinary protected-root upgrade guards remain intact.

### Protected full-Core publication on Kind

Before planning full-Core publication, confirm the latest deployed release was
already applied with SSA: its stored release must explicitly contain
`apply_method: "ssa"`. CSA, absent and unknown apply methods are not supported;
Helm's client-side field-manager conversion would invalidate pinned versions.
The publisher rejects these cases before any of its mutations. It does not
rotate TLS itself or provide a CSA migration. Check this prerequisite before
retirement/manual TLS rotation, not after changing credentials.

After the same retirement and approved fresh TLS rotation, **before finalize**:

```bash
kars credentials grant migrate-root-image publish-core root-migration.jsonl \
  deploy/helm/kars --cluster-name <existing-kind-name> --release kars \
  -f complete-core-values.yaml --set schemaHook.enabled=false
kars credentials grant migrate-root-image finalize root-migration.jsonl
```

The maintained installer pins `kind-<existing-kind-name>` and `kars-system`;
finalize inherits that recorded context and rejects an explicit conflicting one.
No cluster creation, bootstrap, image build, ordinary unjournaled Helm fallback
or hook execution occurs. Supply complete values with the already-live public
CA and reviewed image references. Preserve the original TLS ownership model:
Kind uses an existing operator-managed Secret, so leave the chart's
`inferenceBudget.tls.certificate` and `privateKey` values empty. Do not copy the
rotated private key into those fields or change an external Secret into a
chart-managed one. The external Secret remains outside the Helm manifest and
snapshot; retirement/finalize retain its rotation checks. Values must retain the
reviewed root checksum/template, not generate
another rollout. The publication forces root replicas to zero; finalize alone
restores the sealed replica intent. Pending continuity also validates every
enrolled, paused Deployment companion across the root and retained private
namespaces (including SRE) against its retained parent UID, namespace authority,
and the complete retirement binding; it does not assume Core runs alone.
Missing, extra, replaced, or still-running enrolled consumers are refused.
Supply the retired controller's current UID/resourceVersion in
`controller.upgradePrecondition`, rather than reusing a stored pre-retirement
fence.
The first-install schema hook must be disabled for upgrades, without disabling
governance. An unchanged existing `kars-default` policy is converted from its
static Helm hook into an ordinary fenced resource. Its existing controller cleanup
finalizer is preserved. If Helm trimmed the inline profile's final newline, the
live profile bytes and digest are retained only when adding that one newline
leaves the parsed YAML unchanged; other policy or finalizer differences are
refused. Missing live Helm ownership annotations are added first by a journaled
UID/version-tested metadata-only CAS;
exact live ownership is checked before freezing the snapshot. The recorded
adoption resumes safely before launch, including a lost CAS acknowledgement;
conflicting owners or changed policy identities/bodies are refused. No broad
`--take-ownership` is used. The complete maintained monitoring admission bootstrap
may be omitted only when its hook definitions match a successful retained
predecessor, monitoring values have remained unchanged, its resource inventory
matches, and every projected monitoring resource is unchanged. Those Jobs are
never executed or included in the snapshot. A frozen predecessor's monitoring
values live in its stored chart defaults when Helm recorded no explicit overrides.
Those defaults prove continuity only for the exact hook-free static publication
chart: one maintained file-rendering template, no dependencies or schema, and one
resource file matching its stored release manifest. Other chart shapes, overrides,
changed defaults or mismatched resources cannot provide that fallback. Unknown,
changed, incomplete or unsuccessful hooks remain refused. Monitoring dependency CRDs retain their
separate bootstrap lifecycle: read-only schema qualification checks them without
adopting them into the Core release. New/removal resources and unrelated
ownership changes are rejected. Schema preflight requires compatible,
already-installed schemas. Resource projections use each object's actual
namespace. Protected resource bodies and root-namespace companions must remain
unchanged, except that the datapath settings advance to the next release revision
while capture scope and configuration remain identical. Recovery may skip only
explicitly fenced failed revisions when those attempts left older settings intact. This is
not a general chart migration.

Publication keeps the reviewed root at zero replicas under Pending admission.
It does not wait for that root's new ReplicaSet, which admission correctly denies
until qualification. After normal hook-only Helm completion and durable immutable
fencing, the publisher checks non-root workload, service, PVC and CRD readiness
before releasing ownership. A failed readiness check resumes from `succeeded`
without replaying Helm. Finalize still requires actual new qualified Ready Pods
before restoring writer authority; ordinary install readiness is unchanged.

The command requires inspected Helm 4.1.3 and forces `HELM_DRIVER=secret`. It saves
an exact private minimal chart beside the receipt as `<receipt>.core-<attempt>-…`.
**Unlike image-only receipts, this snapshot may contain private TLS keys and
other credentials.** Files are mode-0400, directories mode-0500; retain the
receipt and snapshot together on private durable storage, never in Git or public
artifacts. Resume uses the snapshot, not changed source/value files. Missing,
altered or extra files and stale live UID/version preconditions fail closed.
Do not remove snapshots while a publication is unresolved; abandoned pre-launch
snapshots may also contain private bytes and require deliberate secure retention
or cleanup once their non-launch status has been established.

Publication owns a namespace refusal marker and journals `publishing` before
Helm runs. A one-time API compare-and-swap consumes launch ownership, preventing
copied preparing/prepared receipts from launching concurrently. Its acknowledged
namespace version is recorded in a replacement private snapshot before launch;
both snapshots remain retained. Normal exit plus exact resource/history verification, acknowledged
resource-version fences and **immutable old/new release Secrets** are required
before durable `succeeded`. Historical bytes and UIDs remain retained; future
supersession attempts can leave older immutable records labeled `deployed`.
Rollback, uninstall, pruning and pending-record removal are not recovery tools.

Rerun the identical `publish-core` command for `preparing`, an unchanged
`prepared`, or durable `succeeded`; it never relaunches a completed publication.
If rendering failed before a snapshot or launch and repaired immutable chart or
values paths are needed, explicitly add `--replace-preparing-request` to
`publish-core`. This is allowed only for the same retired migration in
`rotation-required` with publication exactly `preparing`, no recorded snapshot
or launch outcome, unchanged release storage and intact owned marker, retirement,
rotation and withdrawn-writer fences. It appends the new request to the same
journal and preserves the attempt; it does not clear ownership, replay retirement
or rotate again. The flag is refused on an initial publication or after snapshot
preparation. A witness already stopped during preparation may remain stopped:
replacement first verifies its original identity/spec, zero replicas, no Pods or
active autoscaler, Helm ownership and acknowledged evidence fence. Its original
replica intent and fence stay in the journal; published/restored or unknown witness
outcomes still refuse replacement. No witness write is performed by replacement.
Without the flag, exact-request matching remains mandatory.

When an adopted SRE policy already uses a controller-selected cluster default,
retain that live primary pair in the replacement values with the original
`sre.modelRouteBaseline`; do not overwrite it with stale bootstrap model values.
The Kind profile retains authenticated, UID-enrolled HTTPS provider checks. A
sealed first-install grant of exactly `unboundedTokens: true` does not require a
selected-route spending contract; bounded or monetary funding still does. This
does not bypass runtime funding authority, usage accounting or provider limits.
GitHub credential admission policies render Kubernetes' native match defaults
explicitly so their atomic `matchConstraints` can remain unchanged under SSA,
without forcing ownership or weakening validation.
`verified` proceeds to finalize. Any uncertain launch/terminal save or fence
acknowledgement leaves `publishing` refused, even if Helm history says failed or
a subprocess has stopped. Preserve Pending and ownership; do not use an older
receipt to qualify, clear the marker, restamp an epoch or rotate again. A stale
local journal lock may be removed only after its owning process has stopped;
this **does not** resolve an ambiguous Helm writer.

For an exact terminal **failed**, or completely projected **deployed**, SSA revision, explicitly rerun `publish-core` with
`--recover-failed-publication` and the same receipt, context and release. This also
supports a fenced `succeeded` publication whose readiness needs a chart or values
repair: the deployed revision and immutable history must still match, and the
witness remains stopped. Ordinary readiness retries without this flag keep the
same revision and never relaunch Helm. Repaired
chart/values paths may be supplied when starting recovery; subsequent resumes must
keep that recorded request. The command reserves a fresh successor under the
namespace marker, checks the failed manifest against the immutable snapshot,
and accepts only the frozen, original-publication or reserved-successor admission
fence during reservation. The complete bundle still requires unchanged bodies,
UIDs and current observation; after reservation every admission fence must match
the successor. It fences every frozen resource version and makes the complete matching Helm history
immutable. It preserves failed bytes, UIDs, snapshots, retired authority and
original witness replica intent. Admission-policy metadata version changes are
rebound only from the complete frozen bundle with identical policy bodies and
UIDs, current observation and owned fences. The new revision and all enrolled namespace snapshots are journaled before
exact namespace updates. Every enrolled scope advances together, with partial
updates resumable under the same identity and metadata fences; retirement
attempts, captured Pods and TLS/key history are not reset. The same transition applies after normal publication, and lost
acknowledgements resume without replaying Helm. Only complete rechecks allow a
fresh snapshot and launch. A deployed revision must additionally match every
recorded resource UID/body and have its witness stopped before recovery writes.
Only the stopped witness and kube-state-metrics Deployment's monotonically
advanced Kubernetes revision annotation may be compared using that Deployment's
captured original value: the witness capture or the metrics server-projection
record, not the chart (which may omit controller-managed annotations). Existing
metrics receipts without that captured revision retain exact comparisons. This
permits the reviewed metrics resource change to create a new ReplicaSet; all
UIDs, other metadata and spec fields remain pinned.
Other Deployments retain exact metadata comparisons. The original deployed history is preserved, not
relabeled as failed or treated as customer acceptance. Pending, missing, changed
or incompletely projected outcomes remain refused; the flag is not a force/retry
bypass and cannot be combined with
`--replace-preparing-request`. Keep using the same command to resume a lost recovery
acknowledgement; never clear the owner or replay the failed snapshot. See the
[terminal proof and interruption boundary](../operations/upgrades.md) for the
resource/release-storage distinction. Installed API-server fence behavior,
actual Core profile compatibility, controller convergence and end-to-end Kind
acceptance still require authorized live validation; host-only fixtures are not
those acceptance results.

For a historical failed SSA revision from the ordinary monitoring admission hooks,
`publish-core --retire-historical-monitoring-hooks` is a separate, explicit operation
under an unpublished, genuinely retired root after TLS rotation. It requires the
seven known service-account-only admission bootstrap resources and default ToolPolicy,
an exact successful later hook intent, and fresh absence of the bootstrap Jobs,
ServiceAccount, RBAC and every Pod using that account. Unknown hooks, independent
credentials, pending/latest failures and changed history remain refused. No hook is
executed and no workload, account or release record is deleted.

Before mutation, the receipt durably binds the complete history and failed/successor
identities and digests. A UID/resourceVersion-tested patch preserves the failed Secret's
entire data and failed status, adds its proof annotation and makes it immutable.
Resume the same flagged command after an interrupted acknowledgement; finalization
refuses incomplete retirement. Subsequent publication journals verify the persistent
proof and repeat the live authority checks rather than importing old recovery state.
Do not combine this flag with either other recovery option. This does not authorize
monitoring changes or establish installed customer acceptance by itself.

The normal protected publication can separately apply a requested
`monitoring.kube-state-metrics.resources` adjustment. Only positive CPU/memory
requests and limits on the existing single-container kube-state-metrics Deployment
are eligible. The server dry-run validates the resource quantities; the projected
Deployment must otherwise match its live body exactly, including probes, image,
ServiceAccount, selectors and replicas. All other monitoring configuration and
successful admission-hook intent remain pinned. The changed resources are captured
in the same immutable publication snapshot and verified after Helm applies it;
there is no separate patch of the Helm-owned Deployment. Kind beta uses its own
values override, leaving production defaults unchanged.

## Authority

Core chart upgrades retain the running controller's published privacy-verifier
Service selector instead of resetting it to `unavailable`. Fresh or missing
Services still start closed. Retention requires the existing release ownership,
fixed private endpoint and canonical selector; changed ownership, malformed
selectors and failed reads require operator review. This does not qualify an
endpoint: clients still verify its controller-issued descriptor and TLS identity.

Bridge operators can select an already configured provider's model through
Configuration → Model catalogue. This uses the enrolled controller-settings
store and preserves the existing endpoint, identity and credential references;
a Helm-configured provider does not need to be reconnected as an additional
provider to change its model. Switching to a different default requires the
previous provider to be registered separately first; otherwise Kars rejects the
switch rather than discarding its only endpoint/credential binding. Bridge does
not read an unenrolled bootstrap Secret to bypass that requirement. A cluster
administrator can connect that provider through the normal additional-provider
flow, using its existing credentials or Workload Identity as appropriate.

Ordinary provider, credential and standing-model rollouts do not require a new
permission grant. For an already reviewed, controller-owned runtime Deployment,
the projector records reconciliation in a protected namespace annotation. The
credential annotations `inference-budget-ca-version`, `inference-providers-version`,
`services-credential-version`, `sre-private-credential-uid`, and `sre-tls-expiry`
(under `kars.azure.com/`) may differ from its qualified template. Model transitions
may change only the literal values of agent `KARS_MODEL`, OpenClaw
`OPENCLAW_MODEL`, and router `AZURE_OPENAI_DEPLOYMENT`. Those values must match
both the controller-rendered workload and the current same-namespace
InferencePolicy primary. Environment names, presence, ordering and other fields
remain pinned; duplicate model entries and `valueFrom` substitutions are refused.
The receipt binds the privacy epoch, original template, current complete template
and live Sandbox UID/generation. All other pod spec fields, labels, annotations
and consumer identities remain pinned. Initial upgrade recovery may use an owned
ReplicaSet whose full template matches the original qualification; subsequent
rollouts use the protected receipt without retaining old history. Legacy
credential-only receipts still authorize unchanged model structures; the first
model transition requires the exact reviewed ReplicaSet to establish its model
structure binding. A malformed receipt or unrelated workload change still fails
closed; this is not an exemption for controller image or workload-spec upgrades.
Reconciliation and router acknowledgement are asynchronous: a saved selection is
not evidence of applied routing or successful inference.

Copilot sign-in stores its authentication; **Next** opens model review without
saving. **Save model selection** then publishes the chosen models and closes the
wizard. **Edit models** reuses that stored
sign-in. Saving a selection does not promote the provider. The catalogue retains
identical deployment names on different providers as separate choices, and a
default-provider switch preserves the registered previous provider and any
independent Foundry project connection.

Inference-budget and
retention-policy mutations require Admin. Role membership is managed through
the identity provider and deployment role map, not edited in Bridge.

Private activation compares each live consumer against its reviewed owner
template. Kubernetes' two standard, admission-injected 300-second
`NoExecute` tolerations do not make that execution different. The exact
`node.kubernetes.io/memory-pressure` `Exists`/`NoSchedule` toleration is also
recognized when the reviewed template requests or limits positive CPU or memory,
including init containers or supported Pod-level resources. This shared QoS
rule applies to non-root consumers as well as the root; it does not grant
non-root consumers the AKS root-only admission fallback. Zero or unrelated
resource requests, changed tolerations, duplicate entries, explicit wildcard
policies and every credential,
container and host-authority change still require exact review; custom
admission mutations are not silently ignored.

A Workload Identity opt-in label alone does not turn an exactly matching
non-root consumer into the root controller, even in the root namespace.
Mandatory root replay is selected through the pinned Deployment's owning
ReplicaSet lineage. The root still requires actual Workload Identity admission
when opted in; non-root execution changes still fail ordinary exact review and
do not gain the root-only fallback.
Before returning the pinned root Deployment, mandatory replay is also enforced
regardless of the traversal path. A direct Pod-to-Deployment owner reference
cannot substitute for the supported root ReplicaSet lineage.

If the root admission fallback rejects a consumer, the CLI reports only fixed
boolean execution-comparison categories and whether its namespace/Deployment
match the root. These use the same normalized comparison as enforcement;
no Pod values, environment names, credentials, images or hashes are emitted.
The original refusal remains authoritative. This diagnostic does not prove
which admission plugin ran or turn a rejected consumer into an approved one.

`KarsCredentialGrant/workspace` is a **metadata-only**, namespaced operator
delegation. It pins the workspace UID, writer ServiceAccount UIDs, permitted
agent key names, and each enrolled integration Secret's exact name/UID/purpose.
There are no credential values in the CRD. The operator ClusterRole is unbound;
the running Bridge cannot author or widen its grant. An installing operator may
explicitly select the Bridge chart's `workspaceEnrollment.enabled=true` to create
a UID-bound **zero-writer, zero-store** workspace grant after Core installation.
This opt-in uses the Helm caller's existing authority, grants no BFF permissions,
and leaves readiness to the controller. It refuses adoption or overwrite of
expanded grants. Disable this option before separately reviewed private
enrollment; the resource is retained on disable/uninstall, not implicitly
revoked. See the [Helm enrollment instructions](../../bridge/deploy/helm/kars-bridge/README.md#credential-free-workspace-enrollment).

Core creates source-only writer Roles behind fail-closed admission. The
parameter-independent source boundary continues to restrict Secret creation
even while a grant is being deleted. Native `resourceNames` entries are exact
names, never wildcard patterns. Values remain Opaque Kubernetes Secrets.
Credential-writer match conditions split authorization checks across ANDed
expressions to stay within Kubernetes' two-check-per-expression CEL cost limit.
This preserves the writer, projector and manager predicates; it does not grant
additional authority or disable admission when a settings store is empty.

When core attaches a source's ownership metadata, it records
`status.sources[].ownershipFromResourceVersion`. Together with that entry's
current UID, `resourceVersion` and target identity, this attests one successful
UID/RV-fenced **metadata-only** update. It does not authorize another value
write. The receipt is retained only while the exact source UID, version and
target remain current, and disappears after any other source version change.
Source writers still cannot create or modify ownership references themselves.
An adapter may use this controller-owned status to recognize its own stored
value after enrollment without reading values or accepting arbitrary version
changes. Older cores without this evidence cannot authorize that transition.

Internal bundle creation separately records
`kars.azure.com/credential-bundle-uid` on the actual owning target. For a
Task-owned runtime this is the Task, not its materialized Sandbox. A missing
annotation on the child alone therefore does not establish a bundle failure.

If a successful, acknowledged empty bundle CREATE is followed by a target
resourceVersion conflict, core can retry only the metadata anchor, at most
twice, while retaining that exact CREATE UID/resourceVersion and rechecking
current ownership, grant, source and binding authority. This recovery supports
bare Sandboxes and the verified Task-owned runtime caller; arbitrary Task or
Team callers do not acquire a generic rebase permission. Task recovery also
preserves the authorization digest, generation, spec, conditions and owner
chain. Only the verified runtime reference and execution status/detail may
advance. Successful anchor recovery requires fresh preparation before any
credential value write; it never returns values cached by the conflicted call.

Pre-existing unanchored lookalikes, lost CREATE acknowledgements, changed
authority and persistent conflicts remain explicit failures. Core does not
adopt or delete those bundles to clear an error. Fixed-stage controller
diagnostics distinguish CREATE/anchor conflicts from later ownership refusals
without exposing credential values.

An enrolled provider/controller-settings store may only contain its
purpose-specific keys. Core, not Bridge, applies typed provider environment
updates and UID-bound Teams Deployment rollouts. Bridge has no Deployment patch
permission. The controller-settings payload cannot change images, commands,
ServiceAccounts or arbitrary environment variables. Once the controller's private
workspace is qualified, its complete pod template is sealed. Settings that would
roll that controller (including credential rotation) are refused **before any
Deployment mutation**; they are not permission to rewrite the seal. An unchanged,
already-applied revision is accepted only after checking current private
qualification and enrolled credential identities. Set cluster defaults before
private activation; per-agent model selection is distinct from changing cluster
defaults. A new settings request on a sealed controller remains unavailable rather
than being reported as applied. Image-only migration does not authorize settings
changes.

The seal also covers each qualified consumer's complete Pod template, including
annotations. `kubectl rollout restart` changes that template by adding
`kubectl.kubernetes.io/restartedAt`; it can therefore revoke private writer
readiness and make enrolled-provider discovery unavailable even when Pods are
Ready. Do not add Secret RBAC or ignore that annotation to bypass qualification.
For a recovery exercise, replace an explicitly identified Pod without modifying
its owner's qualified template, retaining normal admission and lineage checks.
If an accidental restart annotation is the only drift, remove only that change
with UID/resourceVersion preconditions **after verifying** the resulting template
matches the existing qualified digest. Otherwise use the reviewed migration or
recovery flow. Check `WriterReady`, `PrivateConsumptionReady`, and authenticated
provider discovery afterward; a Ready Pod or the grant's top-level phase alone
is insufficient.

Private egress observation is a separate opt-in capability,
`kars.azure.com/egress-observation/v1`. `--observe <sandbox>` captures the
actual Sandbox UID. It delegates only GET on `router-services-observer`,
not `router-services-admin`, `router-admin-token`, or the observer TLS private
key. Native Kubernetes GET and ServiceAccount RoleBinding subjects remain
name-bound, not UID-bound; the observation endpoint additionally verifies
current grant, Sandbox, runtime namespace and recipient identities.

Before granting native read rights, core installs controller-UID-protected
finalizers on the enrolled ServiceAccount and its actual namespace. Admission
also covers namespace status/finalize and RoleBinding aliases. Deleting the
writer remains allowed, but its name cannot finish retirement until core has
revoked its owned source/store and observer read Roles and verified that the
Roles and bindings are actually absent, not merely acknowledged for deletion.
The namespace also retains its native `kubernetes` finalizer until that release;
ordinary metadata finalizers alone are not its storage-finalization boundary.
Only enrolled identities are held; this is not a tenant-wide ServiceAccount
deletion ban. Effective permission reviews include the ServiceAccount UID and
all three standard authentication groups, and reject broad Secret, workload,
RBAC and impersonation side channels before issuing writer rights.
The workload checks include create/update/patch on core ReplicationControllers,
apps Deployments/ReplicaSets/StatefulSets/DaemonSets, and batch Jobs/CronJobs.
They apply in every existing protected scope: workspace, writer and controller
namespaces, each observation runtime, and the namespace-omitted review.
Fresh observation privacy RPC verification reuses the same effective-permission
check, so previously issued credentials and Ready status cannot bypass a newly
granted workload permission. A failed or indeterminate review denies authority;
no additional Secret or workload permissions are granted. A writer that needs
these template-writing privileges requires a separately reviewed admission
boundary, not an exception to this isolation proof.
New writer authority requires the default controller leadership barrier;
disabling leader election does not enable a parallel unfenced issuer.

The read-only TLS listener on 9447 exposes `GET /internal/observations/scope`
and `GET /internal/observations/egress/learned`. Both require exact Bearer
authentication; learned observations also require the current
`x-kars-service-scope`. The observer token cannot authorize mutations, resets,
or legacy routes, even through the legacy loopback exception. Bridge pins the
controller-issued CA and Sandbox-UID hostname, resolves only the verified
Pod/ReplicaSet/Deployment lineage, and disables redirects, ambient trust roots
and proxy discovery. Missing capability is an error, never a legacy fallback.
Core adds the receiver-scoped runtime ingress policy and approved observer
runtime/verifier paths described below. Existing BFF egress
isolation must explicitly permit that verified runtime's TCP 9447 before
observation enrollment is usable. Core must not create an egress-only policy
that accidentally isolates a previously unrestricted BFF and blocks its
Kubernetes, provider, GitHub or OIDC calls.

Core now performs a read-only preflight against the actual BFF Pods and their
selected NetworkPolicies before issuing an observation credential. Unrestricted
senders need no new policy. Isolated senders require an explicit TCP 9447 path
to the selected runtime namespace and Sandbox Pods. The private chart's
`networkPolicy.observations` option is off by default, requires confirmation
of **existing** isolation, and accepts only explicitly reviewed target namespace
names. It does not replace the existing API/provider/OIDC/GitHub egress baseline.

The observer router also requires HTTPS access to the canonical Kubernetes API
for its existing authenticated metadata checks. Only an explicitly approved
current observation target, after the runtime/controller isolation preflight,
receives this path. Its existing grant-owned runtime NetworkPolicy includes
exact API Service and ready Endpoint `/32` or `/128` destinations with their
validated HTTPS ports, using the same canonical-target validator as SRE.
No ordinary Sandbox receives this additional policy.

When the `cilium.io/v2` API is already installed, core additionally owns a
namespace-scoped CiliumNetworkPolicy with only `toEntities: [kube-apiserver]`
and those validated TCP ports. Its selector uses source-qualified Sandbox and
namespace labels, never `pod-template-hash` (excluded from Cilium identities by
default). This handles Cilium's API entity classification; it does not install
or configure Cilium, add a CiliumClusterwideNetworkPolicy, or allow world/nodes.
A genuine discovery `404 NotFound` keeps the portable non-Cilium path.
Authorization, transport and malformed discovery errors block issuance rather
than silently treating Cilium as absent.

The chart grants **only the controller** `get/list/create/update/delete` on the
namespaced `ciliumnetworkpolicies` resource through its existing controller
ClusterRole/Binding. These permissions are not granted to agents, Bridge, or
users; controller calls use namespaced policy APIs, not clusterwide policy
inventory. Existing namespace-label patch permission maintains a durable
`kars.azure.com/observer-api-policy=v1` cleanup index. That label grants no
network access and remains until namespace deletion, so interrupted cleanup
cannot hide a policy after other metadata has gone. Grant UID, target UID,
namespace UID, namespace ownership and generation checks authorize lifecycle
operations. Owned policy replacement/deletion uses UID/resourceVersion fences;
replacement removes stale `specs`, extra ingress and other policy extensions
instead of merging them. Removed targets, disabled/deleted grants and stale
generations revoke the CNP even after partial RBAC/NetworkPolicy cleanup.
The UID-1000 egress guard, API authentication, TLS validation and original
observer-readiness deadline are unchanged.

### Controller privacy verification RPC

Enable the approved core verifier explicitly:

```yaml
observationPrivacyRpc:
  enabled: true
```

The default is off, including upgrades with old reused values. This neither
changes standalone/unbounded agents nor requires Bridge to be installed.
Observations require the declared verifier capability; an old/disabled
controller is unavailable, never an invitation to use legacy admin credentials.

The existing controller serves only authenticated
`POST /internal/observations/verify-privacy` over private TLS TCP 9448.
It calls the **full `privacy_epoch` helper on every request**, including active
SRE admission, identity and private token-alias inventory. It also rechecks the
registration/private SA identity and real GET/LIST/WATCH denial for the RPC's
TLS material. No raw Secret read/list permission or additional Kubernetes
credential is granted to the BFF, router or agent.

The existing observation bearer is explicitly scoped to this read-only
verification protocol. The controller derives the only credential lookup from
the verified Sandbox: `kars-<sandbox>/router-services-observer`. It checks that
Secret's current UID/resourceVersion, ownership, purpose, token and configuration,
not merely status. The request binds actual workspace/Sandbox/runtime UIDs,
grant UID/generation, all declared recipient SA/namespace UIDs, canonical service
identity, local scope, operation, verifier identity and a fresh 256-bit nonce.
This is **not** a claim that an opaque token authenticates a Pod or audience.

Successful responses contain only allow/proof metadata, a request digest,
nonce and qualified epoch. Denials are generic and contain no token, alias name,
Secret data or backend diagnostic. Pending/error/timeout, a wrong epoch,
expired credential or replaced identity denies access. Qualified `None` is
accepted only through the real no-registration/retired privacy contract.
Each observation fetches a new proof; no positive proof or HTTP connection is
cached between RPCs. Replies are checked against the current local scope after
the request, so replay across nonce/target/version/scope/operation cannot grant
authority. The endpoint bounds bodies to 32 KiB, concurrency to four and the
entire verification to eight seconds.

Core issues the verifier certificate with the existing TLS provider and keeps
it in the fixed core-owned Secret `kars-observation-privacy-tls`. Its public
descriptor ConfigMap and canonical Service are both named
`kars-observation-privacy` in the configured controller namespace. Clients check
live descriptor/Service/namespace/controller identities, pin the issued CA and
namespace-UID hostname, and resolve only the verified Service ClusterIP.
Redirects, ambient proxies/trust roots and plaintext metrics-port transport are
not used. The shared TLS transport uses already-locked workspace libraries,
without adding package versions or a separate credential/sidecar.

Only a running controller advertises the current TLS revision on its Pod. The
Service selector follows that revision; old binaries do not acquire a ready
endpoint through chart labels alone. Certificate/Secret recreation or rotation
changes the descriptor and observation binding even when material is identical.
The observation token expires within one hour and is renewed ahead of expiry
without rotating it on every reconciliation.

Per-target additive NetworkPolicies permit runtime-to-controller TCP 9448 and
the controller's reverse capability probe on runtime TCP 9447. Existing approved
controller/runtime ingress and egress isolation is required first; no blanket
BFF egress policy is created. The BFF's separately approved TCP 9447 path remains
unchanged. `Prepared` permits only verifier-backed scope discovery, allowing
bootstrap without a Ready cycle. Learned data remains unavailable until the
controller verifies current Pod→ReplicaSet→Deployment lineage and the live TLS
scope response declares the new verifier. Failed/Pending probes preserve the
unfinished rollout rather than destroying it.

When readiness remains `Prepared`, the controller and router emit failure-only
`Private observation readiness pending` events. These contain a fixed `stage`,
numeric `http_status` (`0` means no HTTP status recorded), and `timeout`/`connect`
classification booleans. No error text, credential, endpoint, identity, scope,
request, or proof is included. An interrupted check records its last stage;
the enclosing route/RPC event separately marks an elapsed deadline. A `false`
transport flag alone is not proof of connectivity.

Use `consumer_*` stages for rollout/lineage, `observer_transport` and
`observer_http` for the controller-to-9447 path, `observer_*_read` for router
metadata access, `verifier_*` for live endpoint discovery and pinned 9448
exchange, and `rpc_*` for the controller's current authority/privacy proof.
An HTTP 403 does not alone distinguish bearer rejection from a failed live
proof. Diagnostics do not make `Prepared` ready, change denial responses,
cache proofs, or replace TLS, network, rotation, and unauthorized-peer tests.

`observer_target_client` adds request-local progress for client initialization,
request construction, service entry, post-auth dispatch and response headers,
plus bounded configuration-match facts. Dispatch does not prove packet delivery.
`endpoint_environment_matches` compares effective HTTPS ports and canonical IP
literals against the in-cluster environment. Locked kube-client 3.1.0 omits
`:443` when constructing the in-cluster URI; an absent explicit URI port means
443, not an endpoint mismatch. Missing/invalid environment values, another
host/port, or a non-HTTPS URI still produce `false`.

The optional transport diagnostic group contains only booleans:

- `transport_debug_observable` / `transport_trace_observable`: service entry
  occurred and the corresponding tracing level is compiled in; these do not
  guarantee complete connection-event coverage.
- `tcp_connect_started`: the locked hyper-util 0.1.20 connector reached its
  fixed pre-connect event.
- `tcp_connected`: that connector observed a successful TCP connection.
- `http_handshake_complete`: hyper-util completed HTTP client connection
  setup after the connector returned (including TLS for HTTPS). This is before
  the background HTTP dispatcher is started, not proof of request delivery,
  response headers, API authorization, or observer readiness.

These are request-local observations made while the target service call/future
is polled, not socket identifiers. A pooled connection can skip all three
progress events. A speculative connection can emit events before a different
pooled connection wins; its later work, and HTTP dispatcher futures spawned by
hyper-util, do not inherit this subscriber automatically. Consequently, `false`
does not prove a TCP/TLS failure, absence of traffic, or a CNI denial.

The target service and body/error decoding use the scoped subscriber, which
retains only fixed progress bits and discards raw library events. Literal
matching stops before address/error suffixes; the caller emits bounded
diagnostics outside that scope. Sibling requests keep their own logging and
progress state. This is not process-wide instrumentation of spawned work.

The native `Prepared` observer deadline's root cause remains unknown. Default
port normalization fixes a diagnostic false-negative only; these additive
observations do not fix the timeout or establish/exclude a network-policy
cause. Hosted transport regressions and native qualification remain required.

Operator command failures use `KARS_PRIVATE_COMMAND_FAILURE` with fixed phase,
operation, resource-kind and server-reason classes plus a bounded exit code.
The phase describes the attempted step, not a committed transition. Unknown
reasons stay unknown; no HTTP status is inferred. Original argv, object names,
values, stderr and error causes are not retained, and failed CAS operations
are not retried or rebased by this diagnostic path.

## Operator workflow

Private writer/observation activation is an additional review in the existing
`grant preview` / `grant apply` flow. It is not a prerequisite for ordinary
standalone core installation. The passive consumption policies apply only
after a namespace is protected for this capability. The agent-visible
`router-admin-token` is deliberately not private service authority:
private operator controls use `router-services-admin` through
`KARS_SERVICES_ADMIN_TOKEN` or `/etc/kars/services/control-token`, and reject
agent admin credentials.

Upgraded private enrollment requires a `privateActivation` review containing
the actual root namespace, ServiceAccount and Deployment UIDs, a root template
digest, an explicit controller identity profile, current admission-bundle
UID/revisions, and reviewed namespace/consumer identities. Old review files
are rejected with a re-preview instruction rather than assigned guessed trust.
The optional historical `--controller` integration setting is not this review.

For example, after installing the upgraded core prerequisites:

```sh
kars credentials grant preview --namespace workspace \
  --writer addon/credential-writer --private-root kars-system \
  --private-controller-profile service-accounts \
  --observe agent --private-consumer kars-agent/Deployment/agent > reviewed-grant.json
kars credentials grant apply reviewed-grant.json
```

`service-accounts` pins the actual Kubernetes controller ServiceAccount UIDs.
The alternative `kcm-certificate` profile explicitly permits the authenticated
`system:kube-controller-manager` certificate principal with an absent UID,
not a similarly named ServiceAccount or arbitrary `pods.create` holder.
Only the required child-creation stage is permitted; controller UPDATE
bookkeeping requires unchanged execution templates and ownership. Explicit
registrar authority retains its existing SRE-runtime scope.

Choose the profile from the cluster's actual controller identity configuration,
not from whether the cluster is Kind or AKS. For a visible kube-controller-manager,
`--use-service-account-credentials=true` requires `service-accounts`; selecting
`kcm-certificate` can prevent its Deployment controller from retiring ReplicaSets.
The profile and controller UIDs are bound into the retirement attempt. Do not
switch profiles, discard Pending protection, or rewrite that binding after
staging has begun; a mismatched attempt needs explicit operator recovery.

The CLI checks visible static controller-manager credential flags during preview
and again before staging, rejecting a contradictory profile before any mutation.
The operator must be able to list the selected controller-manager Pods in
`kube-system`. A managed control plane with no visible static Pods still requires
the explicit, independently verified profile; absence does not infer a mode.

For an initial, single-root certificate-profile mistake only, the maintained
recovery command can review a correction to actual controller ServiceAccount UIDs:

```sh
kars credentials grant recover-controller-profile preview original-grant-review.json > reviewed-recovery.json
kars credentials grant recover-controller-profile apply reviewed-recovery.json
```

This requires cluster-scoped credential-operator authority, observed static
controller-manager ServiceAccount credential use, the original root replica
intent restored, unchanged reviewed identities/templates, and a version-1
`pausing` attempt. No budget rotation baseline, qualified epoch, existing private
grant or writer authority is allowed. New consuming Pod UIDs, incomplete
inventories, drift since review and replay are refused. The single UID/version-
fenced namespace update keeps protection Pending and preserves the attempt,
captured Pod UIDs, replica intent and exposed-key history; it grants no authority.
Keep both reviews as operator evidence, then make a **fresh** normal grant preview
with `--private-controller-profile service-accounts` and apply it. Normal retirement,
post-retirement budget-key rotation and qualification still apply. Completed,
shared, partially advanced or otherwise different attempts require separate
operator investigation, not this correction.

Preview is read-only and exports metadata/digests, never private values or raw
templates. Review the referenced root and consumer templates before applying.
Additional `--private-consumer namespace/Kind/name` entries can protect an
owned runtime without granting observation access to it. An unexplained Pod,
an ownership change, a different execution template, or an incomplete inventory
blocks activation; it is not deleted or adopted to make qualification pass.
Unrelated non-consuming Pods are preserved.

Preview and apply both inventory live private consumers before the first
protective write. These checks do not replace the retirement, post-restore or
publication race fences. In particular, a root descendant cannot evade review
by changing its ServiceAccount.

For the reviewed root Deployment → ReplicaSet → Pod path, the CLI can verify
the standard AKS Workload Identity token/env/mount additions, ServiceAccount
image-pull-secret defaults and non-BestEffort memory-pressure toleration without
adding the Pod to `--private-consumer`. It checks the **complete execution and
network metadata**, not an `AZURE_*`/volume/toleration ignore list. The original
namespace, root ServiceAccount and Deployment identities and template remain
pinned. Pod and owner UID/resourceVersion snapshots, namespace admission
metadata and ServiceAccount configuration are rechecked around the comparison.
An existing reviewed Azure client env remains exact even when the ServiceAccount
has no client-ID annotation; missing annotations never authorize a new client env.
The existing ordinary Kubernetes comparison and explicit Pod-template review
remain available; this fallback does not authorize arbitrary workload/webhook
mutations, sidecars, proxy injection or other identity providers.

The fallback sends one **server dry-run CREATE** per Pod verification using a
synthetic name and the full owner-derived Pod template. The raw API request
always includes `dryRun=All&fieldValidation=Strict`, with a 30-second request
timeout. No Pod is persisted, started, executed or deleted, and no Events or
credentials are requested. Kubernetes rejects webhooks that cannot participate
without side effects. This uses the operator's existing Pod-create/read
authority; the CLI adds no RBAC permissions or impersonation. The dry-run result
must match both a restricted derivation of supported admission changes and the
actual Pod, including exact token audiences, paths, permissions, existing env,
containers and security settings. Only scheduler-assigned `nodeName` (when not
template-pinned), the generated API-token volume name, empty lists and toleration
ordering are normalized. Unknown runtime fields are retained and strict server
validation must reject unsupported fields rather than prune them.

On Kubernetes 1.35, the dry-run also replays topology-label admission with the
actual Pod's fenced `nodeName`; the returned region/zone labels must match the
Pod exactly. No Nodes are read or modified. Kars's post-start privacy-RPC
publication markers are separately checked against the reviewed enabled
controller, namespace/ServiceAccount UIDs, the current public
`kars-observation-privacy` ConfigMap descriptor and its exact revision-selected
Service UID/selector. Both objects are re-read with UID/resourceVersion/content
fences. Those named ConfigMap/Service reads fetch no Secret or private key.
This verifies an existing publication marker; it does not mint an RPC capability,
change its protocol, or excuse other network labels/annotations. Neither
publication markers nor topology labels are silently stripped to pass admission.

Admission denial, unsafe/unsupported mutation, incomplete evidence or drift
fails closed with redacted diagnostics; there is no real-create fallback or
automatic retry. A name- or caller-dependent webhook must still produce the
same complete supported result; otherwise use explicit operator review.
Raw templates, environment values and admission responses stay in memory and
are not added to the exported review or retirement receipt.

If an older CLI stopped after restoring the root with `Qualified` namespace
state, a `restoring` retirement record and no published grant, use the corrected
**public CLI** to freshly preview and apply the **original exact scope**.
Successful verification reuses the existing retirement binding, captured UIDs,
epochs and replica intent; it does not restart an already-restored root.
Do not add its Pod as a new consumer, erase annotations, change the sealed
controller template or widen the scope to bypass a failed check. A second
workspace can enroll only after the original completion is verified.

Apply rechecks the complete enforcing policy/binding specifications and their
current type-check/observation status. When updating a grant, that grant's existing
writer authority is retired first, including absence checks for its owned read
Roles/Bindings; another workspace's grant is not reset.

For a verified late-enrollment v2 Task runtime, retirement may temporarily
withdraw Task authorization while the controller observes the new grant
generation. Apply waits up to 120 seconds for current authority under the
exact quiescent grant. Task/Sandbox identity and intent, source data, private
material and executable templates remain pinned. An observed projection
revocation requires a fresh refill revision distinct from both the original
and empty revisions, consumed by the owned Deployment. Only those proven
controller metadata transitions can advance; this is not a new user review,
stale-digest reuse or an arbitrary revision refresh. Already-qualified scopes
retain their independently verified path. Missing retirement/refill witnesses or other drift
preserve retirement and require explicit recovery; no new authority is published.
The Sandbox and Task controllers reconcile independently: the Sandbox may pause
and empty its projection while the Task remains Ready. Apply therefore requires
the actual owned pause and empty projection, not observation of an incidental
Task-status transition. Current Task authorization, the revalidated grant/input
generation, fresh refill, consumed Deployment revision and old-Pod retirement
are still required. If Task authorization was observed withdrawn, a fresh
Ready attestation is required; the captured digest is never substituted for it.
The projection recheck aligns kubectl JSON and JSONPath views only for
`managedFields` absent from the captured JSON view. Originally captured
`managedFields` and all other metadata remain compared; this does not grant
ownership or weaken source, value, revision or template checks.

Task, Deployment and projection reads are not atomic. When an empty-projection
or lineage-template check conflicts with a concurrent, recognized controller
transition, apply rechecks the exact objects and retries observation within the
same 120-second bound. Both the rejected and current Deployment snapshots must
fit the captured transition; unrelated errors and observed unreviewed templates
still fail. Historical withdrawal, pause and empty-revision witnesses are
retained, never invented. Stable missing witnesses or authority drift still
block enrollment. A final recheck after lineage inspection prevents reporting
settlement from an outdated snapshot. No mutation or stale write is retried.

A Kubernetes `Recreate` rollout can report the new `observedGeneration` before
creating its new ReplicaSet and updating `deployment.kubernetes.io/revision`.
The exact restored template with the captured old rollout revision remains
pending within the same deadline; it cannot complete retirement even if other
readiness fields are current. Completion still requires the exact next rollout
revision and all identity, refill, lineage and retirement witnesses. Missing,
malformed or unexpected revisions, and unrelated template changes, still fail.

If restoration validation still refuses the observed state, the CLI emits
`KARS_PRIVATE_WRITER_TRANSITION` with fixed booleans distinguishing generation,
projection/revision, replica, metadata, executable-template and witness
mismatches. No compared values or hashes are emitted. These facts only explain
the unchanged refusal; they do not broaden accepted transitions or the
120-second production bound.

For first qualification,
namespace protection is then enabled in `Pending`, identities/templates are rechecked,
and only approved authority-consuming controller replicas are paused. This
includes private material, privileged ServiceAccount automount/projected tokens,
and host-access authority, not just Secret references. All captured consuming
Pod UIDs, including unlabelled and terminating Pods, must disappear before fresh
unpredictable namespace-UID-bound epochs are generated. A UID/spec receipt cannot
grandfather an old credential-bearing consumer into a new epoch.

Truly non-consuming holders (no privileged token, private material, or host
access) are preserved, even if they carry stale public markers. The reviewed
root is paused and its old token-bearing Pods are awaited regardless of budget
or TLS enablement. Only after their absence is verified are namespace epochs
created, qualified templates stamped, and the root's captured replica intent
restored with UID/resourceVersion fences. Replacement readiness is checked
after restoration, not while the root remains at zero replicas. The grant is
then published with the resulting receipt and current UID/resourceVersion.
Conflicts preserve the protection and require a fresh review; there is no
unprotected rollback.

The epoch is public freshness metadata, not authorization. Correct epochs do
not let a writer add, remove, or modify protected consumption. Admission checks
old **or** new direct/projected Secret references, env/envFrom, init/ephemeral
containers, image-pull/CSI references, privileged identities, and node-access
paths across Pod, RC, Deployment, ReplicaSet, StatefulSet, DaemonSet, Job, and
CronJob templates. Namespace metadata protection covers the parent resource,
`namespaces/status`, and `namespaces/finalize`; it checks old and new private
fields with the same actor and namespace-UID fences. Normal status/finalizer
maintenance that leaves those fields unchanged remains allowed. Connections into activated private namespaces require
explicit operator authority; Pod log GET remains separate. Broad SAR checks
remain defense in depth, not a complete resourceNames-scoped permission proof.

Issuance/reuse and both fresh privacy-RPC snapshots verify the current complete
bundle, root identities, namespace fence, and actual relevant consumers.
Potentially exposed service tokens and TLS identities are regenerated, not
copied. SRE Kubernetes tokens are invalidated by replacing their bound Secret
UID. A potentially exposed GitHub App key requires an operator-rotated key;
changing its PEM encoding does not count as rotation. Private
`Prepared`/consumer availability remains distinct from authorization.

The public integration foundation also supplies managed MCP and governed
inference budgets. Managed MCP's non-consuming, no-automount workload remains
outside this private material classification; its existing local registry-pull
Secret validation is unchanged. A projected
`kars.azure.com/governed-inference-budget` audience token is router-private and
is included in consumption/retirement checks; the budget CA ConfigMap is public,
not a private credential.

When the reviewed root enables the budget broker, activation discovers its
explicit `KARS_INFERENCE_BUDGET_TLS_SECRET` and accounting namespace from the
reviewed root configuration. The additive `root.budgetTls` review contains
namespace/Secret UID and resourceVersion plus the certificate's public-key
digest. Only metadata and `tls.crt` are read for this review, never `tls.key`.
The namespace fence protects that exact configured Secret name, rather than
guessing a default name or making every TLS Secret private.

Metadata review uses kubectl's JSON metadata projection and accepts exactly one
object. Optional absence does not hide authorization or transport errors. Budget
TLS review rereads the same Secret identity after reading the public certificate;
the CLI receives metadata and `tls.crt`, not the private `tls.key`.

The review includes `root.replicaIntent`, including an explicit zero. Before
pausing the root, apply persists this intent and an attempt bound to the reviewed
namespace, ServiceAccount, Deployment, template, consumers, and bundle in
protected namespace metadata. Re-preview recovers that original intent, never
the staging-induced zero. Missing, malformed, or changed attempt/identity/intent
fails explicitly; an old insufficient review must be regenerated.
Only the credential operator, not the retiring root projector, can advance that
record. Its attempt identifier is recovery metadata, not consumption authority.

Only after the root is paused and all captured and actual authority-consuming
Pod UIDs are absent does apply reread the budget certificate and persist its
public-key baseline. It then stops and requests TLS key rotation and public-CA
update through the existing budget operator workflow. Keep the root paused,
rotate, and re-preview/apply. A key rotated while the old root was still live
becomes the baseline, not acceptable evidence of fresh issuance. An unchanged,
copied, or re-encoded public key cannot qualify. The baseline survives retries;
reviewed pre-retirement public keys are retained too, so restoring an earlier
exposed key with a newer Secret resourceVersion does not count as rotation.
Old bundle/key qualification markers cannot bypass this post-retirement proof.
If authority reappears or the pinned Secret UID changes, activation blocks.
The original replica intent is restored only after fences and templates are
qualified. Recovery state remains through grant publication. A verified completed
restoration retains its epochs and fresh key on publication retry; it does not
start another retirement merely because a grant CREATE/PATCH failed.
As for activation without budget TLS, apply waits for the reviewed root rollout and
retirement of its captured old Pod UIDs, including terminating Pods, so the
broker cannot silently keep its old startup-cached TLS identity. No budget
ledger, cancellation, settlement, pricing, or dispatch logic is changed by this
private-capability activation check. Budget operation without this capability
still has no additional activation/bootstrap requirement.

### Same-key budget TLS revision recovery

Normal private qualification requires the exact budget TLS Secret revision,
not merely the same public key. A metadata/field-ownership update can invalidate
that binding without changing the key. Do not weaken verification, restore an old
Secret version, repeat retirement or reissue funding to recover it.

For an already sealed, running **single-root** qualification with exactly its
original `workspace` grant and no affiliated namespaces, a credential operator
can explicitly review and publish just the revision delta:

```bash
kars credentials grant recover-budget-revision preview \
  --private-root kars-system --context my-cluster > budget-revision-review.json
kars credentials grant recover-budget-revision apply \
  budget-revision-review.json --context my-cluster
```

The preview contains public identities and grant intent, never Secret data.
It checks the original sealed retirement history, unchanged public key and Secret
UID, controller identity/template/profile, consumer epochs and complete relevant
grant/namespace inventories. Apply rereads those facts and uses UID/resourceVersion
fences. It changes only the TLS revision in the root seal, namespace binding and
matching grant. The key, original retirement record, grants' other settings,
workloads and financial state are untouched.

The controller may already have quarantined this sealed namespace as `Pending`
after detecting the stale revision. The explicit review records that state and
can restore `Qualified` only with the same sealed history, unchanged running root
and fully reviewed current consumers. Missing history, unreviewed Pods, another
state, or an unchanged Secret revision cannot use this recovery. Ordinary
qualification never accepts `Pending`.

If publication stops after the namespace write, retain this exact review and
rerun `apply`; it accepts only the corresponding new seal and unchanged old/new
grant intent. Ordinary qualification remains invalid until the matching grant
is published. Changes to the key, UID, root, consumers, grant intent or reviewed
revision require a new review, not a force option. After publication, confirm
`WriterReady=True` and `PrivateConsumptionReady=True` at the grant's current
generation. A successful CLI check alone is not installed runtime acceptance.

### Initially retired Helm-managed TLS checksum recovery

For a **Helm-managed** budget identity, rotating the supplied certificate/key also
changes `checksum/inference-budget-tls` on the controller template. The initial
retirement pins that template, so use the explicit checksum recovery rather than
removing the retirement annotation or keeping stale Helm key values:

1. After normal apply reports the post-retirement rotation requirement, run
   `kars credentials grant recover-budget-tls preview reviewed-grant.json > tls-recovery.json`.
   This is read-only and requires the root to be retired and paused with its
   baseline recorded; it exports no key, certificate or execution template.
2. Issue a fresh identity and use the normal core Helm upgrade with the reviewed
   certificate/key and public CA inputs. Keep `controller.replicas=0` in this
   maintenance upgrade. Preserve all other values, images, chart settings and
   templates. Do not roll or recreate the paused root. Do not enable Helm's
   `--wait` for this maintenance upgrade: Pending protection intentionally
   denies creation of the changed root ReplicaSet until qualification. Check
   the exact applied maintenance state instead; wait for readiness only after
   recovery and qualification restore the root.
3. Run `kars credentials grant recover-budget-tls apply tls-recovery.json`.
   It verifies that substituting the **old checksum alone** reconstructs the
   originally reviewed full template digest, that the same Secret now has a
   previously unexposed public key, and that no consuming Pod or writer authority
   appeared. The UID/version-fenced update changes only Pending namespace
   bindings; all captured UIDs, baseline, exposed keys and original replica
   intent are retained.
4. Make a fresh normal grant preview and apply it to qualify and restore the
   original intent. Then persist that same original `controller.replicas` value
   in Helm through an otherwise unchanged upgrade, so subsequent upgrades do
   not restore the temporary maintenance zero. Explicitly supply the newly
   issued TLS inputs again if the maintenance release failed: `--reuse-values`
   can select the last successful release's exposed identity. Verify that the
   restoring render preserves the rotated certificate/key and checksum before
   applying it, then wait for the restored release to become ready.

This checksum recovery is limited to initial single-root retirement, not a
general template migration, completed/shared-root upgrade, certificate issuer or
authority bypass. Any other template change, replaced identity, stale review,
old key, live consumer or existing private grant/writer is refused. Externally
managed Secret rotation with an unchanged controller template continues through
the normal preview/apply flow without this checksum recovery. After qualification,
persist the rotated `inferenceBudget.caBundle` through an otherwise unchanged Core
upgrade. When `inferenceBudget.tls.certificate` is empty, Core references the
external Secret without adopting it into Helm. Continuity still requires the
exact sealed Secret UID/version, namespace, type and live public CA; the catalog
and any chart-managed TLS Secret must remain owned by the Core release.

### Shared roots and multiple workspaces

The same `grant preview` / `grant apply` commands automatically reuse an already
completed root and shared writer namespace. There is no separate activation
command, per-workspace controller root, or test-only override. First qualification
still performs the full retirement above. Once restoration and current consumer
checks succeed, apply seals a bounded version-2 completion envelope in the existing
`kars.azure.com/private-root-retirement` annotation. It preserves the **exact v1
JSON string**, its digest, and the original qualified review including every
original scope epoch. This existing annotation is operator-only even for the root
projector; ordinary private annotations cannot safely hold lifecycle evidence.
No policy change is required. Its public digests are integrity/binding metadata,
not credentials or writer authorization.

Reuse verifies the original full namespace/consumer binding, exact current
bundle/profile/root/template/budget inputs, restored replica intent and readiness,
original scope fences, reviewed owner chains and execution, current epochs, and
absence of every captured old UID—even a returning non-consuming holder. It does
not simply remove namespaces from the retirement binding. Shared namespace
epochs, parent approvals, consumer replicas/templates, older grant documents,
and retirement/key history are not rewritten. Updating or revoking a grant still
uses that grant's existing UID/resourceVersion and owned-authority retirement
flow. Re-enrollment after all grants are removed is supported while the original
namespace and consumer evidence remains intact.

Active-grant updates capture namespace UID/resourceVersion and complete namespace
snapshots **before** quiescing the selected writer grant. The controller removes
that grant's `kars.azure.com/credential-reader-<grant UID>` label, annotation and
metadata finalizer after read authority retires. Apply waits for this cleanup as
well as the current writer acknowledgement and owned-role absence. It may advance
only the affected reviewed namespace resourceVersions, and only when the observed
delta is precisely removal of those existing, controller-bound guard fields.
Other grants' guards, all other labels/annotations/finalizers, the namespace's
native `spec.finalizers` hold, private receipts/epochs and remaining namespace
content must be unchanged. Empty metadata maps/lists omitted after the last guard
is removed are equivalent. Bare RV changes or unrelated drift fail closed.
The original root/template/profile/budget and grant inputs are revalidated; this
is not a new preview or adoption of arbitrary latest versions. Selected grant
intent and UID/resourceVersion CAS remain fenced through publication.

An additional clean scope has its own root-bound version-3 scope receipt in that
same operator-only annotation **on its own namespace**, not on the shared root.
UID/resourceVersion-fenced
`Pending` staging precedes a complete empty-private-consumer check. Its own epoch
is then recorded in `Stamping`, approved templates are stamped, and only verified
completion changes the receipt to `Qualified`. Interrupted stamping reuses that
scope's recorded epoch; it never regenerates shared epochs. A bare Pending or
Qualified annotation without matching lifecycle evidence is not reusable.
Concurrent stale previews/CAS conflicts require re-preview and preserve completed
scopes. Failed controller grant verification issues no writer authority; pending
protection preserves other scopes only after independent live namespace and
consumer verification.

Legacy v1 `restoring` records are not silently treated as completed. Migration
requires the original stored qualified grant (including its epochs), or, if none
exists, the exact original namespace/consumer review plus all live completion
checks. The transition wraps the original v1 JSON with a CAS against those exact
unchanged bytes. Older CLI versions reject the new envelope instead of restarting
the shared lifecycle. An interrupted original restore can resume its
captured replica intent and epochs; it cannot qualify another workspace until
complete. Pausing/retired attempts retain their original binding, captured UIDs,
baseline and exposed-key history, including the existing post-retirement budget
rotation requirement.

### Late enrollment of an existing Task runtime

The same `preview --observe <sandbox> --private-consumer
kars-<sandbox>/Deployment/<sandbox>` and `apply` commands now support a narrow
owner-specific retirement lifecycle. Preview is read-only and reports the effect
on stderr; its JSON and the private-consumption/v1 grant schema are unchanged.
The supported target is one live controller-owned Deployment in its exact
KarsSandbox namespace claim, optionally owned by a current Ready, launched
KarsTask. Namespace/Sandbox/Task UIDs, executable template, source specification,
Task authorization and controller ownership must agree. An unobserved Sandbox
generation or changed reviewed Deployment resourceVersion requires re-preview.
Task authorization is the production `sha256:<64 lowercase hexadecimal digits>`
string, retained byte-for-byte in the configured identity, receipt and recovery
checks. It is not interchangeable with a bare hexadecimal structural digest.
Only the existing controller-issued `router-services-admin` token may need
rotation. Missing or customized keys, existing observer/TLS/App material,
privileged token/host access and other consumers are not adopted.

Apply records a root-bound version-4 receipt in the **target namespace's existing
operator-only `kars.azure.com/private-root-retirement` annotation**:

1. `Pausing` saves original suspension and replica intent, source/owner hashes,
   Secret UID/version and a digest of the existing authentication key. Apply,
   unlike preview, reads this one bounded private token into operator memory;
   neither the token nor source credential values are printed or stored in the
   receipt. UID/resourceVersion-CAS sets `spec.suspended=true` and scales the
   reviewed Deployment to zero. It never sets Task `execution.launch=false`.
2. Every old Pod UID, **including terminating Pods**, must disappear before
   `Retired`. The namespace stays Pending and the original authentication-key
   baseline must remain unchanged throughout retirement.
3. `Rotating` publishes only the target's new epoch/approved parent. The existing
   controller `ensure_bound` quarantine/retired-consumer checks mint a genuinely
   fresh admin token, preserving Secret UID. Apply waits for that token's actual
   bytes to differ and for the exact controller-owned template to reference its
   new Secret UID/resourceVersion and private epoch. A stamp alone cannot qualify.
   Only these two template annotations may change; executable/source drift fails.
4. `Restoring` rechecks original owners/specifications, the new private key and
   captured UID retirement before restoring the original suspension intent.
   `Qualified` requires observed Deployment readiness (or the original suspended
   zero replicas). Supported replica intent is the controller's zero/one policy.

The controller independently honors this operator-owned in-progress receipt.
It forces zero replicas until `Restoring`, verifies the original raw Sandbox/Task
specification and owner/generation bindings, and checks the actual new token
digest and Secret/template versions at the restore boundary. Concurrent tenant
unsuspension cannot bypass the recorded retirement or substitute a different
source. Completed scopes return to the existing controller lifecycle; unrelated
core runtimes without this receipt are unchanged.
The recorded Deployment incarnation is checked before **both CREATE and UPDATE**,
including completed v4 scopes. If it disappears or is replaced, no replacement
is adopted or created; explicit operator recovery is required. Ordinary core
creation and existing v3 scope handling do not acquire this v4 identity fence.

During `Pausing`, a confirmed Kubernetes `Conflict` on the Sandbox suspension
PATCH permits at most three total attempts within the existing 120-second
bound. Each attempt revalidates the full recorded scope, runtime and Task,
and the shared-root proof. A replacement attempt requires a different live
resourceVersion and the unchanged private-key baseline; it never replays the
rejected PATCH. An already-applied, fully verified suspension needs no duplicate
write. Replaced identities, changed intent/templates/authority, unchanged
versions, exhausted bounds, and non-conflict or ambiguous failures still stop
enrollment. This exception does not cover other mutations or refresh approval.

Re-preview after an unresolved CAS conflict or lost response resumes the recorded attempt,
original intent and epoch; it cannot adopt changed templates/specifications or
invent missing retirement evidence. Failure preserves suspension and recovery
records. Task, Sandbox, namespace, source bundles, projections, agent keys and
persistent volumes are not deleted or replaced. **Pod-local ephemeral state,
including `emptyDir`, is restarted**, as preview explains; this is not a backup
or live migration facility. Already-enrolled observer/App runtimes require their
separate explicit rotation workflow. Later executable/source changes also require
an explicit review rather than silent acceptance by this retirement receipt.

**Deliberate bounds:** other additional scopes with existing private consumers
still require owner-specific retirement/rotation; shared enrollment never pauses
the root or adopts arbitrary consumers. Existing marked templates and consuming
Pod/Job instances also require explicit recovery. Changed shared consumers, deleted original
evidence namespaces, changed root/profile/bundle/template/budget identities or
keys, and missing/tampered retirement evidence fail explicitly. A completed budget
qualification can be shared only with its exact already-qualified key and Secret
UID/resourceVersion; this flow does not coordinate a live multi-grant shared-key
rotation. Each receipt is limited to 128 KiB. These failures preserve existing
work and do not authorize a fallback, delete protection, or reset older grants.

Direct Helm RPC enablement only requests the listener. It does not stage root
trust or authorize private writers; the listener remains unavailable until
generic operator activation is qualified. Direct API/Helm grant publication
must carry the same qualified receipt and live namespace fences. A prior
`Ready` value without current `WriterReady` and `PrivateConsumptionReady`
conditions is not private authority. Writer retirement (`writers: []`, or
grant disablement) retains namespace protection; no automatic deactivation
path removes it before authority retirement.

For qualification, the canonical artifact is regenerated/checked with
`python3 tools/private-consumption-bundle.py --check`. CLI fake-executor tests
cover the existing preview/apply hook, two-workspace epoch/authority continuity,
legacy migration, interrupted staging, budget reuse and CAS failures. Controller
transport tests check that a failed grant preserves independently verified shared
scopes without authorizing that grant. These are not native policy/RBAC proof.
The native
`tests/e2e/private_consumption.py::named_cases` fixture runs after operator
activation through the existing API harness: it establishes actual
resourceNames-scoped RBAC, uses inert zero-replica/suspended/no-eligible-node
bases and server-side dry-run mutations, and requires the exact intended
admission denial. It never executes a credential-reading payload. Native
qualification and independent source review remain required before sign-off.
The namespace-surface regression first proves named status/finalize RBAC,
requires exact namespace-fence denials for metadata changes, and then requires
the named workload consumption denial with the actual fence still intact.
`root_token_retirement_case` uses a short-lived API-issued token bound to the
reviewed old root Pod and TokenReview booleans before/after the existing
activation callback. It reads no mounted token, emits no credential, and cannot
pass while that Pod UID remains (including terminating) or while its API
authority remains authenticated.

The existing bootstrap collector adds `publicPolicyFailure` to failed
`kars-private-consumption` API diagnostics. It emits only complete known public
field/annotation keys or CEL binding names from the canonical bundle, plus
canonical expression indexes/names when the response actually supplies a
location, identifier, or exact public expression. It does not infer an
expression from a missing key. Unknown keys, policy drift, ambiguous attribution
and unavailable locations remain explicitly `unclassified`; no raw Status,
object, header, token, annotation value or expression text is exported.
Recognition never changes the original HTTP status or failed bootstrap
assertion. The same bounded collector runs in the existing schema CI job.

Install the new CRD, controller and admission policies first. Install the private
add-on's ServiceAccount without broad Secret or Deployment write permissions.
The namespaces must already exist.

Bootstrap a missing, explicitly selected empty store when needed:

```sh
kars credentials grant bootstrap-store --namespace kars-system \
  --name kars-inference-providers --purpose providers --dry-run
```

Review before omitting `--dry-run`. Existing stores are refused by bootstrap,
not overwritten or adopted. Kind beta permits this empty provider store during
Core upgrades because it cannot override the default route. A populated store
still requires separate provider-route qualification; bootstrap alone neither
enrolls credentials nor qualifies another inference provider.
Repeat for the operator stores in use, including
`kars-credential-controller-settings` with purpose `controller-settings`.

Generate a metadata-only review:

```sh
kars credentials grant preview --namespace kars-system \
  --writer bridge-private/kars-bridge \
  --agent-key GITHUB_TOKEN \
  --store kars-inference-providers=providers \
  --store kars-credential-controller-settings=controller-settings \
  --controller > credential-grant-review.json
kars credentials grant apply credential-grant-review.json
```

Preview includes real API UIDs, not assumed names. Apply rechecks all identities
before mutation and CAS-fences an existing grant's UID/resourceVersion.
Use a separate grant in the Bridge integration namespace for its existing
Teams Secret and `--bridge-consumers`. Empty/missing tenant credentials must
not start the gateway or block ordinary web-only operation.

For legacy migration, inspect `status.legacySources`, review the source
namespace UID, Secret UID/resourceVersion, complete key-name set and target UID,
then supply that metadata array through `--legacy-review`. Existing values are
not printed or changed by preflight. Unsupported/reserved keys and ambiguous
ownership block import before projection; the operator must resolve them
explicitly. An unclaimed old runtime namespace still requires the independent
namespace-ownership workflow; credential migration does not adopt it.

## Binding and delivery

`credentialBindings` on a Task blueprint or directly authored Sandbox contains
the grant `{name, uid}` and ordered sources:

1. explicitly selected workspace source;
2. explicitly UID-bound Team source;
3. explicitly UID-bound target source.

Each selection contains a source `{name, uid}`, approved key names and, for
Team/target scopes, the owning target identity. References and key grants are
part of the shared effective Task authorization snapshot. Child references
and key sets may not exceed their parent's credential authority.
Attenuation compares the final **declared source authority per key** after
ordered precedence, not only each selection independently. A later selection
is still an overriding authority/mask when its Secret has no value. A child
cannot drop that selection (or its retained key) to reveal a parent's hidden
earlier credential.

Before publishing ordinary Task `Ready`, core performs a read-only live grant,
source and GitHub-enrollment preflight. This does not prepare bundles or mint
credentials, and does not require the candidate Task to already be Ready.
Delegation ancestors still require normal readiness. Invalid authority clears
the Task readiness proof used by other controllers, including governed
inference; no alternate budget predicate or authorization digest is introduced.
Selected Tasks recheck on the existing short reconciliation interval.

Prelaunch sources remain unbound. Bridge stages Tasks/Teams without runnable
execution, captures the actual CREATE UID, attaches the source selections, and
only then requests activation. A CREATE conflict is never converted to adoption.
Activation retains UID/resourceVersion fencing and allows at most two retries
for status writes or the controller's initial Task/Team cleanup-finalizer
registration. Only that exact added finalizer and its isolated SSA ownership
entry are exempted from the intent comparison; existing finalizers, other field
ownership, plans, credential bindings, annotations, identity and deletion state
must remain unchanged. Removing a finalizer or changing authority still fails.
Core verifies current Task authority before preparing a UID-owned bundle and
the existing UID-fenced runtime projection. Agent values never enter router
EnvFrom. Runtime environment overrides of selected keys are rejected.

Removing an agent key records persistent metadata-only removal intent in the
source annotation `kars.azure.com/credential-removed-keys`. The source value and
intent update together under UID/resourceVersion CAS. Core applies these masks
after reviewed legacy import, including when the source was created before its
first import. Retries do not restore the key; explicitly setting it again clears
its tombstone. Secret values never enter that annotation.

Missing selected keys mask lower-priority values. Removing a key does not remove
the binding or restore direct credentials. Missing/replaced/revoked authority
stops the credential consumer and clears only its owned projection. Previously
governed consumers do not silently return to the old direct collection.

While a still-launched governed Task is unready, core pauses its exact owned
runtime rather than deleting the Sandbox, namespace or stored state. Explicit
unlaunch/deletion retains the established cleanup behavior. Optional private
observations report separate integration errors and cannot create a circular
dependency between the source grant's readiness and the Task they observe.

Team credential rebinds do **not** unlaunch Tasks. The controller requests a
credential pause, durably clears Ready/its authorization digest, retracts the
old current attestation and holds the exact owned Sandbox runtime at zero
replicas. It waits for all old Pods, including terminating Pods, before changing
the binding. Task, Sandbox, namespace and stored-data UIDs remain unchanged.
Resume requires current Team/Task constraints, a newly validated configuration,
the matching current receipt and the same owned quiescent runtime. A
UID/resourceVersion-fenced Deployment apply prevents stale work from undoing
the pause. Existing explicit Sandbox suspension is preserved. Explicit user
unlaunch/deletion retains normal teardown behavior.

`CredentialsReady` and grant status expose key names, source/bundle/projection
UIDs, observed versions and reasons—not values. Non-404 API errors are errors,
not an empty configuration.

## Lifecycle and qualification

### Keyless GitHub enrollment

For a new Bridge installation, first complete the explicit
[GitHub App storage enrollment](../../bridge/deploy/helm/kars-bridge/README.md#github-app-storage-enrollment)
bootstrap/review/apply steps. Credential-free Helm enrollment intentionally has
no App store or writer: disable `workspaceEnrollment.enabled` before operator
expansion, retain all existing grant authority in the full desired-state
preview, and qualify private activation. The Console withholds its App key
form until the exact store is enrolled and readable. Store setup is distinct
from the repository authority reviewed below.

`--github-review <file>` accepts a metadata-only array of reviewed connections:
`connection:{name,uid}`, `appSecret:{name,uid}`, `appId`, `ownerSubject`,
`installationId`, canonical `repositories`, and `write`. The same connection may have separate reviewed entries for different repository
sets and write permissions—for example, write access to a demo repository and
read-only access to a private repository. Each requested binding must be fully
covered by one entry; permissions from separate entries are never combined.
Existing App, connection, owner and installation identity checks still apply.
Admission checks both resource permissions for every entry, up to the existing
32-entry schema bound. These checks are split across four fail-closed policies
so neither an expression nor a binding exceeds Kubernetes CEL authorization
cost limits. All four `kars-credential-github-authority-*` policy/binding pairs
must be installed before removing the older combined GitHub check from
`kars-credential-grant-authority`; no resource permission check is optional.
The App store must also be explicitly enrolled with purpose `github-app`. Preview/apply recheck
the existing Secret and connection ConfigMap UIDs, installation and repository
inventory without printing values. They never adopt another store or grant
Bridge the ability to enlarge that operator review.

The effective Task/Team/Sandbox `githubBinding` carries exact grant/connection
UIDs and a repository/write subset. Core verifies the current effective Task
authorization, reads the enrolled App store, then materializes the consumer's
exact `router-github-app/config.json` schema through the same strict
`privacy_epoch`-gated private issuer. Configuration changes rotate the private
version and require retirement of old consumers. Source stores retain their
UIDs and values; neither tokens nor App keys enter agent source bundles.

For read-only private repository metadata, select the App-connected repository
in Mission/Team review and use native `network` capability. Both runtimes route
canonical, headerless `GET https://api.github.com/repos/{owner}/{repo}` and
paginated `/branches` reads through the existing keyless adapter. Match the
repository's `default_branch` to the branch list's `commit.sha`; the adapter does
not support `/branches/{branch}` or `/git/ref/heads/{branch}`. No extra direct
GitHub egress, `github-pr` capability or write grant is needed for these reads.
Binding authorization and the bounded REST allowlist still apply.

**GitHub MCP is separate:** its configured bearer credential determines its
access. Selecting App repositories neither grants nor restricts that credential.
Successful MCP initialization or public reads do not prove private access. Use
the App route for supported private metadata reads, or have the administrator
review the MCP service's own authorization when MCP is explicitly required;
do not broaden credentials or direct egress to work around a denial.

Mission and Team readiness and creation derive write authority from the reviewed
execution plan: only `github-pr` in a role phase or synthesis requests PR access.
Network-only plans request read-only access. Team edits use the effective edited
plan (or the saved plan when unchanged), and a plan-only change reauthorizes the
saved repository selection; it cannot silently retain or add write permission.
Legacy packages without a typed plan retain their explicit PR-access semantics.

The Team editor displays its saved repository selection from `githubBinding`,
including read-only selections, without treating them as permission to write.
A present read-only or empty binding never falls back to broader legacy
`gitWrite` selections. Older Teams without a binding still display their legacy
selection until they are saved through the governed connection flow.

The private Secret's separate source-revision annotation binds grant UID/spec
generation, App-store and connection UIDs/resourceVersions, Sandbox generation,
runtime namespace UID and the canonical managed identity. A changed revision
forces a new projection version and consumer rollout even when `config.json`
bytes are identical; no unsupported fields are added to the runtime parser.
Grant status-only resourceVersion changes do not cause perpetual rollouts.
Pending privacy qualification has a typed non-issuance outcome rather than
being treated by the GitHub adapter as a source-authority failure.
After explicit binding removal, a controller-protected retirement marker
disables the legacy optional GitHub mount. A distinct retirement version waits
for old cached consumers, including terminating Pods, before readiness can
recover. Removing a binding must not make retained private material usable as
legacy configuration.

Keyless mode requires explicit governed agent sources, rejects opaque GitHub
egress, and currently rejects raw GitHub/custom agent credential combinations
without a separate purpose review. This is not a migration of legacy bare
Sandbox credentials. Operator-approved custom credentials remain usable in
the existing explicitly unbounded standalone mode; that mode is **not**
repository-enforced by the GitHub gateway.

The exact GitHub runtime consumer `d3dc3ce8` is locally forward-integrated.
Its shared mount helper remains optional for ungoverned standalone
configuration and is required for an issued governed binding. The combined
issuer/consumer still needs qualification before use. A mount is not evidence
that any published router image contains this consumer.

Grant finalization revokes its owned writer/operator bindings. Namespace and
source UID checks prevent adopting a replacement. Source cleanup follows its
actual target UID; workspace sources and operator stores are not Helm-owned and
remain after Bridge uninstall. Legacy stores remain for explicit review.
Legacy discovery skips unrelated terminating targets/stores/namespaces; it first
checks whether the legacy Secret exists. Transport/authorization errors are not
reported as absence. Selected owners and reviewed source identities still fail
closed on deletion/replacement. An unrelated stuck deletion must not revoke
the whole workspace's writer, observer or GitHub authority.

Typed controller settings validate every enrolled credential Secret UID, purpose
and referenced key before taking an unchanged-config fast path. Rollout
revisions include the current UID/resourceVersion of those references as well
as the settings store. Rotating a token behind an unchanged `secretKeyRef`
therefore refreshes controller environment; grant status-only writes do not
cause a rollout loop. Revision evidence contains no values.

Writer status is now separate from delivery status. `WriterReady=False`
prevents delegated writes, but a deleted, terminating or replaced writer does
not revoke valid source/GitHub delivery authority. An operator can explicitly
retire writers with a reviewed `spec.writers: []` while retaining `enabled:
true`. Deleting/replacing a selected source or disabling/deleting its grant
still fails delivery closed. Private add-on uninstall needs the core controller
running so it can release the enrolled name holds; it does not delete core
data. A changed controller ServiceAccount UID or a foreign/legacy reader Role
without controller provenance requires operator review rather than adoption.
Do not force-remove a guard to bypass a failed revocation.

Kubernetes reconciliation is asynchronous. Sandboxes with v1 credential
references, v2 credential bindings or governed GitHub bindings use a 30-second
successful-reconciliation backstop to recheck delivery authority even when no
owned-resource event arrives. Unbound legacy Sandboxes retain their five-minute
backstop. This is not a termination deadline: permission, node or API failures
can delay consumer termination and revocation. It does not revoke a token at
its external provider or erase values an agent already observed.

This candidate still requires coordinated Rust and real API/admission lifecycle
qualification before release. The Bridge app remains private; this core
contract is not permission to publish that application or its images.

The new name-continuity admission/lifecycle code passes targeted core Rust tests
and strict Clippy, but still requires real Kubernetes qualification, including deletion/status/finalize, inherited RBAC,
controller leadership/restart and delayed Role deletion. The approved purpose-only
core privacy RPC now supplies active-SRE verification; real Kind/CNI acceptance
of its network path and private BFF Rust/API qualification remain required.
TLS, CA integrity, projected private volumes, Kubernetes admission
and control-plane integrity remain trust dependencies. Do not claim complete
end-to-end UID/privacy qualification yet.
