# Kars Helm chart

This chart installs the Kars CRDs, controller, RBAC, admission controls, default
policies, and optional operational components.

It does **not** provision a Kubernetes cluster, container registry, model
provider, AGT relay/registry, cloud identity, or AI Runway/KAITO.

## Support status

| Environment | Status |
|---|---|
| Local kind | Tested with `values-local-dev.yaml` |
| AKS | Primary tested deployment |
| EKS / GKE / other Kubernetes | Templates may render; full runtime support is not claimed without environment qualification |

See `docs/reference/compatibility.md` in the source checkout or published
documentation site.

## Prerequisites

- Kubernetes 1.30+ for the default admission policy set.
- Helm 3.
- Cluster-admin-equivalent permission for CRDs, cluster RBAC, and admission
  policies.
- Registry access to all selected images.
- A NetworkPolicy-capable CNI.
- An inference backend and authentication configuration.
- AGT AgentMesh relay and registry.
- For AKS Workload Identity: OIDC issuer enabled, a federated credential for
  the controller identity, and the required Azure RBAC.
- A Pod Security policy decision for the privileged datapath witness. Disable
  `datapathWitness.enabled` when the cluster will not grant that exception.
- A real `signerPolicy` issuer and SAN configuration; the placeholder tenant
  value is not production-ready.

## Local kind

```bash
helm lint deploy/helm/kars
helm upgrade --install kars deploy/helm/kars \
  --namespace kars-system \
  --create-namespace \
  --values deploy/helm/kars/values-local-dev.yaml
```

Load the development images into kind before installation or override every
image repository/tag with pullable images.

## Existing AKS cluster

```bash
helm upgrade --install kars deploy/helm/kars \
  --namespace kars-system \
  --create-namespace \
  --values my-values.yaml
```

At minimum, `my-values.yaml` should define:

```yaml
controller:
  image:
    repository: <registry>/kars-controller

inferenceRouter:
  image:
    repository: <registry>/kars-inference-router

sandbox:
  image:
    repository: <registry>/openclaw-sandbox

foundry:
  endpoint: https://<resource>.services.ai.azure.com
  projectEndpoint: https://<resource>.services.ai.azure.com/api/projects/<project>
  deployments: '["gpt-4.1"]'

azure:
  workloadIdentity:
    enabled: true
    clientId: <managed-identity-client-id>
```

Configure runtime images under `runtimes.*.image` when using adapters other than
the default runtime.

## AgentMesh

The chart configures Kars to use the `agt` mesh provider but does not install
the relay and registry. Install the matching AGT stack separately:

```bash
kubectl apply -f deploy/agentmesh-agt.yaml
```

## Validate

```bash
helm lint deploy/helm/kars
helm template kars deploy/helm/kars --values my-values.yaml >/tmp/kars.yaml
kubectl apply --dry-run=client -f /tmp/kars.yaml
kubectl -n kars-system rollout status deploy/kars-controller
kubectl -n kars-system get crd | grep kars
```

## Important defaults

- Kars standardizes component defaults on `:latest` with
  `imagePullPolicy: Always`; do not introduce independent component version
  tags that can drift.
- Azure integrations are enabled in the default values and must be reviewed for
  non-Azure clusters.
- `admission.seccompAutoStamp` is disabled because it requires Kubernetes 1.34
  and a beta feature gate.
- The managed Everything MCP image is a conformance fixture, not a production
  integration.
