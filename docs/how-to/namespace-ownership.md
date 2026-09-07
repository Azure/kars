# Sandbox namespace ownership (claim v1)

KarsSandbox CRs are namespaced, but their runtime namespace remains
`kars-<sandbox-name>`. Names must therefore be unique across workspaces when
claiming an unowned namespace. This feature adds ownership checks; it does not
rename namespaces, migrate workloads, change images, or introduce credential
sources.

## Before upgrading the controller

Use the CLI containing this feature **before** replacing the controller:

```sh
kubectl config current-context
kars namespace preflight
```

Preflight is read-only and works on generic Kubernetes as well as AKS. It reads
Sandbox CRs, namespace metadata, and Deployments; it never reads Secrets. API,
RBAC, malformed-response, and transport errors fail the check rather than
appearing as an empty cluster.

`kars upgrade` (including `--dry-run`), `kars up --upgrade`, and `kars sre install`
against an existing controller run the same check before changing the controller.
Direct Helm/GitOps upgrades must run it explicitly.
No `up` provisioning or operator defaults change. A healthy controller Deployment
alone does **not** prove that sandbox ownership migration will succeed.

The preflight must succeed for all existing Sandbox CRs. Resolve every failure
before upgrading. A conflict discovered later sets `Ready=False` and
`Degraded=True`, reason `NamespaceOwnershipConflict`; it leaves target resources
untouched. Existing running pods are not stopped by a failed adoption.

### Automatic legacy adoption

For an unclaimed namespace, the controller requires **all** of:

* Exactly one current Sandbox of that name across all workspaces.
* Its existing `status.namespace` identifies the target namespace, and its
  namespace-cleanup finalizer is already present.
* The namespace has controller-managed sandbox labels, including an `Apply`
  managedFields entry by `kars-controller/karssandbox` owning the sandbox label.
* Its same-named Deployment has matching sandbox labels and selector/pod labels,
  controller-managed spec fields, and no foreign ownerReferences. If the
  Deployment carries `kars.azure.com/parent-namespace`, it must match the CR.
* The Deployment was created **strictly after** this Sandbox incarnation.
  Equal timestamps are ambiguous because Kubernetes creation timestamps have
  second precision. A newer recreated Sandbox cannot adopt an older Deployment.

The namespace itself may predate the Sandbox: the legacy CLI created namespaces
to stage credentials before creating CRs. Proven deployments retain their
namespace UID, labels, workloads, pod templates, and all data. Adoption changes
only claim metadata and the Sandbox's namespace-UID backlink, without a rollout.

Names, labels, managedFields, or CR status **alone** are not ownership proof.
Missing evidence, older field-manager formats, same-second creation, overlay-only
deployments, and unfinished legacy prestaging require an explicit administrator
decision. Preflight rejects these rather than silently disabling them on upgrade.

### Explicit adoption of an ambiguous legacy namespace

First verify that the namespace and **all** its contents belong to the intended
Sandbox. Check workspace, deployment history, creation times, and audit records.
Back up customer data through your normal secure process. Do not use adoption to
transfer a previous Sandbox incarnation's credentials to a new agent.

Obtain the current identifiers without printing any Secret:

```sh
kubectl get karssandbox demo -n workspace-a \
  -o jsonpath='{.metadata.uid}{"\n"}'
kubectl get namespace kars-demo -o jsonpath='{.metadata.uid}{"\n"}'
kars namespace adopt demo --namespace workspace-a \
  --sandbox-uid <reviewed-sandbox-uid> --namespace-uid <reviewed-namespace-uid>
kars namespace preflight
```

The explicit adoption command is a privileged, metadata-only write. It requires a
unique live Sandbox, rejects foreign/partial claims, ownerReferences, terminating
resources, and stale reviewed UIDs, and uses namespace UID/resourceVersion
preconditions. It never deletes/recreates a namespace or copies credential data.
Only a namespace administrator should receive this permission.

If a namespace already has a claim for another UID/workspace, stop and resolve
the original owner's lifecycle. Do not remove or overwrite its claim annotations.
If a *bound* namespace disappears or is replaced, recovery also requires an
administrator to investigate: the controller will not silently create another
namespace under an existing backlink. For failed same-name contenders with
finalizers, verify that they never owned the namespace before manually removing
only their own cleanup finalizer; never delete the winning namespace.

## Claim contract

The namespace carries these reserved annotations:

| Annotation | Value |
|---|---|
| `kars.azure.com/namespace-claim-version` | `v1` |
| `kars.azure.com/sandbox-namespace` | Namespace containing the Sandbox CR |
| `kars.azure.com/sandbox-name` | Sandbox CR name |
| `kars.azure.com/sandbox-uid` | Exact live Sandbox UID |

The Sandbox carries `kars.azure.com/namespace-uid`, the exact target namespace
UID. These are namespace-controller/administrator authority, not tenant labels.
Namespace create is atomic. Existing claims must match both identities;
adoption and backlink writes use UID/resourceVersion checks. A 409 retries from
fresh API reads, never through force apply. Same-name reconciles are serialized
inside the controller, in addition to API-server concurrency checks.

