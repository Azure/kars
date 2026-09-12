# Governed credential sources and operator stores

This additive contract does not require Bridge. Direct credentials and the
existing ten-key `credentialsRef` v1 flow remain unchanged unless explicitly
selected for migration.

## Authority

Private activation compares each live consumer against its reviewed owner
template. Kubernetes' two standard, admission-injected 300-second
`NoExecute` tolerations do not make that execution different. Explicit
tolerations, nonstandard durations, duplicate entries and every credential,
container and host-authority change still require exact review; custom
admission mutations are not silently ignored.

`KarsCredentialGrant/workspace` is a **metadata-only**, namespaced operator
delegation. It pins the workspace UID, writer ServiceAccount UIDs, permitted
agent key names, and each enrolled integration Secret's exact name/UID/purpose.
There are no credential values in the CRD. The operator ClusterRole is unbound;
Bridge cannot author or widen its grant.

Core creates source-only writer Roles behind fail-closed admission. The
parameter-independent source boundary continues to restrict Secret creation
even while a grant is being deleted. Native `resourceNames` entries are exact
names, never wildcard patterns. Values remain Opaque Kubernetes Secrets.

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
ServiceAccounts or arbitrary environment variables.

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
Core adds only the receiver-scoped runtime ingress policy. Existing BFF egress
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
The complete target request, including body/error decoding, suppresses raw
library logging; the caller emits bounded diagnostics outside that scope.
Sibling requests keep their own logging and progress state.

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

Preview is read-only and exports metadata/digests, never private values or raw
templates. Review the referenced root and consumer templates before applying.
Additional `--private-consumer namespace/Kind/name` entries can protect an
owned runtime without granting observation access to it. An unexplained Pod,
an ownership change, a different execution template, or an incomplete inventory
blocks activation; it is not deleted or adopted to make qualification pass.
Unrelated non-consuming Pods are preserved.

Apply rechecks the complete enforcing policy/binding specifications and their
current type-check/observation status. When updating a grant, that grant's existing
writer authority is retired first, including absence checks for its owned read
Roles/Bindings; another workspace's grant is not reset.

For a verified late-enrollment v2 Task runtime, retirement may temporarily
withdraw Task authorization while the controller observes the new grant
generation. Apply waits up to 120 seconds for genuine re-attestation under the
exact quiescent grant. Task/Sandbox identity and intent, source data, private
material and executable templates remain pinned. An observed projection
revocation requires a fresh refill revision distinct from both the original
and empty revisions, consumed by the owned Deployment. Only those proven
controller metadata transitions can advance; this is not a new user review,
stale-digest reuse or an arbitrary revision refresh. Already-qualified scopes
retain their independently verified path. Missing witnesses or other drift
preserve retirement and require explicit recovery; no new authority is published.
The projection recheck aligns kubectl JSON and JSONPath views only for
`managedFields` absent from the captured JSON view. Originally captured
`managedFields` and all other metadata remain compared; this does not grant
ownership or weaken source, value, revision or template checks.

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

Re-preview after a CAS conflict or lost response resumes the recorded attempt,
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
not overwritten or adopted. Repeat for the operator stores in use, including
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

`--github-review <file>` accepts a metadata-only array of reviewed connections:
`connection:{name,uid}`, `appSecret:{name,uid}`, `appId`, `ownerSubject`,
`installationId`, canonical `repositories`, and `write`. The App store must
also be explicitly enrolled with purpose `github-app`. Preview/apply recheck
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
