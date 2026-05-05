# Getting started

This guide takes you from a clean machine to a working AzureClaw agent in two steps:

1. **[Local — five minutes](#step-1--local-five-minutes)** — `azureclaw dev` runs a sandbox in one Docker container on your laptop. No Azure subscription, no AKS, no Kubernetes.
2. **[AKS — half an hour](#step-2--deploy-to-aks)** — `azureclaw up` provisions AKS + ACR + Foundry + the AzureClaw control plane in your subscription, and runs the same sandbox under Workload Identity, NetworkPolicies, and the egress guard.

The sandbox YAML you wrote in step 1 runs unchanged in step 2. That is the whole point.

---

## Prerequisites

| For | You need |
|---|---|
| Local mode | Docker Desktop (or any OCI runtime), Node.js 22+, Rust 1.88+, an Azure OpenAI endpoint + deployment + key. |
| AKS mode | The above, plus the [Azure CLI](https://learn.microsoft.com/cli/azure/) (`az`), [`kubectl`](https://kubernetes.io/docs/tasks/tools/), [Helm 3.14+](https://helm.sh/), and an Azure subscription where you can create resource groups. |

The CLI bootstraps everything else (Helm chart install, Foundry resource creation, ACR build/push, federated identity wiring). You do not need to provision any of it by hand.

---

## Step 1 — Local (five minutes)

### 1.1 Build the CLI

```bash
git clone https://github.com/Azure/azureclaw.git
cd azureclaw/cli
npm ci && npm run build
npm link    # exposes `azureclaw` on your PATH
```

The CLI is a Node 22 ESM build with a small Rust dependency for the local router. `npm run build` compiles both.

### 1.2 Launch a sandbox

```bash
azureclaw dev --name hello
```

On the first run you are prompted for:

- **Endpoint** — your Azure OpenAI / Foundry endpoint, e.g. `https://my-resource.openai.azure.com`.
- **Deployment name** — the model deployment you want the agent to use, e.g. `gpt-4.1`.
- **API key** — a resource-level key. **This is the only credential local mode ever sees, and it is mounted from a local secret file — it never leaves your machine.**

The CLI then builds (or pulls cached) the local sandbox image and starts a single container. In dev mode the agent runtime and the inference router are co-located in that one image — there is no separate router pod, no init container, no NetworkPolicy. You get the same router code path, the same governance profile, the same audit format.

### 1.3 Talk to the agent

```bash
azureclaw connect hello   # opens the TUI
```

Or drive it from another terminal:

```bash
azureclaw list               # see running sandboxes
azureclaw logs hello -f      # tail logs (router + agent)
azureclaw policy show hello  # what is allowed / denied / approval-gated
```

When you are done:

```bash
azureclaw destroy hello
```

### 1.4 What you just ran

The local sandbox is the right place to:

- Author plugins / tools and watch them go through the policy decision point.
- Iterate on `ToolPolicy` and `InferencePolicy` YAML before you push it to a cluster.
- Run smoke tests in CI without standing up Kubernetes.

It is **not** the right place to run multi-tenant workloads, accept untrusted prompts at scale, or rely on hardware-isolated execution. Those are AKS-mode properties.

A side-by-side breakdown of what is and is not isolated in dev mode is in **[Architecture — Two modes](architecture.md#two-modes)**.

---

## Step 2 — Deploy to AKS

### 2.1 Sign in to Azure

```bash
az login
az account set --subscription <your-subscription-id>
```

You need permission to create resource groups, AKS clusters, ACRs, Foundry resources, and federated credentials in your subscription.. `Contributor` + `User Access Administrator` is sufficient.

### 2.2 Bring it up

```bash
azureclaw up --name prod-agent --location swedencentral
```

What this does, in order:

1. Creates a resource group `azureclaw-<name>-rg`.
2. Creates an ACR (your private registry) and an AKS cluster with Workload Identity and OIDC issuer enabled.
3. Creates an Azure AI Foundry project, Content Safety binding, and a model deployment.
4. Builds and pushes the controller, inference-router, A2A gateway, and sandbox images to the new ACR.
5. Installs the AzureClaw Helm chart (controller + AgentMesh relay/registry + A2A gateway + CRDs).
6. Creates the federated credentials so each sandbox's pod identity can call Foundry without keys.
7. Submits your first `ClawSandbox` and waits until it is `Ready`.

The whole flow is idempotent. If it fails halfway through (a quota error, an IAM hiccup), re-running picks up where it left off.

### 2.3 The pod you get

In AKS mode the sandbox is a multi-container pod, **not** a single container:

- `init: egress-guard` — installs iptables rules so only the router can reach the cluster network.
- `agent` — your runtime (OpenClaw, OpenAI Agents, MAF, LangGraph, Anthropic, Pydantic-AI, or BYO), running as **UID 1000** with no direct egress.
- `inference-router` — the Rust router, running as **UID 1001** on `127.0.0.1:8443`. It is the only container in the pod that can talk to Foundry, the mesh, or A2A peers.

A NetworkPolicy on the namespace pins the pod's allowed egress to exactly: cluster DNS, Foundry, the AgentMesh relay, the A2A gateway. Nothing else. See **[Architecture diagrams](architecture-diagrams.md)** for the full picture.

### 2.4 Talk to the AKS sandbox

```bash
azureclaw connect prod-agent      # tunnels the TUI through kubectl port-forward
azureclaw list                     # all sandboxes in your AKS cluster
azureclaw logs prod-agent -f       # router + agent logs
azureclaw operator                 # full-fleet TUI
```

### 2.5 Add another sandbox

```bash
azureclaw add another-agent --runtime LangGraph --model gpt-4.1
```

`azureclaw add` reuses the existing AKS cluster and Foundry project — only the pod is new. See **[CLI reference](cli-reference.md)** for the full surface.

### 2.6 Tear it down

```bash
azureclaw destroy prod-agent           # one sandbox
azureclaw destroy --all --purge-rg     # everything, including the resource group
```

---

## Bring your own AKS / Foundry / ACR

If you already have an AKS cluster and a Foundry project, you can install AzureClaw into them directly with the Helm chart:

```bash
helm install azureclaw deploy/helm/azureclaw \
  --namespace azureclaw-system --create-namespace \
  --set acr.loginServer=<youracr>.azurecr.io \
  --set foundry.endpoint=https://<your>.openai.azure.com \
  --set foundry.deploymentName=gpt-4.1 \
  --set workloadIdentity.clientId=<federated-mi-client-id>
```

Then submit `ClawSandbox` resources directly with `kubectl apply`. The CLI is convenient but optional — every action it takes is a Helm value, a Kubernetes resource, or an `az` call you can perform yourself. See **[Operations / GitOps](operations/gitops.md)**.

---

## What to read next

- **[Architecture](architecture.md)** — the design in 15 minutes.
- **[CRD reference](api/crd-reference.md)** — every spec field of every CRD.
- **[Runtimes](runtimes.md)** — choosing between the seven adapters and BYO.
- **[Blueprints](blueprints/00-index.md)** — five reference deployment shapes (developer inner loop → sovereign air-gapped).
- **[Security model](security.md)** — what each layer enforces and what it does not.

---

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `azureclaw dev` hangs on first run | Docker Desktop is not running | Start Docker. |
| `azureclaw up` fails on `az login` | Stale CLI session | `az logout && az login --use-device-code`. |
| Sandbox stays `Pending` | Foundry quota / model not deployed | `kubectl describe clawsandbox <name>` — the controller surfaces the cause as a `Condition`. |
| Agent gets `403` on tool call | `ToolPolicy` denies it | `azureclaw policy show <name>` and adjust. |
| Mesh KNOCK fails | Trust score below threshold | See **[AGT boundary](architecture/agt-boundary.md#trust-scoring)**. |

The complete operational runbook is in **[`docs/operations/`](operations/)**.
