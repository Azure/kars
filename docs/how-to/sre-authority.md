# Registered SRE authority and credential privacy

SRE namespace occupancy is not authorization. The cluster-scoped
`KarsSRERegistration/canonical` is the operator trust root. It pins the exact
controller Deployment and controller namespace UIDs, owning release, canonical
`sre` Sandbox namespace/name/UID, and `kars-sre` namespace UID.

The `kars-sre-registrar` ClusterRole is **unbound**. Only an explicit cluster
administrator delegation gives an operator registration permissions.
Namespace administrators, agents, and the Bridge BFF receive none by default.
Installers also need their normal core/Helm deployment permissions.
Kubernetes must support stable `admissionregistration.k8s.io/v1`
ValidatingAdmissionPolicy and CEL authorizer checks. The controller refuses
privilege until all fourteen policies are observed, type-checked without
warnings, and bound with `Deny`.

## Fresh installation

Install the core prerequisite controller and router first. SRE remains disabled
by default. A registrar can then run:

```sh
kars sre install --namespace kars-system --release kars
```

The CLI stages only supporting policies and a genuinely new source CR, using
atomic CREATE and its returned UID. A racing existing CR is never fetched and
adopted. It waits for that UID's complete namespace claim and backlink before
enrollment. No privileged RoleBinding is emitted by Helm.

If either the source or runtime namespace already exists, use explicit review
below instead. A foreign `kars-sre` occupant remains untouched and receives no
privilege.

## Existing installation: stage, review, enroll

Normal `up`, `up --upgrade`, `upgrade`, local-Kubernetes `dev`, and `push --apply`
stop before Kubernetes deployment changes when legacy SRE grants remain.
Staging is the explicit registrar operation that installs the prerequisite
controller/API while retaining old grants unchanged:

```sh
kars sre authority stage --namespace kars-system --release kars \
  --controller-image <qualified-controller-repository>:<tag> \
  --router-image <qualified-router-repository>:<tag> --dry-run
```

Review the output, then run the same command without `--dry-run`. Staging does
not enroll an occupant or issue private SRE credentials.

SRE installation and staging inspect all Helm release statuses. Helm 3 uses
`list --all`; Helm 4 removed that flag and lists all statuses by default. The
CLI retries without it only after that exact flag error and a confirmed Helm 4
version. Other discovery failures remain errors, not an absent release or
permission to install over existing resources.

```sh
kars sre authority preview --namespace kars-system --release kars
kars sre authority enroll --namespace kars-system --release kars \
  --sandbox-uid <reviewed-source-uid> --namespace-uid <reviewed-runtime-namespace-uid> \
  --binding 'ClusterRoleBinding//kars-sre-reader=<uid>@<resourceVersion>' \
  --binding 'ClusterRoleBinding//kars-sre-action-author=<uid>@<resourceVersion>' \
  --consumer '<deployment-uid>@<resourceVersion>' --dry-run
```

Use **every** binding review printed by preview, not just the illustrative
names above. Updating an existing registration additionally requires
`--registration-uid` and `--resource-version`. After review, omit `--dry-run`
and wait for migration:

```sh
kars sre authority migrate --namespace kars-system --release kars
```

The controller validates the full review set, then submits every planned
retirement PATCH to the API server with `dryRun=All` before applying any of
them. Each dry-run and real PATCH uses the identical reviewed UID/resourceVersion,
retirement annotation and surviving subjects. Only the legacy SRE subject is
removed; unrelated subjects and the binding's roleRef remain intact. The
reviewed old consumer is stopped only after all binding retirements succeed.
Broad group grants and unreviewed resources stop migration; the operator must
restructure those grants explicitly.

### Binding authorization and custom roles

Kubernetes applies RBAC privilege-escalation checks even to a **subtractive**
ClusterRoleBinding or RoleBinding update. Ordinary `patch` permission and
registrar `use` are not sufficient: the controller must already hold the
referenced permissions or have the appropriate named `bind` permission.
The chart grants `bind` on exactly `kars-sre-reader`,
`kars-sre-private-diagnostics`, `kars-sre-action-author` and `kars-sre-router-renew`.
It does not grant wildcard `bind`, `escalate`, cluster-admin, or automatic
authority over custom roles.

A reviewed custom role can therefore fail server-side preflight. A failure
such as `Preflight reviewed SRE ClusterRoleBinding retirement: Kubernetes status 403`
leaves all legacy bindings and the consumer unchanged on that initial attempt.
The controller relies on the actual API server's RBAC, admission and validation
decisions; it does not simulate those checks or grant itself missing authority.
An operator must inspect the referenced role and explicitly resolve the grant:
restructure or retire it, or separately authorize only the exact required
role-specific binding permission after review. Never add broad bind/escalate
rights merely to make migration pass. Refresh enrollment UID/resourceVersion
reviews if an operator changes the reviewed resources.

