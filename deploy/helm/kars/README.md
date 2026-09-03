# Kars Helm chart

This chart installs the Kars CRDs, controller, RBAC, admission controls,
policies, and optional operational components into an existing Kubernetes
cluster. It does not provision the cluster, registry, inference backend, cloud
identity, or AGT relay/registry.

## Support status

| Environment | Status |
|---|---|
| Local kind | Tested with `values-local-dev.yaml` |
| AKS | Primary tested deployment |
| EKS, GKE, and other Kubernetes | Chart-render tested with `values-generic.yaml`; runtime support depends on operator-provided integrations |

## Existing non-AKS cluster

The generic overlay disables Azure Workload Identity metadata and Azure-only
chart settings, uses RuntimeDefault seccomp, and replaces the AKS-specific
sandbox pool selector with the standard Linux node label.

```bash
helm upgrade --install kars deploy/helm/kars \
  --namespace kars-system \
  --create-namespace \
  --values deploy/helm/kars/values-generic.yaml \
  --values my-generic-values.yaml
```

`my-generic-values.yaml` must provide pullable images, an inference endpoint
and router-side authentication, a reachable AGT relay/registry, and any
environment-specific secret-store, policy, monitoring, ingress, and signing
integrations. A NetworkPolicy-capable CNI is required.

The overlay is opt-in and does not change existing AKS defaults.

## Validate

```bash
helm lint deploy/helm/kars
helm template kars deploy/helm/kars \
  --namespace kars-system \
  --values deploy/helm/kars/values-generic.yaml >/tmp/kars.yaml
```

Install the matching AGT stack separately before creating sandboxes:

```bash
kubectl apply -f deploy/agentmesh-agt.yaml
```
