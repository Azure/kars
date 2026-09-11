# Kars Helm chart

This chart installs the Kars CRDs, controller, RBAC, admission controls,
policies, and optional operational components into an existing Kubernetes
cluster. It does not provision the cluster, registry, inference backend, cloud
identity, or inference credentials. The generic profile also deploys the
Microsoft AGT AgentMesh relay and registry from the public Kars release images.

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
kars schemas prepare --release kars --namespace kars-system \
  --chart deploy/helm/kars \
  --values deploy/helm/kars/values-generic.yaml \
  --values my-generic-values.yaml

helm upgrade --install kars deploy/helm/kars \
  --namespace kars-system \
  --create-namespace \
  --values deploy/helm/kars/values-generic.yaml \
  --values my-generic-values.yaml
```

`my-generic-values.yaml` must provide pullable images, an inference endpoint
and router-side authentication, and any environment-specific secret-store,
policy, monitoring, ingress, and signing integrations. A NetworkPolicy-capable
CNI is required.

The overlay is opt-in and does not change existing AKS defaults.

The schema preparation step is required before the first admission installation;
a single Helm invocation cannot order asynchronous CRD OpenAPI publication ahead
of policy type checking. Use the same chart, release, namespace, context and
values as the following Helm operation. The public helper installs only exact
owned CRDs and verifies Established, resource discovery, hashed OpenAPI v3
documents and resolvable declared types. It does not install policies or grant
writer authority. See [the schema lifecycle](../../../docs/how-to/helm-installation.md#required-schema-before-admission-stage)
for ownership, upgrade and already-failed-policy bounds.

To use an externally managed AgentMesh deployment instead, set:

```yaml
agentMesh:
  enabled: false
```

## Validate

```bash
helm lint deploy/helm/kars
helm template kars deploy/helm/kars \
  --namespace kars-system \
  --values deploy/helm/kars/values-generic.yaml >/tmp/kars.yaml
```

CRDs remain in the chart's tracked templates, not Helm's install-only `crds/`
directory. Pre-created CRDs carry the exact intended Helm ownership, and the
following Helm operation records/manages them normally. Later upgrades use the
same schema preflight; they do not delete CRDs or customer resources.

For an existing AKS cluster, run `kars config adopt-aks` after Helm installation
to write the local deployment context used by `kars upgrade`, `kars push`, and
mesh lifecycle commands. Kubernetes-only commands already use the selected
kube context directly.

Start from `values-existing-aks.yaml`; it mirrors the Helm values written by
`kars up` after provisioning and marks every customer-specific value with
`REPLACE_ME`. It uses the fixed public GHCR repositories. When a private mirror
is required, replace those repositories, import every image into the selected
ACR, and grant the AKS kubelet identity `AcrPull`. Stamp `karsRelease` with the
exact common release tag before installation.
