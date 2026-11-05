<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Install Kars with Helm

Use the Helm chart when the Kubernetes cluster, image access, inference backend,
and required identity configuration already exist. The chart can manage
AgentMesh or use an existing external deployment.

For the public `kars-bridge` integration branch and optional Bridge add-on, see
the [beta Helm installation and acceptance guide](../../bridge/docs/public-beta-helm.md).
Its [release artifacts and two Helm commands](../../bridge/docs/public-beta-helm.md#release-artifacts-and-the-two-helm-commands)
use version-selected, digest-bound Core and Bridge chart archives with bundled
locked dependencies. Once those artifacts are published, installation requires no
source checkout or image build. Private operator values, genuine HTTPS OIDC,
provider credentials and a compatible qualified cluster remain prerequisites;
packaging alone is not publication or acceptance. Use matching generic/Kind
profiles and apply the release image overrides last, not an unrelated stable
`latest` release.

Before upgrading an existing controller, run `kars namespace preflight` with the
updated CLI and resolve all ownership conflicts. See
[namespace ownership migration and adoption](namespace-ownership.md). The check
does not read Secrets or change namespaces, workloads, or Helm values.

Optional [workspace credential sources](credential-sources.md) require the
matching controller and CRD. Upgrade both before using `--credential-source`;
legacy direct credential Secrets remain the default.

## Helm-only first-install preview

For the two-release Core + Bridge beta path, use the opt-in
[Core schema bootstrap](../../cli/schema-hook/README.md) and the
[beta first-install sequence](../../bridge/docs/public-beta-helm.md#helm-only-first-install-preview--the-beta-acceptance-path).
The hook stages schemas inside the original Core Helm install; it does not
require a separate CLI schema command or a third product release. It remains
disabled by default pending helper-image and native installation qualification.
Its reviewed Helm/Kubernetes versions, image digest, API-only networking and
retained-helper lifecycle are required inputs, not inferred defaults.

The hook is not an upgrade, rollback or existing-release migration mechanism.
The CLI-assisted procedures below remain the compatibility path when it is
disabled; they do not satisfy the Helm-only beta acceptance gate.

The cold SRE funding contract pins the original router digest as well as the
source, policies and funding mode. Disabling the schema hook does not authorize a
router refresh: both the chart and controller reject changing that sealed intent.
Use the qualified router digest in a fresh install with its own explicit funding
intent: unbounded, non-expiring tokens and record-only accounting for Kind beta,
or an explicitly reviewed production grant/enforcement policy. Existing installs
require a separately reviewed identity-preserving migration.
Never edit the immutable receipt, reset the existing ledger, or treat a failed
upgrade as a successful rollout.

## Required schema-before-admission stage

When the first-install hook is disabled, use the matching CLI/chart to stage
schemas **before** Helm creates or updates the admission policies:

```bash
kars schemas prepare --release kars --namespace kars-system \
  --chart deploy/helm/kars --context my-cluster \
  --values my-values.yaml --timeout 120 --atomic
helm upgrade --install kars deploy/helm/kars \
  --namespace kars-system --create-namespace --kube-context my-cluster \
  --values my-values.yaml --atomic
```

Repeat the same values files and `--set`/`--set-string` overrides in both commands.
For a Helm upgrade using `--reuse-values` or `--reset-then-reuse-values`, pass that
same option to `schemas prepare`; the helper reads the appropriate release values
without printing them. `--check` verifies existing owned schemas without writes.
Reconcile reused values with reviewed live runtime pins, rotated budget TLS and
the current inference catalog before an upgrade; `--reuse-values` alone can
reintroduce old operator configuration. The chart retains an owned catalog's
exact JSON serialization when its parsed content matches the requested values,
so a formatting-only change does not seize an operator-managed field.
If the requested TLS certificate/key already match the owned live Secret, the
chart also retains the owned controller's existing TLS checksum: synchronizing
values after an operator rotation must not restart a sealed controller. An actual
certificate/key change still computes a new checksum and requires the normal
controller-mutation review.

For a protected controller, the explicit **non-atomic Helm 4.1.3 SSA no-op**
path accepts `controller.upgradePrecondition.uid` and
`controller.upgradePrecondition.resourceVersion`, both strings from a fresh
controller read. Pass the identical values and `--server-side true` to
`kars schemas prepare` and the following Helm operation. Preparation requires
digest-pinned controller/init-container images, verifies a real Helm-manager
SSA admission preview leaves the entire controller spec and meaningful metadata
unchanged, and rereads the controller and namespace evidence before proceeding.
The chart carries the UID/resourceVersion into the actual write, so a changed or
replaced controller conflicts rather than receiving a stale update. Reused Helm
values retain this fence: review and supply a fresh one on the next upgrade.
After SRE adoption, the chart validates this exact two-field fence against the
live controller before excluding it from the sealed configuration comparison;
the original adoption receipt and every other setting remain protected. Fences
are not accepted during initial SRE adoption.
This does not authorize a restart, root change, controller removal, template
apply, automatic rollback, or bypass of chart-hook retirement checks. An
existing chart-managed schema hook still enforces its conservative historical
root checks; do not disable one to obtain this path. Fresh installations need
neither this fence nor a private-root exception.

The controller no-op proof does **not** make other release resources immutable.
In particular, Helm can change a TLS Secret's resourceVersion while retaining
identical certificate/key bytes. A private qualification pins that exact revision.
Preparation therefore refuses this no-op exception when the current or previous
release includes the revision-bound budget TLS Secret, including its removal.
Do not remove it from the chart to evade the guard. An already incurred same-key
revision change has a separate, explicit
[sealed qualification recovery](governed-credential-grants.md#same-key-budget-tls-revision-recovery);
that recovery is not authorization for another Core upgrade.

Pass `--atomic` (or Helm 4's `--rollback-on-failure`) to preparation whenever the
following Helm operation uses automatic rollback. The helper does not infer an
external caller's later flags or silently remove them.
The command uses the bundled chart when `--chart` is omitted. No Azure deployment,
special controller, privileged Job, probe policy or Bridge-specific schema is
involved.

`up`, fast/full upgrade, Helm-owned image push/mesh updates, and SRE Helm
install/upgrade use the same prerequisite automatically. Local render/apply
installation uses `--ownership template`: newly created CRDs receive explicit
`kars-schema-stage` release/namespace ownership, and subsequent apply excludes
CRDs. Direct render/apply callers must likewise omit CRDs from the later payload;
do not reassign their schema fields to another apply manager. SRE authority's
existing narrowly fingerprinted action-schema repair is followed by the same
published-schema gate before its policies are installed.
CLI rollback resolves an explicit previous Helm revision, stages that recorded
revision's schemas through the same gate, and refuses a rollback that would
remove a CRD, change retained schema/data semantics, or whose release history
changed during preparation.

Preparation plans all CRDs before writing, refuses foreign/unmarked ownership,
and never adopts resources by matching a name or label alone. Existing schemas
must match the target chart, their recorded staged schema, or the owning Helm
release's previous manifest. Customized schemas, changed ownership, UID/RV races,
foreign SSA field conflicts, and storage-version/identity migrations stop explicitly.
Updates normally use UID/resourceVersion-fenced server-side apply with no force
conflicts. When the already-verified Helm owner has legacy `Update` ownership and
all spec field managers are that same Helm manager at the same API version, the
helper uses a UID/resourceVersion-fenced in-place replacement instead. It retains
unrelated live metadata and status; it does not delete/recreate the CRD or force
field ownership. Foreign spec managers keep the normal SSA conflict protection.
Unrelated metadata and customer custom resources are not rewritten or deleted.
An interrupted stage retains any already-created owned schemas for a safe retry;
it does not roll back by deleting CRDs.

### Schema compatibility and failure rollback

Ownership evidence is not compatibility evidence. The same compatibility check
applies to live schema updates, explicit rollback and automatic rollback targets.
Every retained version must preserve its fields and types; defaults, required
sets, constraints/CEL, map/list topology, unknown-field preservation, storage and
conversion semantics cannot change under this automation. Optional non-defaulted
properties may be added only where they do not narrow previously preserved
arbitrary data. Descriptions/printer columns may change. Version migrations,
field loss, changed validation or other unproven transitions fail before schema
writes, even when the ownership digest or Helm manifest is valid.

All 21 CRDs in the core chart carry `helm.sh/resource-policy: keep`. This remains
in the rendered Helm release, not just transient live metadata, so failed fresh
installs and newly introduced CRDs survive failure cleanup. CRD hooks, forced
replacement/adoption and `--cleanup-on-fail` are refused because those paths
cannot promise the same retention.

For an atomic upgrade, preparation reads the **latest successful deployed or
superseded** Helm revision, matching
[Helm 3 atomic rollback selection](https://github.com/helm/helm/blob/v3.16.0/pkg/action/upgrade.go)
and [Helm 4 rollback-on-failure](https://github.com/helm/helm/blob/v4.1.3/pkg/action/upgrade.go).
It checks both proposed and live schemas against that rollback manifest before
any schema write, and fences the release-history snapshot. An added field that
the rollback schema would prune is blocked; a new CRD may proceed only with keep
retention. Fresh installs and image-only/unchanged-schema atomic upgrades remain
supported when the relevant retention contract is present.

**Migration bounds:** a previous successful release without complete CRD
retention cannot safely be an automatic rollback target. A separately approved
retention-only release with unchanged schemas can establish that prerequisite;
this tool does not perform it implicitly, edit Helm history or drop atomic.
Incompatible schema/data changes require a separately reviewed migration with
data preservation evidence. There is no force/confirmation override here. The
existing explicit, non-atomic SRE authority-stage command supports the closed
BASE365 migration below; ordinary upgrades and rollback do not gain that
exception.

SRE `authority stage --dry-run` remains a read-only server-side preview: it does
not run the action-params conversion, schema writes, controller rollout or
subject changes. A successful preview is not a completed migration. Failures
emit `SRE-STAGE-FAILURE <fixed-stage>` before propagating the original error;
the marker contains no command arguments, response bodies or raw cause.
`prerequisite-chart-render`, `action-schema-review`, and `helm-server-dry-run`
identify pre-mutation failure points. `core-schema-preparation` covers read-only
planning in both preview and apply, and schema publication during apply; the
marker alone does not establish that writes occurred. Template-mode action
conversion uses `action-schema-migration`; a real Helm update uses `helm-upgrade`.

### Closed BASE365 SRE migration

`kars sre authority stage` has a separate, explicitly reviewed migration for
the canonical BASE365 chart (`8b206065608593667a40665b3f48225ef9ce278d`) to the
current core schemas. It does not relax `assertSchemaCompatibility` or add a
general CEL/validation exception. A private, non-serializable permit is issued
only after the complete 18-existing/21-target CRD inventory matches pinned
before/after spec fingerprints and the exact Helm release/namespace owner.
The finite target catalog includes the incoming `b5ad6791` evaluator-protocol-v2
report-reference, report-UID and evidence-digest fields. The summary distinguishes
`core-470` (pre-composition) from `evaluator-v2`; it does not guess missing fields.

The controller must already be paused at zero replicas, with no remaining
controller-ServiceAccount Pods. Staging does not silently stop a live controller.
Its UID/resourceVersion and quiescence remain fenced until schema qualification
finishes. Already-completed current schemas do not require a second migration
or controller pause for normal image-only authority staging.

Before any real action/schema write, the CLI qualifies all schemas and a complete
bounded inventory of affected objects, validates unchanged UID/RV-bound objects
through **server dry-run PUTs**, and dry-runs every proposed CRD CREATE/SSA update.
The full Helm stage is also server-previewed before applying the schema plan.
An unpersisted CRD CREATE preview has an ephemeral UID but no storage
resourceVersion. It is checked only as a preview of the exact proposed schema
and ownership; its identity is never copied into the real CREATE. Existing
objects, update previews and real publication still require their strict UID/RV
identities. Fixed child-step diagnostics distinguish these checks without
printing CR contents or raw API error bodies.
Foreign ownership, customized/unknown before or after schemas, forbidden reads or
dry-runs, incomplete inventories and late data/UID/RV changes stop the operation.
The bound is 512 affected objects and 8 MiB total reviewed data; larger or actively
changing installations need a separate reviewed migration procedure.

The closed transition preserves old valid data rather than inventing new
authority:

| Canonical change | Required existing-data qualification |
|---|---|
| Action params: boolean additionalProperties to preserve-unknown-fields | Existing object validates under the old schema; arbitrary nested params survive unchanged |
| Task/Team/Profile budget scope and new Task/Team budget CEL | All newly introduced scope/binding/account fields are absent, including roster members; old-schema server validation still succeeds |
| MCP managed mode and mutual-exclusion CEL | No pre-existing managed-mode fields or new workload/readiness evidence |
| Sandbox source-or-bundle reference pattern and private bindings | Existing references remain exact v1 `kars-credential-source-*` references; no newly introduced binding/budget/observation fields |
| Evaluator v2 optional status fields | No pre-existing report-reference/UID/digest evidence is grandfathered |

No custom-resource spec/status is migrated or rewritten. Canonical data hashes,
UIDs and resourceVersions are rechecked before writes and after publication;
new authority CRDs must remain empty during this legacy transition. Each real
schema write retains the original UID and uses its reviewed resourceVersion
without forced field ownership. `SRE-SCHEMA-MIGRATION` reports only fixed profile,
counts and `qualified`/`applied` state, never object values. An interruption can
resume only from the exact canonical before/already-applied target states with
fresh data qualification; there is no destructive schema rollback.

This SRE transition is explicitly **non-atomic**. It never removes a supplied
rollback flag or edits Helm history. Old successful releases without full keep
retention cannot become automatic rollback targets by virtue of this permit.
Where atomic operation is required, establish an explicitly reviewed
retention-only, unchanged-schema release first. Unknown target revisions, live
post-baseline authority, non-quiescent controllers and incompatible customer
data remain unsupported rather than being adopted.

The native BASE365 fixture calls the actual public CLI preview and apply for
negative late ownership/schema cases, verifies zero earlier action conversion, and measures
Task/Team/MCP/Eval/action data plus UIDs/RVs across the real positive stage.
Local transport and pure fixture tests are not that native proof; the composed
target still needs the hosted run.

### Rendering and target identity

An existing Helm release is rendered against its actual API server with live
`lookup`, `.Capabilities` and release upgrade context. Helm 3.13+ uses
`--dry-run=server --validate`; Helm 4 uses `--dry-run=server`.
This matters for the chart's existing SRE source ownership checks:
[Helm 3 template](https://github.com/helm/helm/blob/v3.16.0/cmd/helm/template.go)
otherwise replaces capabilities with client defaults, while
[Helm 4 server rendering](https://github.com/helm/helm/blob/v4.1.3/pkg/action/install.go)
retains real capabilities and lookups. A cold cluster's initial client render is
only a CRD bootstrap plan; after staging it must pass a full server-aware render
and exact CRD recheck before policy/workload installation. A chart whose CRDs
change between bootstrap and server rendering is explicitly unsupported, not
silently reconciled to a different plan. For an existing release, if the initial
client render cannot satisfy live CNI or sealed-lookup guards, preparation uses
the authoritative server render instead. Server guard failures still stop before
writes; fresh-install bootstrap and successful missing-dependency bootstrap plans
retain their existing checks.

The historical SRE schema acceptance fixture predates bundled monitoring. Its
prerequisite phase renders the current cold chart to select monitoring CRDs and
uses the same guarded dependency planner to create only absent definitions and
wait for publication. It does **not** install that cold chart or apply its Core
resources. Only then does it render the unchanged historical authority-stage
upgrade with live lookups, compare the exact monitoring schema fingerprint, and
run the full Helm server dry-run. This avoids requiring PodMonitor, Prometheus
and ServiceMonitor discovery before their prerequisite definitions exist; neither
SRE ownership guards nor chart validation are disabled. This fixture preparation
is not evidence that a released customer installation or upgrade has passed.

The separate render/apply ownership mode retains its template-render semantics;
it does not run Helm's install/adoption validation against non-Helm-owned CRDs.

Local kind installation pins `kind-<requested cluster>` for render, schema
preparation, namespace/credential preparation, final apply and controller
rollout. Reusing an existing kind cluster never selects or rewrites the global
current-context to make an ambient-context command appear safe.

`Established` is necessary but insufficient. The gate checks the actual served
resource mapping, fetches `/openapi/v3`, follows its server-relative hashed schema
URL, locates each served GVK and resolves local references as KCM does. The
published declaration surface must match the exact live chart CRD; the URL/hash
and CRD identity/schema are rechecked before proceeding. Missing publication or
references remain pending within the bounded deadline. Auth/transport failures,
untrusted URLs and malformed discovery fail rather than counting as readiness.
This is condition-based polling, not a sleep or a negative-cache workaround.

**Existing failed policies are a separate bound.** Kubernetes 1.31's
[status controller](https://github.com/kubernetes/kubernetes/blob/v1.31.0/pkg/controller/validatingadmissionpolicystatus/controller.go)
skips generations already observed and does not enqueue on CRD schema changes.
Its [type checker](https://github.com/kubernetes/kubernetes/blob/v1.31.0/staging/src/k8s.io/apiserver/pkg/admission/plugin/policy/validating/typechecking.go)
omits the `params` declaration when schema resolution fails.
[KCM initialization](https://github.com/kubernetes/kubernetes/blob/v1.31.0/cmd/kube-controller-manager/app/validatingadmissionpolicystatus.go)
uses a definitions resolver plus
[fresh OpenAPI v3 client discovery](https://github.com/kubernetes/kubernetes/blob/v1.31.0/staging/src/k8s.io/apiserver/pkg/cel/openapi/resolver/discovery.go);
there is no KCM negative schema cache for this flow to clear.
An unchanged policy with already-observed warnings/invalid status is therefore
reported as blocked, not repaired by waiting, generation/text toggles, status
patches or deletion/recreation. A genuine policy upgrade or separate
operator/upstream recovery is required. An existing pending unchanged policy
must finish observation cleanly before the prerequisite succeeds.

CRDs stay in Helm's tracked templates so release ownership, subsequent schema
upgrades and resource retention remain visible to Helm. Schema preparation may
persist even when a later workload upgrade fails; it never promises an atomic
rollback of customer schemas/data. Legacy unmarked template installations need
an explicit ownership migration review, not automatic adoption.
Offline render and transport-fixture tests cover these checks, but repeated cold
installs on the actual Kubernetes/control-plane topology remain required native
evidence, especially for multi-apiserver deployments.

## Local kind

```bash
kars schemas prepare --release kars --namespace kars-system \
  --chart deploy/helm/kars --values deploy/helm/kars/values-local-dev.yaml
helm upgrade --install kars deploy/helm/kars \
  --namespace kars-system \
  --create-namespace \
  --values deploy/helm/kars/values-local-dev.yaml
```

Load all referenced development images into kind before installation.

## Existing non-AKS Kubernetes cluster

Start with the generic overlay and layer environment-specific values on top:

```bash
kars schemas prepare --release kars --namespace kars-system \
  --chart deploy/helm/kars \
  --values deploy/helm/kars/values-generic.yaml --values my-generic-values.yaml
helm upgrade --install kars deploy/helm/kars \
  --namespace kars-system \
  --create-namespace \
  --values deploy/helm/kars/values-generic.yaml \
  --values my-generic-values.yaml
```

The generic overlay leaves AKS defaults untouched. It disables Azure identity
metadata and schedules sandboxes with the portable `kubernetes.io/os=linux`
selector. It still installs `profiles/kars-strict.json` for existing/default
enhanced sandbox CRs; Helm values do not change those CRD defaults. To use the
portable standard/RuntimeDefault posture, declare it explicitly in each CR as
shown below. Supply pullable images, inference
authentication, a NetworkPolicy-capable CNI, and any environment-specific
integrations. The profile deploys the Microsoft AGT AgentMesh relay and
registry; set `agentMesh.enabled=false` only when those services are managed
externally.

The shipped AgentMesh implementation supports the fixed `agentmesh` namespace
and one registry/relay replica each (zero is allowed for deliberate maintenance).
Other namespaces or multiple replicas are rejected rather than accepted with
broken routing or independent in-memory state. Updates use `Recreate`; expect
clients to reconnect during a mesh upgrade.

After creating a suitable `InferencePolicy` in `kars-system`, replace
`existing-inference-policy` with its name and submit a portable sandbox:

```yaml
apiVersion: kars.azure.com/v1alpha1
kind: KarsSandbox
metadata:
  name: portable-agent
  namespace: kars-system
spec:
  runtime:
    kind: OpenClaw
    openclaw: {}
  inferenceRef:
    name: existing-inference-policy
  sandbox:
    isolation: standard
    seccompProfile: RuntimeDefault
```

## Existing AKS

Copy the checked-in template that mirrors the values emitted by `kars up`:

```bash
cp deploy/helm/kars/values-existing-aks.yaml my-aks-values.yaml
# Replace every REPLACE_ME value.

kars schemas prepare --release kars --namespace kars-system \
  --chart deploy/helm/kars --values my-aks-values.yaml
helm upgrade --install kars deploy/helm/kars \
  --namespace kars-system \
  --create-namespace \
  --values my-aks-values.yaml
```

The template includes controller/router/sandbox and runtime images, Foundry
account/project/deployment values, Content Safety, Workload Identity, Key
Vault, kubelet IMDS identity, federated-credential metadata, AgentMesh, and the
release stamp. The referenced Azure resources and role assignments must already
exist.

The template uses the fixed public `ghcr.io/azure/*` image repositories. To
mirror images into a private ACR instead, replace the repository values, import
every referenced image, and grant the AKS kubelet identity `AcrPull`. Keep one
release tag across every component and set `karsRelease` to that tag; mixing
tags can create controller/router/runtime protocol drift.

The minimum Azure-side prerequisites are:

- AKS with OIDC issuer and Workload Identity enabled;
- public GHCR access, or an ACR containing every privately mirrored image
  referenced by your values;
- a federated controller managed identity and its client ID;
- Foundry/Azure OpenAI data-plane access for the identities used by the router;
- Key Vault/CSI permissions when `azure.keyVaultCsi.enabled=true`;
- NetworkPolicy-capable networking and nodes matching `sandbox.nodeSelector`.

After the required operator schema stage and Helm installation, Kars resources
can be submitted directly with `kubectl apply`. One-pass Helm installation alone
is not a deterministic schema-publication barrier for admission type checking.

## Register an existing AKS installation with the CLI

Kubernetes-facing commands recognize any explicit/current kube context.
Azure lifecycle commands also need the deployment metadata normally written by
`kars up`. After a manual Helm installation, register that metadata without
provisioning or changing infrastructure:

```bash
kars config adopt-aks \
  --subscription <subscription-id> \
  --region eastus2 \
  --resource-group <aks-resource-group> \
  --cluster <aks-cluster-name> \
  --context <kubectl-context>
```

The command verifies the Kars Helm release and `KarsSandbox` CRD, then writes
`~/.kars/context.json`. Add `--acr-login-server <registry>.azurecr.io` when
using a private mirror or when enabling `kars push` and the current ACR-based
`kars upgrade` flow. Optional Workload Identity, OIDC, Foundry, identity, and
Key Vault flags enable the corresponding advanced flows.

`kars push --apply` updates the owning image configuration and verifies the
pushed artifact, rather than merely restarting an old image. Explicit sandbox
image pins are preserved. `sandbox-base` is a build dependency, not a deployable
target; rebuild the sandbox image before applying it. Ordinary Helm image updates
first complete the chart's guarded SRE adoption with unchanged installation values
when needed, then turn off the first-install-only schema hook and keep the controller
digest aligned with the selected artifact. Existing SRE adoption, schema compatibility and
private-root protections still apply; this does not authorize protected-root upgrades.

Core upgrades leave external AgentMesh services untouched. Mesh image updates
are limited to the owning Kars Helm release or the standalone Kars deployment
marked `kars.azure.com/mesh-provider=agt`. Matching service/deployment names alone
do not authorize an update. With external mesh, select an explicit core
`kars push --only` target; the CLI will not adopt or replace external workloads.
