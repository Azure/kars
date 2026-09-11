# Deployment

Kars Bridge is an additive Helm release installed into a compatible Kars
cluster. The chart deploys:

- the Rust BFF;
- the Next.js web application;
- the BFF ServiceAccount and ClusterRole;
- NetworkPolicies;
- optional ingress;
- optional in-cluster Dex for private-preview testing;
- optional Microsoft Teams gateway resources, with zero replicas by default.

It does not install Kars or provision a Kubernetes cluster.
Kars remains usable without Bridge. Install the complete compatible Kars
runtime first; the public foundation PRs alone are insufficient. This is
integration-preview deployment guidance, not a public image-availability or
release-readiness claim.

All commands below run from `bridge/` at the repository root (`cd bridge`).

## Support statement

| Environment | Status |
|---|---|
| AKS | Historical private-preview evidence only; current candidate not qualified |
| Local kind | Development/acceptance harness; current native gates must pass |
| EKS | Templates render; not live-qualified end to end |
| GKE | Templates render; not live-qualified end to end |
| Other Kubernetes | No blanket support claim |

“Cloud-agnostic templates” means the workloads use standard Kubernetes APIs.
It does not mean identity, ingress, registry, inference, CNI behavior, or all
Kars features work unchanged on every distribution.

## Prerequisites

- A [compatible Kars installation](compatibility.md).
- Kubernetes 1.30+ when using the default Kars admission controls.
- Operator-built Bridge images and registry pull access. Legacy chart repository
  defaults do not imply published public images.
- DNS and NetworkPolicy connectivity from web → BFF and BFF → Kubernetes API.
- An authentication mode selected deliberately.

## Install

Build the standalone application images with `make images REGISTRY=<registry>`.
The optional Teams image has a separate `make image-gateway REGISTRY=<registry>`
target. Build targets do not push images or change the cluster. Publish to your
chosen registry separately before installation.

The default `namespace: kars-system` and `createNamespace: false` join the
Kars-owned namespace without adding it to the Bridge release. Setting
`createNamespace: true` for `kars-system` is rejected on new installs, rather
than attempting to claim or replace the core namespace. Legacy chart-owned
namespaces remain in upgrade manifests so retention can be applied safely.

```bash
helm upgrade --install kars-bridge deploy/helm/kars-bridge \
  --namespace kars-system \
  --values my-values.yaml
```

Example values:

```yaml
namespace: kars-system

bff:
  image:
    repository: <registry>/kars-bridge-bff
    tag: latest
    pullPolicy: Always

web:
  image:
    repository: <registry>/kars-bridge-web
    tag: latest
    pullPolicy: Always

global:
  imagePullSecrets:
    - name: registry-pull
```

For an existing custom namespace, also leave `createNamespace: false`. For a
fresh dedicated namespace, keep it false and use Helm's `--create-namespace`
with matching chart `namespace` and Helm `--namespace` values. The namespace
then remains outside the release's resource ownership.

`createNamespace: true` is supported for a fresh workload namespace when the
Helm release is stored in a separate existing namespace, and for upgrades of
legacy chart-owned namespaces. A new release cannot bootstrap its own storage
namespace from a chart template: Helm must store release history before applying
that template. Chart-owned workload namespaces are retained on uninstall. Chart
placement in a custom namespace does not imply every multi-workspace workflow
is qualified.

Microsoft Teams is disabled by default (`teamsGateway.enabled: false`, rendered
replicas `0`). Its Secret references in the BFF are optional, so absent Entra
tenant/bot credentials do not block the web surface. Enable the gateway only
after its dedicated credentials and role mapping are configured; web OIDC
authentication is a separate requirement.

## Private-preview Dex

Dex is useful for a colleague test ring without a public ingress:

```yaml
idp:
  enabled: true
  issuer: http://localhost:3000/dex
  redirectURIs:
    - http://localhost:3000/auth/callback
```

```bash
kubectl -n kars-system port-forward svc/kars-bridge-web 3000:3000
```

The web application proxies `/dex/*` to the in-cluster Dex Service. Seed
password users are for private preview only. Replace them with real users or an
upstream IdP before broader use.

For ingress, set the externally reachable HTTPS issuer and callback URI. The
issuer is a security boundary and must match exactly.

## Production identity

Use a real OIDC provider such as Entra ID, Okta, Auth0, Keycloak, or an
enterprise Dex deployment. Store client and session secrets in a Kubernetes
Secret; never commit them to values.

Set `auth.principalSecretName` so the web session signing key and the BFF
principal-verification key are the same secret. External OIDC configuration
without BFF principal verification is not a production multi-user deployment.

See [Identity](identity.md).

## Validate

```bash
helm lint deploy/helm/kars-bridge
make helm-test
helm template kars-bridge deploy/helm/kars-bridge --values my-values.yaml \
  | kubectl apply --dry-run=client -f -
helm upgrade --install kars-bridge deploy/helm/kars-bridge \
  --namespace kars-system \
  --values my-values.yaml \
  --dry-run=server
```

After installation:

```bash
kubectl -n kars-system rollout status deploy/kars-bridge-bff
kubectl -n kars-system rollout status deploy/kars-bridge-web
kubectl auth can-i list karstasks.kars.azure.com \
  --as system:serviceaccount:kars-system:kars-bridge
```

Exercise at least one real mutation through the deployed BFF ServiceAccount.
Testing with a developer’s cluster-admin kubeconfig does not validate Bridge
RBAC.

The BFF readiness probe calls `/readyz`, not `/healthz`. A 503 means at least one
required Kars API cannot be read within five seconds; inspect BFF logs for the
kind and failure. Do not bypass readiness, weaken RBAC, or remove guardrail APIs
to make an incomplete core installation appear compatible. API readiness does
not replace the [source/image and workflow qualification](compatibility.md).

## Upgrade and rollback

```bash
helm upgrade kars-bridge deploy/helm/kars-bridge \
  --namespace kars-system \
  --values my-values.yaml

helm history kars-bridge -n kars-system
helm rollback kars-bridge <revision> -n kars-system
```

Upgrade Kars first when a Bridge release requires new CRD fields or controller
behavior. The compatibility matrix must identify the required order.

## Uninstall

```bash
helm uninstall kars-bridge -n kars-system
```

This removes Bridge workloads and chart-owned configuration without removing
Kars CRDs, missions, teams, receipts, or sandboxes. The recommended
`createNamespace: false` joins an existing namespace without owning it. For a
dedicated namespace with `createNamespace: true`, the namespace is annotated with
`helm.sh/resource-policy: keep`, so Helm leaves it behind rather than
cascade-deleting namespaced Kars resources. Remove an empty retained namespace
explicitly only after inspecting its contents.

The retention annotation must be present in the **installed release manifest**
before uninstalling. Upgrades inspect the configured namespace's Helm ownership
and retain it if this release already owns it, even when `createNamespace`
changes to false. The Helm caller therefore needs permission to get that
Namespace. Confirm `helm get manifest` contains `helm.sh/resource-policy: keep`
before uninstalling an upgraded legacy release. Do not move a legacy release
to a different workload namespace before applying retention to its old one:
removing an unprotected Namespace from an upgrade manifest can delete it too.
Do not uninstall an old revision or roll back to it and assume the new retention
behavior applies. Helm ownership checks still
apply; never use `--take-ownership` to transfer Kars-owned
resources to Bridge. Bridge-created runtime CRs and their evidence are not Helm
resources and remain for explicit operator lifecycle management.
