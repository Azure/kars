# Governed credential sources and operator stores

This additive contract does not require Bridge. Direct credentials and the
existing ten-key `credentialsRef` v1 flow remain unchanged unless explicitly
selected for migration.

## Authority

`KarsCredentialGrant/workspace` is a **metadata-only**, namespaced operator
delegation. It pins the workspace UID, writer ServiceAccount UIDs, permitted
agent key names, and each enrolled integration Secret's exact name/UID/purpose.
There are no credential values in the CRD. The operator ClusterRole is unbound;
Bridge cannot author or widen its grant.

Core creates source-only writer Roles behind fail-closed admission. The
parameter-independent source boundary continues to restrict Secret creation
even while a grant is being deleted. Native `resourceNames` entries are exact
names, never wildcard patterns. Values remain Opaque Kubernetes Secrets.

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

## Operator workflow

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

Prelaunch sources remain unbound. Bridge stages Tasks/Teams without runnable
execution, captures the actual CREATE UID, attaches the source selections, and
only then requests activation. A CREATE conflict is never converted to adoption.
Core verifies current Task authority before preparing a UID-owned bundle and
the existing UID-fenced runtime projection. Agent values never enter router
EnvFrom. Runtime environment overrides of selected keys are rejected.

Missing selected keys mask lower-priority values. Removing a key does not remove
the binding or restore direct credentials. Missing/replaced/revoked authority
stops the credential consumer and clears only its owned projection. Previously
governed consumers do not silently return to the old direct collection.

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

Keyless mode requires explicit governed agent sources, rejects opaque GitHub
egress, and currently rejects raw GitHub/custom agent credential combinations
without a separate purpose review. This is not a migration of legacy bare
Sandbox credentials. Operator-approved custom credentials remain usable in
the existing explicitly unbounded standalone mode; that mode is **not**
repository-enforced by the GitHub gateway.

The GitHub runtime consumer checkpoint must be forward-integrated and jointly
qualified before this candidate can be used. A mount is not evidence that a
particular router image contains that consumer.

Grant finalization revokes its owned writer/operator bindings. Namespace and
source UID checks prevent adopting a replacement. Source cleanup follows its
actual target UID; workspace sources and operator stores are not Helm-owned and
remain after Bridge uninstall. Legacy stores remain for explicit review.

Kubernetes reconciliation is asynchronous. Permission, node or API failures
can delay consumer termination and revocation; this does not revoke a token at
its external provider or erase values an agent already observed.

This candidate still requires coordinated Rust and real API/admission lifecycle
qualification before release. The Bridge app remains private; this core
contract is not permission to publish that application or its images.

Outstanding qualification boundaries include ServiceAccount recreation while
native Secret-read Roles exist, and live observation RPC privacy checks beyond
registration status plus GET/LIST/WATCH denials. The issuer calls the full
strict helper; the RPC currently does not repeat the controller's admission
and private-SA token-alias inventory. TLS, CA integrity, projected private
volumes, Kubernetes admission and control-plane integrity remain trust
dependencies. Do not claim complete end-to-end UID/privacy qualification yet.
