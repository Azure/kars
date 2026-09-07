# Install Kars with Helm

Use the Helm chart when the Kubernetes cluster, image access, inference backend,
and required identity configuration already exist. The chart can manage
AgentMesh or use an existing external deployment.

## Local kind

```bash
helm upgrade --install kars deploy/helm/kars \
  --namespace kars-system \
  --create-namespace \
  --values deploy/helm/kars/values-local-dev.yaml
```

Load all referenced development images into kind before installation.

## Existing non-AKS Kubernetes cluster

Start with the generic overlay and layer environment-specific values on top:

```bash
helm upgrade --install kars deploy/helm/kars \
  --namespace kars-system \
  --create-namespace \
  --values deploy/helm/kars/values-generic.yaml \
  --values my-generic-values.yaml
```

The generic overlay leaves AKS defaults untouched. It disables Azure identity
metadata and schedules sandboxes with the portable `kubernetes.io/os=linux`
selector. It still installs `profiles/kars-strict.json` for existing/default
enhanced sandbox CRs; Helm values do not change those CRD defaults. To use the
portable standard/RuntimeDefault posture, declare it explicitly in each CR as
shown below. Supply pullable images, inference
authentication, a NetworkPolicy-capable CNI, and any environment-specific
integrations. The profile deploys the Microsoft AGT AgentMesh relay and
registry; set `agentMesh.enabled=false` only when those services are managed
externally.

The shipped AgentMesh implementation supports the fixed `agentmesh` namespace
and one registry/relay replica each (zero is allowed for deliberate maintenance).
Other namespaces or multiple replicas are rejected rather than accepted with
broken routing or independent in-memory state. Updates use `Recreate`; expect
clients to reconnect during a mesh upgrade.

After creating a suitable `InferencePolicy` in `kars-system`, replace
`existing-inference-policy` with its name and submit a portable sandbox:

```yaml
apiVersion: kars.azure.com/v1alpha1
kind: KarsSandbox
metadata:
  name: portable-agent
  namespace: kars-system
spec:
  runtime:
    kind: OpenClaw
    openclaw: {}
  inferenceRef:
    name: existing-inference-policy
  sandbox:
    isolation: standard
    seccompProfile: RuntimeDefault
```

## Existing AKS

Copy the checked-in template that mirrors the values emitted by `kars up`:

```bash
cp deploy/helm/kars/values-existing-aks.yaml my-aks-values.yaml
# Replace every REPLACE_ME value.

helm upgrade --install kars deploy/helm/kars \
  --namespace kars-system \
  --create-namespace \
  --values my-aks-values.yaml
```

The template includes controller/router/sandbox and runtime images, Foundry
account/project/deployment values, Content Safety, Workload Identity, Key
Vault, kubelet IMDS identity, federated-credential metadata, AgentMesh, and the
release stamp. The referenced Azure resources and role assignments must already
exist.

The template uses the fixed public `ghcr.io/azure/*` image repositories. To
mirror images into a private ACR instead, replace the repository values, import
every referenced image, and grant the AKS kubelet identity `AcrPull`. Keep one
release tag across every component and set `karsRelease` to that tag; mixing
tags can create controller/router/runtime protocol drift.

The minimum Azure-side prerequisites are:

- AKS with OIDC issuer and Workload Identity enabled;
- public GHCR access, or an ACR containing every privately mirrored image
  referenced by your values;
- a federated controller managed identity and its client ID;
- Foundry/Azure OpenAI data-plane access for the identities used by the router;
- Key Vault/CSI permissions when `azure.keyVaultCsi.enabled=true`;
- NetworkPolicy-capable networking and nodes matching `sandbox.nodeSelector`.

The CLI remains optional. After Helm installation, Kars resources can be
submitted directly with `kubectl apply`.

All Kars CRDs are rendered by the chart and installed by Helm before the
controller begins reconciling custom resources.

## Register an existing AKS installation with the CLI

Kubernetes-facing commands recognize any explicit/current kube context.
Azure lifecycle commands also need the deployment metadata normally written by
`kars up`. After a manual Helm installation, register that metadata without
provisioning or changing infrastructure:

```bash
kars config adopt-aks \
  --subscription <subscription-id> \
  --region eastus2 \
  --resource-group <aks-resource-group> \
  --cluster <aks-cluster-name> \
  --context <kubectl-context>
```

The command verifies the Kars Helm release and `KarsSandbox` CRD, then writes
`~/.kars/context.json`. Add `--acr-login-server <registry>.azurecr.io` when
using a private mirror or when enabling `kars push` and the current ACR-based
`kars upgrade` flow. Optional Workload Identity, OIDC, Foundry, identity, and
Key Vault flags enable the corresponding advanced flows.
