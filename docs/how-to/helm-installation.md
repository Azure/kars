# Install Kars with Helm

Use the Helm chart when the Kubernetes cluster, registry, inference backend,
identity, and AgentMesh services already exist.

The chart alone is not a full cloud provisioner.

## Local kind

```bash
helm lint deploy/helm/kars
helm upgrade --install kars deploy/helm/kars \
  --namespace kars-system \
  --create-namespace \
  --values deploy/helm/kars/values-local-dev.yaml
```

Load all referenced images into the kind cluster before installation.

## Existing AKS

Create an override file containing the controller, router, sandbox, runtime,
managed-MCP, Foundry, and Workload Identity settings.

```bash
helm upgrade --install kars deploy/helm/kars \
  --namespace kars-system \
  --create-namespace \
  --values my-values.yaml
```

Then install AGT AgentMesh:

```bash
kubectl apply -f deploy/agentmesh-agt.yaml
```

## Cluster policy considerations

- Kubernetes 1.30+ is required for the default admission controls.
- The datapath witness is privileged and may require a Pod Security exemption;
  disable `datapathWitness.enabled` where that exception is not acceptable.
- Customize `signerPolicy` before production use.
- Non-Azure clusters must disable or replace Azure-specific identity and CSI
  settings.

The source checkout also includes `deploy/helm/kars/README.md` next to the
chart. The rendered documentation site uses this page as the canonical Helm
guide.