Preflight is **not a multi-resource transaction**. RBAC, admission or object
versions can change before a real PATCH. Every real write still enforces its
original UID/resourceVersion and current API authorization, and errors stop the
attempt. Earlier successful retirements can remain applied; they are recognized
on retry, not rolled back by restoring old privileges. The consumer is not
stopped and private credentials are not issued while retirement remains incomplete.

Real Kubernetes authorization reviews must deny the old SRE principal Secret
get/list/watch access before private credentials are issued. The shared
`shared/sre_privacy.rs` contract checks both namespace and cluster scope,
including generic access and name-restricted access to the two protected
Secrets. Both issuance and each proxy authorization run these live checks;
an earlier `Ready` status boolean is not sufficient. Namespace/source
recreation, API errors, or incomplete claims fail closed. No ServiceAccount
delete/recreate shortcut revokes the legacy identity.

The controller also rotates proven owned `router-services-admin` credentials,
if any exist, and restarts their owned consumers. Their absence on the
prerequisite base is normal. Later governed-service issuers must call
`sre_authority::privacy_epoch` immediately before issuance.

### Legacy token Secrets and watch-only grants

The private `sre-api-router` identity must **never** use legacy
`kubernetes.io/service-account-token` Secrets. Admission denies CREATE and
UPDATE for arbitrary Secret names when either the old or new object combines
that type with `kubernetes.io/service-account.name=sre-api-router`. There is no
registrar or Kubernetes token-controller exception. Normal bound TokenRequest
renewal remains supported.

Before creating the private ServiceAccount, granting authority, or issuing
credentials, the controller inventories Secret **metadata only**. Prestaged
aliases are rejected even if already populated, carrying an obsolete SA UID,
or annotated as another type. A renamed alias retaining the current or
registered SA UID is also rejected. Incomplete inventories and API errors fail
closed. Unknown Secret values are never read for this inspection, logged,
adopted, or deleted; an operator must review and resolve the conflicting objects.

Discovery after a prior `Ready` revokes owned grants, retires owned private
credentials/identity, and clears the published privacy evidence. Foreign
objects and unrelated binding subjects remain intact. The Pod's `sandbox`
ServiceAccount and Azure federated subject are never deleted or replaced.
Recovery requires clean live checks and new bound credentials; replacement of
the owned Secret UID invalidates tokens from a previous privacy epoch. That UID
also drives consumer rollout, independently of TLS expiry.

Privacy revision `kars.azure.com/sre-privacy/v2` prevents an old get/list-only
status from authorizing the new proxy or issuance helper. Watch-only and
wildcard Secret grants are included in controller and CLI legacy inventory;
unreviewed/group grants block migration before private issuance. The router
identity alone receives narrowly scoped authorization-review creation
permission for the live check. This API is not exposed through the agent proxy.

## Helm and GitOps sequencing

1. Install the prerequisite core/chart with `sre.enabled=false` for a fresh
   installation. For an existing Helm-owned enabled SRE, use the explicit
   `authority stage` workflow above: server-side lookup retains the exact
   legacy source and grant UIDs/resourceVersions rather than re-rendering or
   replacing them. Do not use offline `helm template` to guess those identities.
2. Wait for the CRD and admission policies to converge. Delegate the unbound
   registrar role explicitly to the operator performing enrollment. Do not
   grant it to the tenant GitOps controller or SRE agent.
3. For a fresh source, run `kars sre authority stage-source`, which atomically
   CREATEs the CR and reports its actual source/namespace UIDs. For an existing
   source, run `authority preview` instead; no source adoption by name occurs.
4. Review the `authority enroll --dry-run` JSON and exact binding/consumer
   reviews. CREATE that cluster-scoped registration under registrar authority,
   or apply an explicitly reviewed UID/resourceVersion-fenced update. The CLI
   performs these operations; a GitOps operator may commit the same reviewed
   specification but must not omit the API identity checks when applying it.
5. Wait for `Ready` with matching `observedGeneration`, then enable
   `sre.enabled=true,sre.authorityStage=false` in the managed release.
   GitOps pruning must preserve the chart's retained authority, registration,
   legacy-binding, and namespace resources through this sequence.

Non-Helm/template installs use the same CLI stage/enroll workflow; staging
validates all authority resources first and CAS-updates only controller image
and router image configuration, retaining unrelated settings. Different
unowned authority objects require explicit operator resolution, not force
adoption. Standard install/upgrade commands are not a migration bypass.

