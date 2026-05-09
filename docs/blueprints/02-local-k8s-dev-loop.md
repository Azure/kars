# Blueprint 02 — Local Kubernetes dev loop

> *"I'm on my laptop. I want production-shaped infrastructure — kind cluster, CRDs, controller, sidecar router, NetworkPolicies, Headlamp dashboard — without standing up AKS. When I'm done, one command tears it all down."*

## Persona & intent

- **You are:** an AzureClaw maintainer working on the controller, the inference router, the CRD schema, or the Headlamp plugin. Or an agent author who wants to see exactly what AKS will do to your `ClawSandbox` before you push to AKS.
- **You want:** the same Helm chart, the same CRDs, the same controller image, the same router image — running locally on a kind cluster — with a dashboard that shows you everything in one place.
- **You do not want:** to maintain a separate "Docker dev" code path that drifts from the K8s code path.

This blueprint is the **K8s-shaped** developer loop. For the lighter-weight single-Docker-container loop, see [Blueprint 01](01-developer-inner-loop.md). Both are first-class — pick the one that matches the change you're testing.

## Topology

```mermaid
flowchart TB
  subgraph Laptop["💻 Your laptop"]
    CLI["azureclaw dev --target local-k8s"]
    Browser["browser → http://localhost:4466"]
    subgraph Kind["kind cluster (azureclaw-dev)"]
      direction TB
      subgraph SystemNs["azureclaw-system ns"]
        Controller["azureclaw-controller<br/>(Rust kube-rs operator)"]
        DevSecret["azureclaw-dev-creds Secret<br/>(mounted creds + provider)"]
      end
      subgraph SandboxNs["azureclaw-&lt;name&gt; ns"]
        Pod["sandbox pod<br/>┌───────────────┐<br/>│ openclaw      │<br/>│ inference-router │<br/>└───────────────┘"]
      end
      subgraph HeadlampNs["headlamp ns"]
        Headlamp["headlamp<br/>+ AzureClaw plugin"]
      end
      Controller -->|reconciles ClawSandbox CRDs| Pod
      DevSecret -.->|envFrom| Pod
    end
    CLI -->|kubectl apply / helm template| Kind
    CLI -->|builds + ConfigMap-mounts| Headlamp
    Browser --> Headlamp
  end
  Copilot["🤖 GitHub Copilot / Foundry / GitHub Models"]
  Pod -.->|via inference-router| Copilot
```

## Trust boundary

The trust boundary is **identical to AKS in all respects except node isolation**:

- ✅ Sidecar router as the only egress path.
- ✅ NetworkPolicy isolating sandbox namespace.
- ✅ ServiceAccount + RBAC per sandbox.
- ✅ Strict seccomp profile from the chart (Linux + macOS arm64 / amd64).
- ✅ Same `azureclaw-controller` and `azureclaw-inference-router` images that AKS runs (just locally built and `kind load`-ed).
- ⚠️ **Single-node kind cluster** — no cross-node isolation. The control-plane node is labelled `azureclaw.azure.com/pool=sandbox` so sandboxes schedule, with **no NoSchedule taint** (the lone node has to host everything).
- ⚠️ **No Workload Identity.** Credentials are mounted from the dev secret (api-key mode for Foundry, GitHub PAT for Copilot/Models). Production AKS uses IMDS-exchanged tokens.

## Primary flow

```mermaid
sequenceDiagram
  participant Dev as Developer
  participant CLI as azureclaw CLI
  participant Kind as kind cluster
  participant Ctrl as azureclaw-controller
  participant HL as Headlamp + plugin
  Dev->>CLI: azureclaw dev --target local-k8s
  CLI->>Kind: kind create cluster
  CLI->>Kind: docker build && load (controller, router, sandbox)
  CLI->>Kind: helm template | kubectl apply --server-side
  CLI->>Kind: create azureclaw-dev-creds Secret (Foundry / Copilot / Models)
  CLI->>HL: helm install headlamp
  CLI->>HL: ConfigMap + volumeMount (azureclaw plugin)
  CLI->>Dev: prints Headlamp URL + 24h SA token
  Dev->>Kind: kubectl apply -f examples/basic-agent/clawsandbox.yaml
  Ctrl->>Kind: reconciles → namespace, NetworkPolicy, Deployment
  Dev->>HL: opens browser → AzureClaw sidebar → ClawSandbox detail
  Dev->>CLI: azureclaw dev down
  CLI->>Kind: kind delete cluster
```

## What you provision

```bash
# 1. Build images locally (one-time per change to controller / router / sandbox).
make image-controller image-inference-router
make sandbox-image  # or `azureclaw push --only sandbox` against a local registry

# 2. Bring everything up (~2 min first run, ~30 s on re-runs).
azureclaw dev --target local-k8s

# 3. Apply a sandbox CR.
kubectl apply -f examples/basic-agent/clawsandbox.yaml -n azureclaw-system

# 4. Watch reconciliation in Headlamp's AzureClaw → Sandboxes view.

# 5. When done.
azureclaw dev down                      # delete cluster + kill port-forward
azureclaw dev down --keep-cluster       # only stop port-forward
```

The CLI handles:

- Kind cluster create (idempotent — re-runs reuse).
- Cross-arch image load (kind load → fallback `<runtime> save | ctr images import`).
- Container runtime auto-detection (docker → podman → nerdctl, in order of preference). Override with `AZURECLAW_DEV_RUNTIME=docker|podman|nerdctl`. kind is invoked with `KIND_EXPERIMENTAL_PROVIDER` set automatically when needed.
- Node label `azureclaw.azure.com/pool=sandbox` so sandboxes schedule on the single node.
- Helm chart render with a per-run overlay containing the dev secret name.
- Headlamp install via official chart.
- Build + ConfigMap-mount the AzureClaw Headlamp plugin.
- Detached `kubectl port-forward` for Headlamp on `:4466` (survives CLI exit).
- Browser open + 24h ServiceAccount token print.

## What is unique

| Property | This blueprint | Blueprint 01 (Docker dev) | Blueprint 02 (AKS) |
|---|---|---|---|
| Runtime | kind cluster | Single Docker container | Managed AKS |
| CRDs / controller | ✅ Real | ❌ Skipped (CLI generates equivalent config inline) | ✅ Real |
| NetworkPolicy / RBAC | ✅ Real | ❌ One process, one netns | ✅ Real |
| Inference router | Sidecar pod | Co-located in same container | Sidecar pod |
| Auth mode | API key from dev secret | API key from dev secret | Workload Identity (IMDS) |
| Dashboard | Headlamp + AzureClaw plugin | `azureclaw connect` (gateway port-forward) | Azure Portal + Headlamp |
| Teardown | `azureclaw dev down` | `azureclaw destroy <name>` | `azureclaw destroy <name>` |

The point is: this blueprint validates **the K8s glue** (controller reconciliation, CRD admission, helm chart, NetworkPolicy) before you ever touch AKS. If `azureclaw dev --target local-k8s` is green, the AKS bring-up is almost guaranteed to be green too.

## References

- Implementation: `cli/src/commands/dev/local-k8s.ts`
- Headlamp plugin: `tools/headlamp-plugin/`
- Helm chart overlay: `deploy/helm/azureclaw/values-local-dev.yaml`
- CRDs: `deploy/helm/azureclaw/templates/crd-*.yaml`
- Strict seccomp profile: `deploy/helm/azureclaw/files/azureclaw-strict.json`
