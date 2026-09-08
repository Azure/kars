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

Router egress-operator access is a separate optional delegation of GET on the
existing `router-admin-token` in verified runtime namespaces. It is not an
agent source, does not grant a Secret list, and never falls back to unauthenticated
operator calls.

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