## Existing Hermes images remain compatible

The Pod still uses ServiceAccount `sandbox`, preserving its Azure Workload
Identity federated subject and existing IMDS behavior. The router alone receives
the ordinary projected Kubernetes credential. A separate `sre-api-router`
identity, scoped to the registered namespace incarnation, performs diagnostic
API access and renews its own Secret-bound short-lived TokenRequests.

The agent receives **no Kubernetes JWT**. Its existing paths contain an opaque,
agent-safe proxy credential, loopback CA, and namespace:

- `/var/run/secrets/kubernetes.io/serviceaccount/token`
- `/var/run/secrets/kubernetes.io/serviceaccount/ca.crt`
- `/var/run/secrets/kubernetes.io/serviceaccount/namespace`

Standard `KUBERNETES_SERVICE_HOST/PORT` point to `https://127.0.0.1:9446`.
Pinned Hermes clients continue using HTTPS, CA verification, raw pod-log GETs,
and proposal POSTs without an image-specific fallback. Azure token projection
is excluded from the agent container. The old apiserver egress bypass is gone.
Upstream Pod-log requests use API-compatible media negotiation; the facade
still returns only the bounded plain-text log response.
Admission protects both direct Pod mounts and Deployment/ReplicaSet/Job and
CronJob templates from laundering a private mount through Kubernetes workload
controllers. Exec/attach/port-forward into the private SRE runtime requires
registrar authority. Cluster-wide workload controllers remain trusted;
installing a custom privileged controller is a cluster-operator action.
The Deployment-controller handoff is authorized only for ReplicaSet requests
and requires cluster-wide `apps/replicasets` CREATE authority; namespaced
workload permissions are insufficient. It does not grant the Deployment
controller Pod CREATE or registrar authority.

The registered router's API egress includes the canonical Service IP and its
ready HTTPS endpoint IP/port pairs, since policy enforcement can see the
post-DNAT destination rather than the Service VIP. These are exact `/32` or
`/128` targets, not private-subnet allowances; malformed, missing, terminating,
or oversized endpoint inventories fail closed. The controller fetches
`default/kubernetes`, with only named `kubernetes` Endpoints GET added to its
authority role, and refreshes registered SRE reconciliation every 30 seconds.
The UID-1000 egress guard and opaque agent credential are unchanged.

The proxy checks current registration and live UID/claim authority. It permits
the bounded first-party diagnostic read/log/metrics paths and Pending-only
`KarsSREAction` creation in `kars-sre`. Secret responses retain key names but
exclude values, `stringData`, annotations, labels, and managed-field copies.
The typed `SecretList` envelope supplies item types when Kubernetes omits item
`kind`/`apiVersion`; conflicting explicit type metadata is still rejected.
Encoded/noncanonical paths, watches, streaming log follow, token requests,
exec/proxy subresources, arbitrary writes, and non-JSON media escapes are denied.
The existing Hermes proposal builder's two diagnostic labels are accepted;
owner references, status injection, arbitrary metadata, and approval fields
other than exactly `{"state":"Pending"}` are rejected.

Limits: 16 concurrent operations, 64 KiB proposal bodies, 8 MiB JSON responses,
and 256 KiB logs. First-party watchers poll rather than stream. Private
credential renewal never falls back to the agent or Pod's less-privileged
credential. TLS rotation changes the opaque token and CA together and rolls
the SRE consumer; the old client rebuilds its TLS client on token change.

## Retirement and rollback

```sh
kars sre authority retire --registration-uid <uid> --resource-version <version>
kars sre uninstall --namespace kars-system --release kars
```

Retirement revokes owned private grants and credentials before source cleanup.
The registrar-authorized controller first quiesces and UID/resourceVersion-
fences deletion of its owned SRE Deployment; it does not leave that protected
object for the unprivileged namespace controller. Foreign/replaced consumers
and external finalizers are preserved, not adopted or forced. Retained Retired
records can finish this owned cleanup without reissuing authority.
Uninstall/destroy refuse an active enrollment or unretired legacy grants.
Retired registrations remain audit records; recreating the source requires
explicit enrollment of the new UIDs (`authority stage-source` can atomically
stage a genuinely new source).

Do not roll back to a release that restores agent-held Kubernetes credentials
or legacy SRE grants. CLI rollback is rejected while registration exists, and
retained admission policies reject restoration of the old privileged subject.
Use a reviewed roll-forward release. Do not prune retained authority/admission
objects through GitOps without an operator-reviewed retirement.

This boundary assumes the controller namespace and runtime infrastructure are
operator controlled. Cluster administrators can change RBAC/admission itself;
deliberately bypassing those controls is not defended by a container mount.
