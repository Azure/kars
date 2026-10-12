<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Deploying kars Bridge

kars Bridge is an **additive** layer on top of [kars](https://github.com/Azure/kars):
kars runs on its own; the Bridge deploys the operator console + workspace (BFF + web)
and the least-privilege RBAC the BFF needs — it never replaces any kars component.

**Public source integration:** Bridge is published in **Azure/kars** through
the **`kars-bridge`** integration branch, but is **not yet release-qualified**.
There is no public Bridge image/release matrix yet. The
[compatibility document](../../../docs/compatibility.md) separates the current
integration candidate from historical preview qualification; source publication
alone does not qualify the full runtime required by Bridge.

The chart uses standard Kubernetes workloads. Historical **AKS** and **local
kind** preview results do not qualify the current public candidate. EKS and GKE
also require environment-specific identity, registry, ingress, CNI, inference,
and compatibility validation.

## Prerequisites

Run the commands below from `bridge/`, entered from the repository root (`cd bridge`).

- A Kubernetes cluster (new or existing) with the **kars CRDs + controller** installed:
  ```bash
  helm install kars ../deploy/helm/kars -n kars-system --create-namespace
  ```
- The Bridge images, pushed to a registry your cluster can pull (or loaded into kind).

## Build the images

```bash
# BFF (Rust) — repository-root context includes shared/; run from bridge/
docker build -f bff/Dockerfile -t <registry>/kars-bridge-bff:<tag> ..
# Web (Next.js standalone) — build context is web/
docker build -f web/Dockerfile -t <registry>/kars-bridge-web:<tag> web
docker push <registry>/kars-bridge-bff:<tag>
docker push <registry>/kars-bridge-web:<tag>
```

## Install modes

### Sharing Dex with Core Headlamp

The optional `idp.headlampClient` registers a separate confidential OIDC client
in the existing Dex instance. Set its `clientID` and `callbackURL` to the same
values as Core's `headlamp.oidc.clientID` and `headlamp.oidc.callbackURL`. Provision
matching client-secret values in existing Secrets in the Bridge and `headlamp`
namespaces; configure each chart's Secret reference. The chart does not copy or
render the secret value and does not grant any Kubernetes permissions.

This requires an HTTPS `idp.issuer`, reachable by the browser, Headlamp, and the
Kubernetes API server. Either terminate TLS at an existing trusted ingress, or
enable `idp.dex.tls.enabled` and reference an existing `kubernetes.io/tls` Secret
with `idp.dex.tls.secretName`. Native Dex TLS listens on Service port **5557**;
the existing HTTP **5556** Bridge proxy/backchannel remains unchanged. The
certificate must cover the issuer hostname. Rotate the certificate or client
Secret with a reviewed Dex restart; the chart does not inspect Secret contents.

For a private CA, configure Core `headlamp.oidc.caConfigMap` and API-server OIDC
trust explicitly. Local split-horizon DNS can use Core `headlamp.hostAliases`;
the API server and browser must independently resolve that same issuer.
For a Kind static API-server Pod, a node's `/etc/hosts` entry alone is not
sufficient: configure the issuer mapping in the Pod's `hostAliases` as well
(or provide DNS that resolves correctly inside that Pod). Keep the issuer CA
and hostname verification enabled.

With Cilium and a host-network API server on another node, namespace selectors
do not grant that server access to Dex. Set
`networkPolicy.dexApiServerIngress=true` for native Dex TLS. This adds a Cilium
policy allowing the `kube-apiserver` identity and nodes labeled
`node-role.kubernetes.io/control-plane=""` to Dex TCP **5557**, not the HTTP
backchannel or arbitrary nodes. Kind's kube-proxy can SNAT API-server traffic
to its node's Cilium address, losing the `kube-apiserver` identity; the
control-plane-node rule preserves a narrow allow-list in that case. Enable
Cilium `nodeSelectorLabels=true` to make this rule effective. For Cluster Mesh,
also use `clustermesh.policyDefaultLocalCluster=true` to restrict node selection
to the local cluster. Do not replace this with `remote-node` or `cluster` access.
It requires the CiliumNetworkPolicy API,
`networkPolicy.create`, and `idp.dex.tls.enabled`; it is off by default.
Check actual API-server discovery/JWKS access and per-user resource access,
not merely a successful browser login.

Bridge also uses authenticated Kubernetes `pods/proxy` requests to sandbox
routers. On a multi-node Cilium cluster, set
`networkPolicy.routerApiServerIngress=true`. This requires
`networkPolicy.create` and the CiliumClusterwideNetworkPolicy API. It permits
only the same API-server/control-plane identities to TCP **8443** on sandbox
Pods in sandbox-role namespaces, including future Mission and Team namespaces.
The control-plane node-label and Cluster Mesh prerequisites above also apply.
It does not expose runtime gateway ports, add RBAC privileges, or change egress
isolation. It is off by default for other CNIs. Verify an actual authenticated
`GET /api/v1/namespaces/<ns>/pods/<pod>:8443/proxy/healthz` request; a Ready router
or controller-confirmed policy alone does not prove this distinct Bridge path.
The separate router admin token is still required for `/internal/policy-status`;
Kubernetes proxy access alone must not grant administrative router access.

Browser trust can be confined to a dedicated development browser profile;
never disable certificate verification. Neither chart installs a host CA,
changes API-server authentication, or creates per-user RBAC bindings. Those
operator prerequisites are separate from enabling the client and listener.

The defaults join the existing `kars-system` namespace with
`createNamespace: false`. New installs cannot claim `kars-system`; upgrades
preserve a legacy chart-owned namespace so retention can be applied safely.
An existing custom namespace must also use `createNamespace: false`. For a new
dedicated namespace, keep it false and use Helm's `--create-namespace` with
matching Helm `--namespace` and chart `namespace` values. Helm creates its
storage namespace before saving the release, without owning it as a chart
resource. Advanced deployments may use `createNamespace: true` for a fresh
workload namespace only when Helm stores the release in a separate existing
namespace; the chart annotates that workload namespace
`helm.sh/resource-policy: keep` to prevent cascading deletion on uninstall.
See [upgrade/uninstall caveats](../../../docs/deployment.md#uninstall) before
removing an older release.

Microsoft Teams is optional: the gateway defaults to zero replicas and the
BFF's Teams Secret references are optional. Missing tenant credentials do not
block a Kars-backed web-only deployment.

### Bridge alone, on an EXISTING kars cluster (the common case)

```bash
helm install kars-bridge deploy/helm/kars-bridge -n kars-system \
  --set bff.image.repository=<registry>/kars-bridge-bff \
  --set web.image.repository=<registry>/kars-bridge-web \
  --set bff.image.tag=<tag> --set web.image.tag=<tag>
```

### kars + Bridge together, on a NEW cluster

```bash
helm install kars       ../deploy/helm/kars -n kars-system --create-namespace
helm install kars-bridge deploy/helm/kars-bridge -n kars-system   # additive
```
(or `make helm-install` — see the Makefile.)

### Credential-free workspace enrollment

Creating missions and teams requires a current, UID-bound
`KarsCredentialGrant/workspace`, even when no private credentials are selected.
For a new credential-free workspace, add this explicit option to the **second
Helm command** above:

```bash
helm install kars-bridge deploy/helm/kars-bridge -n kars-system \
  -f <your-bridge-values.yaml> --set workspaceEnrollment.enabled=true
```

This creates only metadata: no writers, agent keys, integration stores, GitHub
connections, or private activation authority. The installing operator needs the
Core `manage` permission on that grant (in addition to Helm installation rights).
No operator role is bound by this option; BFF RBAC is unchanged. Core must already
be installed. Helm looks up the actual namespace UID; offline `helm template`
cannot fabricate it. Use a cluster-connected install/upgrade or
`helm template --dry-run=server` for enrollment validation.

The workspace defaults to `core.namespace`, independently of Bridge's workload
namespace. Set `workspaceEnrollment.namespace` only for another existing
workspace. Wait for the controller's `Ready` condition at the grant's current
generation before creating work. `PrivateConsumptionReady` remains false; this
does not qualify private credentials or an agent execution route.

Leave enrollment disabled for existing operator-managed grants. The template
refuses adoption, namespace replacement, disabled/terminating grants, or any
expanded authority. Disable enrollment through Helm **before** separately
reviewed private enrollment. The grant is retained on disable/uninstall
(`helm.sh/resource-policy: keep`); uninstall is not credential revocation.
Explicit retirement must follow the
[governed-grant lifecycle](../../../../docs/how-to/governed-credential-grants.md).

## Cloud-specific values

| Cloud | Image registry | Ingress class | Notes |
|-------|----------------|---------------|-------|
| **AKS**  | `<acr>.azurecr.io` | `webapprouting.kubernetes.azure.com` or `nginx` | `az acr login`; Workload Identity for the controller. |
| **EKS**  | `<acct>.dkr.ecr.<region>.amazonaws.com` | `alb` or `nginx` | Template guidance only; not live-qualified. |
| **GKE**  | `<region>-docker.pkg.dev/<project>/<repo>` | `gce` or `nginx` | Template guidance only; not live-qualified. |
| **kind** | locally loaded (`kind load docker-image`) | `nginx` (ingress-nginx) | `--set *.image.pullPolicy=IfNotPresent`; reach via `kubectl port-forward`. |

Enable ingress with, e.g. on EKS:
```bash
helm install kars-bridge deploy/helm/kars-bridge -n kars-system \
  --set ingress.enabled=true --set ingress.className=alb \
  --set ingress.host=bridge.example.com
```

## Reach the surfaces

With no ingress, port-forward the web Service (it proxies `/api/*` to the BFF):
```bash
kubectl -n kars-system port-forward svc/kars-bridge-web 3000:3000
# http://localhost:3000/workspace   (users)
# http://localhost:3000/console     (operators / admins)
```

## Validate before installing

```bash
helm lint deploy/helm/kars-bridge
make helm-test
helm template kars-bridge deploy/helm/kars-bridge | kubectl apply --dry-run=client -f -
helm install kars-bridge deploy/helm/kars-bridge -n kars-system --dry-run=server
```

`make helm-test` uses the existing `teams-gateway` Vitest runner and requires
Helm and that package's development dependencies; it does not contact a
cluster. BFF `/readyz` also requires all fourteen documented Kars APIs and
list permission, with a five-second total budget. The chart uses a ten-second
readiness timeout and keeps `/healthz` for liveness. Neither test proves
controller behavior or replaces a qualified source/image matrix.

The PR workflow additionally runs an actual Helm install, upgrade and uninstall
against disposable Kind. It checks resource UIDs and retained data for both an
existing shared namespace and a chart-owned workload namespace. It does not
launch Kars or Bridge images and is not full runtime acceptance.

The separate native credential workflow exercises real core/Bridge images and
credential rebind. Its continuity contract preserves Task/Sandbox/namespace
identities, namespace-owned resources and current authorization/receipts.
The existing core mounts `/sandbox` as `emptyDir`: files survive a container
restart in the same Pod, but not replacement of that Pod. This release does not
add persistent workspaces or PVC migration. The native case checks that storage
mode explicitly rather than claiming filesystem durability from namespace
retention. Credential revocation, old-consumer retirement, TLS/CNI and all
required outcomes remain mandatory.
After recording Team rebind continuity, the harness explicitly unlaunches that
UID-pinned test Task and waits for normal controller cleanup before creating
the independent observer fixture. This releases the completed fixture's CPU
reservation on the bounded Kind worker without changing production requests,
node taints, scheduling policy or observer readiness requirements.

## Security

The BFF verifies the authenticated Bridge principal and enforces persona routes.
The `kars-bridge` ServiceAccount then limits the aggregate Kubernetes operations
the product can perform. Both layers are required. Most credential Secrets are
write-only. The BFF has narrowly scoped `get` access to the canonical provider
Secrets so it can report connection metadata; credential values are never
returned to browsers or agents.