Namespace watch events enqueue the annotated source CR, but are only routing
hints: live identity checks still authorize every reconciliation. Periodic
requeues provide a backstop. Egress-approval target writes/cleanup and router
admin-token reads use the same read-only claim check; policy confirmation keeps
the referring workspace rather than treating a bare Sandbox name as authority.
Run a single elected controller as usual; these
checks do not defend against a cluster administrator deliberately rewriting
namespace claims or bypassing Kubernetes lifecycle controls.

## Credential prestaging compatibility

Updated `kars add` and local-Kubernetes `kars dev` still create credentials
**before** the Sandbox CR/pod. For a
new namespace it writes an explicit v1 reservation with source namespace/name,
no sandbox UID yet, and
`kars.azure.com/namespace-prestage: bind-next-sandbox`. The CR CREATE includes
that namespace's UID backlink. Only that two-way reservation permits initial
binding. The controller atomically replaces the prestage intent with the live
Sandbox UID before touching workloads. Subsequent CR recreation cannot reuse it.
An interrupted explicit reservation can be resumed by `kars add`; unrelated or
unmarked namespaces cannot.

Existing running legacy deployments remain valid staging targets when the same
legacy proof succeeds. Unfinished **unmarked** prestaging from older clients
cannot be attributed safely: complete/verify those Sandbox workflows and record
an explicit administrator claim before upgrading. Inventory outstanding
pre-CR reservations separately (`kubectl get namespaces`); without a CR they are
not included in `namespace preflight`. Update provisioning clients to the v1
reservation contract before creating further pre-CR namespaces. Do not resolve
this gap by granting broader Secret permissions.

This feature does not change `credentials update/remove`, Secret contents,
EnvFrom behavior, or workspace/team credential merging. It is the namespace
safety prerequisite for a separate future credential-source feature.

The existing handoff credential writer also waits for the created Sandbox's
exact workspace/UID claim and namespace backlink. It establishes a metadata-only
Secret anchor, rechecks ownership, and writes values with Secret
UID/resourceVersion preconditions. Namespace or Secret replacement cannot turn
that update into a blind write to a new target. Failed or incomplete claims do
not authorize credential copying; errors are surfaced without credential values.

## Built-in SRE installation

Fresh `kars sre install` and Helm installations with `sre.enabled=true` let the
controller create and claim `kars-sre`. The chart's InferencePolicy, ToolPolicy
and Sandbox already reside in the Helm release namespace; they do not require
precreating the runtime namespace. The controller creates the existing
`sre-writer` ServiceAccount only after the namespace is claimed, with token
automount disabled. The SRE role/binding targets are unchanged.

On a real Helm upgrade, live lookup retains a Namespace and writer account owned
by that exact release. The Namespace also receives `helm.sh/resource-policy:
keep`, even when the upgrade disables SRE, so removing an old chart resource
cannot bypass controller cleanup. Customer metadata is preserved. Namespace
lookup errors stop rendering rather than silently dropping a possibly owned
resource. Other releases' resources are never imported.

Client-only `helm template` cannot discover previous Helm ownership. Do not
replace a legacy Helm release with a pruning GitOps deployment using a fresh
render: first perform the live Helm migration and preserve the retained namespace
in the GitOps ownership/pruning policy. Existing template/apply installations
must likewise keep previously managed runtime namespaces out of pruning; run
preflight and resolve any ambiguous legacy claim before updating the controller.

## Cleanup and rollback

Deletion rechecks ownership and namespace UID, then sends UID/resourceVersion
preconditions. The CR cleanup finalizer remains until deletion is accepted (or
the namespace is already gone) and the remaining required cleanup succeeds.
The controller does not wait for namespace disappearance: a CR inside that
namespace would otherwise deadlock its own deletion. Terminating namespaces
retain their claim and cannot be adopted by another Sandbox.
Non-404 failures retain the CR finalizer and retry;
a stuck namespace is diagnosed, not force-finalized. Other CR finalizers are
preserved.

A narrow legacy exception handles a Sandbox stored inside its own runtime
namespace when both are already terminating. Namespace GC may have removed the
Deployment before claim-v1 adoption. The controller then uses the live CR UID
and resourceVersion to release only its own CR cleanup finalizer and stops
reconciling; it does not adopt, write, or delete the unproven namespace or other
resources. Foreign/partial claims and recorded namespace-UID mismatches still
fail closed. This prevents a namespace-GC deadlock without inventing ownership.
Valid two-way v1 reservations are not legacy namespaces: cancellation during
binding follows the normal guarded cleanup path instead of the legacy shortcut.

Rolling back to a controller without claim-v1 support removes these protections:
older binaries ignore the claim metadata. Avoid provisioning, deleting, or
reusing sandbox names during such a rollback; resolve ownership and run preflight
before returning to the protected controller. No claim or namespace is
Helm-owned by this feature.
