<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Blueprint 02 — Local Kubernetes dev loop

> *"I'm on my laptop. I want production-shaped infrastructure — kind cluster, CRDs, controller, sidecar router, NetworkPolicies, Headlamp dashboard — without standing up AKS. When I'm done, one command tears it all down."*

## Persona & intent

- **You are:** a kars maintainer working on the controller, the inference router, the CRD schema, or the Headlamp plugin. Or an agent author who wants to see exactly what AKS will do to your `KarsSandbox` before you push to AKS.
- **You want:** the same Helm chart, the same CRDs, the same controller image, the same router image — running locally on a kind cluster — with a dashboard that shows you everything in one place.
- **You do not want:** to maintain a separate "Docker dev" code path that drifts from the K8s code path.

This blueprint is the **K8s-shaped** developer loop and the **recommended primary dev flow** for kars: it reproduces the production pod shape, `NetworkPolicy`, UID split, and the controller reconciliation loop, so what's green here is green in AKS. For the lighter-weight single-Docker-container fast inner loop, see [Blueprint 01](01-developer-inner-loop.md). Both are first-class — start in local-k8s for anything you intend to ship, and drop to Docker only for quick prompt or tool-policy iteration.

The **Headlamp plugin** is the operational UX for this loop: a browser dashboard (`http://localhost:4466`) with a dedicated **kars sidebar** that surfaces every `KarsSandbox` and its sibling CRDs, live reconciliation status, pod health, and events — so you watch the controller do its work instead of polling `kubectl`.

## Topology

```mermaid
flowchart TB
  subgraph Laptop["💻 Your laptop"]
    CLI["kars dev --release --target local-k8s"]
    Browser["browser → http://localhost:4466"]
    subgraph Kind["kind cluster (kars-dev)"]
      direction TB
      subgraph SystemNs["kars-system ns"]
        Controller["kars-controller<br/>(Rust kube-rs operator)"]
        DevSecret["kars-dev-creds Secret<br/>(mounted creds + provider)"]
      end
      subgraph SandboxNs["kars-&lt;name&gt; ns"]
        Pod["sandbox pod<br/>┌─────────────────────┐<br/>│ agent (openclaw │<br/>│   or hermes …)      │<br/>│ inference-router    │<br/>└─────────────────────┘"]
      end
      subgraph HeadlampNs["headlamp ns"]
        Headlamp["headlamp<br/>+ kars plugin"]
      end
      Controller -->|reconciles KarsSandbox CRDs| Pod
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
- ✅ Same `kars-controller` and `kars-inference-router` images that AKS runs (just locally built and `kind load`-ed).
- ⚠️ **Single-node kind cluster** — no cross-node isolation. The control-plane node is labelled `kars.azure.com/pool=sandbox` so sandboxes schedule, with **no NoSchedule taint** (the lone node has to host everything).
- ⚠️ **No Workload Identity.** Credentials are mounted from the dev secret (api-key mode for Foundry, GitHub PAT for Copilot/Models). Production AKS uses IMDS-exchanged tokens.

## Primary flow

```mermaid
sequenceDiagram
  participant Dev as Developer
  participant CLI as kars CLI
  participant Kind as kind cluster
  participant Ctrl as kars-controller
  participant HL as Headlamp + plugin
  Dev->>CLI: kars dev --release --target local-k8s
  CLI->>Kind: kind create cluster
  CLI->>Kind: build/pull + load images (controller, router, sandbox) via the detected runtime
  CLI->>Kind: helm template | kubectl apply --server-side
  CLI->>Kind: create kars-dev-creds Secret (Foundry / Copilot / Models)
  CLI->>HL: helm install headlamp
  CLI->>HL: ConfigMap + volumeMount (kars plugin)
  CLI->>Dev: prints Headlamp URL + 24h SA token
  Dev->>Kind: kubectl apply -f examples/basic-agent/karssandbox.yaml
  Ctrl->>Kind: reconciles → namespace, NetworkPolicy, Deployment
  Dev->>HL: opens browser → kars sidebar → KarsSandbox detail
  Dev->>CLI: kars dev down
  CLI->>Kind: kind delete cluster
```

## What you provision

For clusters newly created by the CLI, each verified Kind node gets a
128 MiB runtime journal limit (16 MiB per file). This prevents RAM-backed
system logs from consuming a large share of a bounded node's memory.
CPU and memory limits, log levels and rate limits are unchanged. Existing
clusters are reused without changing their journal configuration or history;
plain `kind create cluster` does not apply this CLI setting.

```bash
# 1. Build images locally (one-time per change to controller / router / sandbox).
make image-controller image-inference-router
make sandbox-image  # or `kars push --only sandbox` against a local registry

