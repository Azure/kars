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
# BFF (Rust) — build context is bff/
docker build -f bff/Dockerfile -t <registry>/kars-bridge-bff:<tag> bff
# Web (Next.js standalone) — build context is web/
docker build -f web/Dockerfile -t <registry>/kars-bridge-web:<tag> web
docker push <registry>/kars-bridge-bff:<tag>
docker push <registry>/kars-bridge-web:<tag>
```

## Install modes

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
