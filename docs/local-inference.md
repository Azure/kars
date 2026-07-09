# Local (in-cluster) inference providers

kars can route a mission or sub-agent to a model running **inside your own
cluster** — no external API, no per-token billing, no egress dependency on
a third-party inference provider. This is powered by two real, upstream,
open-source projects:

- **[AI Runway](https://github.com/kaito-project/airunway)** — a unified
  `ModelDeployment` CRD. You give it a model id + optional GPU request; its
  controller picks the right engine (vLLM / SGLang / TensorRT-LLM /
  llama.cpp) and provider (KAITO / Dynamo / KubeRay) automatically.
- **[KAITO](https://github.com/kaito-project/kaito)** (Kubernetes AI
  Toolchain Operator) — the provider AI Runway delegates to whenever no GPU
  is requested (CPU inference via [AIKit](https://github.com/kaito-project/aikit)/llama.cpp),
  or when you're on a GPU node pool.

kars does **not** install or manage either project. You install them once,
the same way you'd install any other cluster addon — using their own real
`helm`/`kubectl` commands, with your own cluster-admin kubeconfig. kars-bridge
only detects that they're present and builds a normal, narrowly-scoped
`ModelDeployment` CRUD flow on top — exactly like it detects an
already-configured GitHub App or Azure AI Foundry connection, rather than
configuring those itself.

## Tier 0 — CPU-only, works everywhere (recommended default)

This is the easiest path: a small model, no GPU required, works identically
on a local `kind` cluster and on AKS with no GPU node pool at all. **Verified
live** on a plain single-node `kind` cluster with these exact commands.

### 1. Install AI Runway's controller + CRDs

```bash
# Pin to a real release tag — never `main`.
kubectl apply -f https://raw.githubusercontent.com/kaito-project/airunway/v0.7.0/deploy/controller.yaml
kubectl apply -f https://raw.githubusercontent.com/kaito-project/airunway/v0.7.0/providers/kaito/deploy/kaito.yaml
```

### 2. Install KAITO's workspace controller

```bash
helm repo add kaito https://kaito-project.github.io/kaito/charts/kaito
helm repo update kaito

helm upgrade --install kaito-workspace kaito/workspace --version 0.11.0 \
  --namespace kaito-workspace --create-namespace \
  --set clusterName="$(kubectl config current-context)" \
  --set featureGates.disableNodeAutoProvisioning=true \
  --set nodeProvisioner=byo
```

> **The one flag KAITO's own docs don't mention clearly.** Setting
> `featureGates.disableNodeAutoProvisioning=true` alone is **not** enough —
> the admission webhook still unconditionally requires
> `resource.instanceType` unless `--node-provisioner` is **also** set to the
> literal string `byo` (a third valid value beyond `azure-gpu-provisioner`/
> `karpenter` that the chart's `values.yaml` never mentions or defaults to —
> found only by reading the controller's own `main.go` flag definitions).
> Without `nodeProvisioner=byo`, every CPU/BYO-node deployment fails with:
> `instanceType is required when node auto-provisioning is enabled`, even
> though auto-provisioning is explicitly disabled.

Two DaemonSets from this chart (`csi-local-node`, the local-NVMe CSI driver)
will show `CrashLoopBackOff`/`Error` on a cluster with no local NVMe devices
(e.g. `kind`, or any AKS node pool without a local disk). This is expected —
that component is only for NVMe-backed model caching, an optional
optimization the CPU/small-model path doesn't need. The
`kaito-workspace` deployment itself (the actual controller) reaching
`1/1 Running` is what matters.

### 3. Deploy a tiny model

```bash
kubectl label node <your-node-name> apps=llm-inference

cat <<'EOF' | kubectl apply -f -
apiVersion: airunway.ai/v1alpha1
kind: ModelDeployment
metadata:
  name: local-llama-1b
  namespace: default
spec:
  model:
    id: "llama-3.2-1b-instruct"
  engine:
    type: llamacpp
  image: "ghcr.io/kaito-project/aikit/llama3.2:1b"
  nodeSelector:
    apps: llm-inference
EOF
```

Watch it come up:

```bash
kubectl get modeldeployment local-llama-1b -w
# PHASE goes Deploying -> Running (first run pulls the ~860MB image)
```

### 4. Verify

```bash
CLUSTERIP=$(kubectl get svc local-llama-1b -o jsonpath='{.spec.clusterIP}')
kubectl run curl-test --rm -i --restart=Never --image=curlimages/curl -- \
  curl -s -X POST http://$CLUSTERIP:80/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model":"llama-3.2-1b-instruct","messages":[{"role":"user","content":"say hi"}],"max_tokens":20}'
```

Other small CPU-tier models from AIKit's
[pre-made image list](https://kaito-project.github.io/aikit/docs/premade-models/):
`ghcr.io/kaito-project/aikit/llama3.2:3b`, `.../gemma2:2b`. Larger ones (8B+)
will work but are noticeably slower without a GPU.

## Tier 1 — an existing AKS GPU node pool

If you already have GPU nodes (`az aks nodepool add --node-vm-size
Standard_NC6s_v3 ...`), the same `nodeProvisioner=byo` install above works —
just label the GPU nodes and use a `spec.resources.gpu` request with a real
vLLM-served model instead of the CPU/llamacpp path:

```bash
kubectl label node <gpu-node-name> apps=llm-inference
```

```yaml
apiVersion: airunway.ai/v1alpha1
kind: ModelDeployment
metadata:
  name: local-phi4-mini
spec:
  model:
    id: "microsoft/Phi-4-mini-instruct"
  resources:
    gpu:
      count: 1
      type: "nvidia.com/gpu"
  nodeSelector:
    apps: llm-inference
```

The controller auto-selects the `vllm` engine here (GPU requested), and KAITO
schedules onto your labeled node — no Azure IAM setup needed beyond the node
pool itself.

## Tier 2 — GPU auto-provisioning (advanced, not required)

KAITO can auto-provision GPU nodes on demand via Azure's
[`gpu-provisioner`](https://github.com/Azure/gpu-provisioner) (Karpenter-based).
This needs an Azure **managed identity with a Contributor role on the
resource group** and a **federated credential** — real Azure subscription-level
IAM that no in-cluster ServiceAccount should ever hold, so it stays a
separate, manual, one-time Azure CLI runbook. See
[KAITO's Azure auto-provisioning guide](https://kaito-project.github.io/kaito/docs/azure/)
for the exact steps; kars has no involvement in this tier at all beyond the
same `ModelDeployment` + `spec.resources.gpu` shape working unchanged once
it's set up.

## What kars-bridge does on top of this

Once AI Runway's `modeldeployments.airunway.ai` CRD is detected in the
cluster, the Bridge's provider wizard offers a **"Local model (in-cluster)"**
card: a curated list of the Tier 0/Tier 1 models above, plus a free-text
HuggingFace model id for advanced use. Deploying one creates a
`ModelDeployment`; once it reports `Running`, the Bridge auto-registers its
Service endpoint as a normal additional inference provider (no API key
needed) — every sandbox's `InferencePolicy.modelPreference` can then route to
it exactly like any other connected provider.