# 2. Bring everything up (~2 min first run, ~30 s on re-runs).
kars dev --release --target local-k8s

# 3. Apply a sandbox CR.
kubectl apply -f examples/basic-agent/karssandbox.yaml -n kars-system

# 4. Watch reconciliation in Headlamp's kars → Sandboxes view.

# 5. When done.
kars dev down                      # delete cluster + kill port-forward
kars dev down --keep-cluster       # only stop port-forward
```

The CLI handles:

- Both default harness images: OpenClaw and Hermes are built locally or pulled from the selected release, then loaded alongside the controller, router and metrics server. Missing required images or images that do not match the host architecture stop installation before Core is applied. A selected release must include native images for both harnesses; the installer does not silently run an emulated runtime.
- OpenClaw manifests from `kars add` and local-Kubernetes dev share the packaged `2026.9.8` baseline. The Docker dev summary labels it as a baseline, not a detected version of an arbitrary image override. Regression checks bind this default to the existing Docker and plugin pins; changing the metadata does not rebuild or upgrade an installed image.
- Explicit local Hermes configuration through `values-local-dev.yaml` (`runtimes.hermes.image: kars-runtime-hermes:latest`), so the composer does not depend on access to the controller's private-registry fallback. Existing private-named local Hermes images can be retagged during loading. This makes the harness selectable; it does not certify completed work or inter-harness collaboration.
- With explicit `--no-build`, building and image loading remain skipped: the caller must already have loaded the exact configured images into Kind.
- The public release workflow builds Hermes on native AMD64 and ARM64 runners, then merges and signs its multi-architecture manifest using the existing sandbox release stage. Per-architecture SBOMs remain separate. These build records are not a substitute for installed inter-harness acceptance.
- Kind cluster create (idempotent — re-runs reuse).
- Native image load with exact CRI identity verification on every node returned by the selected Kind cluster, including workers. Missing or stale images trigger a sequential `<runtime> save | ctr images import` fallback only on the affected node; a failed worker verification stops installation. An empty or invalid node inventory is an error, not a successful load.
- Container runtime auto-detection (docker → podman → nerdctl, in order of preference). Override with `KARS_DEV_RUNTIME=docker|podman|nerdctl`. kind is invoked with `KIND_EXPERIMENTAL_PROVIDER` set automatically when needed.
- Node label `kars.azure.com/pool=sandbox` so sandboxes schedule on the single node.
- Helm chart render with a per-run overlay containing the dev secret name.
- Headlamp install via official chart.
- Build + ConfigMap-mount the kars Headlamp plugin.
- Detached `kubectl port-forward` for Headlamp on `:4466` (survives CLI exit).
- Monitoring links are discovered in the selected context. Grafana is optional: when its service is absent, the CLI reports it as not installed instead of advertising a broken URL or anonymous access. Missing monitoring services do not trigger port-forward replacement; discovery permission and connection failures remain errors.
- Browser open with the configured Core Headlamp sign-in instructions, or a 24h ServiceAccount token for a standalone Headlamp installation.

## What is unique

| Property | This blueprint | Blueprint 01 (Docker dev) | Blueprint 03 (AKS) |
|---|---|---|---|
| Runtime | kind cluster | Single Docker container | Managed AKS |
| CRDs / controller | ✅ Real | ❌ Skipped (CLI generates equivalent config inline) | ✅ Real |
| NetworkPolicy / RBAC | ✅ Real | ❌ One process, one netns | ✅ Real |
| Inference router | Sidecar pod | Co-located in same container | Sidecar pod |
| Auth mode | API key from dev secret | API key from dev secret | Workload Identity (IMDS) |
| Dashboard | Headlamp + kars plugin | `kars connect` (gateway port-forward) | Azure Portal + Headlamp |
| Teardown | `kars dev down` | `kars destroy <name>` | `kars destroy <name>` |

The point is: this blueprint validates **the K8s glue** (controller reconciliation, CRD admission, helm chart, NetworkPolicy) before you ever touch AKS. If `kars dev --release --target local-k8s` is green, the AKS bring-up is almost guaranteed to be green too.

## References

- Implementation: `cli/src/commands/dev/local-k8s.ts`
- Headlamp plugin: `tools/headlamp-plugin/`
- Helm chart overlay: `deploy/helm/kars/values-local-dev.yaml`
- CRDs: `deploy/helm/kars/templates/crd-*.yaml`
- Strict seccomp profile: `deploy/helm/kars/files/kars-strict.json`

---

_Last tested with kars `v0.1.18` on 2026-06-26._
