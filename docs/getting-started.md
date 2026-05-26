# Getting started

This guide takes you from a clean machine to a working AzureClaw agent in two steps:

1. **[Local — five minutes](#step-1--local-five-minutes)** — `azureclaw dev` runs a sandbox in one Docker container on your laptop. No Azure subscription, no AKS, no Kubernetes.
2. **[AKS — half an hour](#step-2--deploy-to-aks)** — `azureclaw up` provisions AKS + ACR + Foundry + the AzureClaw control plane in your subscription, and runs the same sandbox under Workload Identity, NetworkPolicies, and the egress guard.

The sandbox YAML you wrote in step 1 runs unchanged in step 2. That is the whole point.

---

## Prerequisites

| For | You need |
|---|---|
| **Local mode (GitHub Copilot — recommended)** | Docker Desktop (or any OCI runtime), Node.js 22+, Rust 1.88+, an active **GitHub Copilot** seat (Individual / Business / Enterprise). One device-code OAuth login at signup. **No Azure account, no PAT, no key files.** |
| Local mode (Foundry / Azure OpenAI) | Docker Desktop, Node.js 22+, Rust 1.88+, an Azure AI Foundry (or Azure OpenAI) endpoint + deployment + key. |
| Local mode (GitHub Models) | Docker Desktop, Node.js 22+, Rust 1.88+, a GitHub PAT with `models:read` scope. **No Azure account needed.** |
| AKS mode | The above, plus the [Azure CLI](https://learn.microsoft.com/cli/azure/) (`az`), [`kubectl`](https://kubernetes.io/docs/tasks/tools/), [Helm 3.14+](https://helm.sh/), and an Azure subscription where you can create resource groups. |

The CLI bootstraps everything else (Helm chart install, Foundry resource creation, ACR build/push, federated identity wiring). You do not need to provision any of it by hand.

### Quickest path: GitHub Copilot (no Azure account, no PAT)

If you have a GitHub Copilot seat — Individual, Business, or Enterprise — `azureclaw dev` is a one-step setup:

1. Run `azureclaw dev`. The CLI prints a **device code** and a URL.
2. Open <https://github.com/login/device> in your browser, paste the code, approve the AzureClaw client.
3. Pick a model from the catalogue the CLI shows you — current Claude, GPT, Gemini, and reasoning-class models are exposed; run `azureclaw models` to see today's list. The router will use the selected model for every chat completion the agent makes.

That's it. No PAT to rotate, no API key on disk, no subscription to provision. The OAuth token is stored in `~/.azureclaw/` and refreshed automatically.

**Why we recommend Copilot for the inner loop:**

- **Frontier models, large contexts.** Current Claude, GPT, and Gemini frontier tiers through one auth surface — exactly the catalogue you'd compose by hand against three vendors.
- **Native Anthropic shape for Claude.** AzureClaw routes Claude requests to Copilot's `/v1/messages` endpoint with no shape translation, preserving full tool-calling fidelity (no lossy OpenAI-to-Anthropic rewrites).
- **One credential, no key sprawl.** The same OAuth token works for the parent agent and every sub-agent it spawns; the router refreshes it on its own.
- **Sub-agent inheritance.** Spawned sub-agents automatically inherit the parent's provider, model, and credentials — no per-agent wiring.

You can switch to Foundry or GitHub Models any time with `azureclaw credentials`.

### Alternative: GitHub Models (no Azure account, smaller scale)

If you don't have a Copilot seat and don't want to provision Foundry, GitHub Models works with just a PAT:

1. Create a fine-grained PAT at <https://github.com/settings/personal-access-tokens/new> with the **`models:read`** scope.
2. Run `azureclaw dev` and pick **GitHub Models** at the provider prompt.
3. Paste your PAT. The CLI verifies it against `https://models.github.ai/catalog/models` and saves it to `~/.azureclaw/`.

Subsequent runs reuse the saved provider — no flag required. To override for one run only (without overwriting your saved provider), pass `--github-token <pat>`.

> ⚠️ **Trade-offs in GitHub Models mode.** Foundry-only routes return `501 Not Implemented` (Memory Store, agents, evaluations, indexes, knowledge bases, datasets, deployments, connections). Inline Content Safety prompt-shield filtering is **not enforced** server-side — the router can only act on `prompt_filter_results` returned by the model, and GitHub Models doesn't return them. Smaller context windows and tighter rate limits than Copilot or Foundry — fine for trivial demos, frustrating for real agent loops. See [GitHub Models docs](https://docs.github.com/github-models) for current quotas.

### Don't have an Azure AI Foundry deployment yet?

Local mode needs an existing Azure AI Foundry resource and a model deployment. Foundry is the unified successor to standalone Azure OpenAI accounts — same model catalogue, same OpenAI-compatible API, plus Content Safety, Memory Store, agents, and the rest of the AI Services surface in one resource. Two `az` commands get you both. Pick a region that has the model you want (`gpt-4.1` is widely available in `swedencentral`, `eastus2`, `westus3`):

```bash
# 1. Create the Foundry (AI Services) resource (≈ 30 s)
az cognitiveservices account create \
  --name my-foundry \
  --resource-group my-rg \
  --kind AIServices --sku S0 \
  --location swedencentral \
  --custom-domain my-foundry

# 2. Create a model deployment on it (≈ 10 s)
az cognitiveservices account deployment create \
  --name my-foundry \
  --resource-group my-rg \
  --deployment-name gpt-4.1 \
  --model-name gpt-4.1 --model-version "2025-04-14" \
  --model-format OpenAI \
  --sku-capacity 50 --sku-name GlobalStandard

# 3. Read the values you'll paste into the `azureclaw dev` prompt
az cognitiveservices account show     -n my-foundry -g my-rg --query properties.endpoint -o tsv
az cognitiveservices account keys list -n my-foundry -g my-rg --query key1            -o tsv
```

Use `--kind AIServices` (not `--kind OpenAI`) — Foundry is what AzureClaw integrates with end-to-end (Content Safety, Memory Store, the 18 Foundry API groups the router proxies). Standalone `--kind OpenAI` accounts work for `dev` mode's model calls too, but you lose the rest of the surface. Full reference: [Azure AI Foundry quickstart](https://learn.microsoft.com/azure/ai-foundry/).

If you'd rather skip provisioning by hand, jump to **[Step 2 — Deploy to AKS](#step-2--deploy-to-aks)** — `azureclaw up` provisions the Foundry resource, project, Content Safety binding, and a model deployment for you.

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
azureclaw dev
```

On the first run you are shown a **3-way provider picker**:

```
$ azureclaw dev

  ╭────────────────────────────────────────────────╮
  │  AzureClaw · Local Sandbox                     │
  │  Secure AI Agent Runtime on Azure              │
  ╰────────────────────────────────────────────────╯

  👋 First time? Pick an inference provider — no Azure account needed for the GitHub options.
  Copilot is the default (largest context). You can change later with `azureclaw credentials`.

? Which inference provider do you want to use?
❯ GitHub Copilot                    (recommended; needs an active Copilot seat — large context, Claude/GPT/Gemini)
  Azure AI Foundry / Azure OpenAI   (full feature set: Memory Store, agents, Content Safety, etc.)
  GitHub Models                     (free; just need a GitHub PAT — small context, Foundry features disabled)
```

- **GitHub Copilot** *(default — recommended)*. The CLI prints a device code and a URL (`https://github.com/login/device`); you paste it, approve once, and the OAuth token is stored in `~/.azureclaw/`. The CLI then fetches the live model catalogue from the Copilot API and lets you pick — Claude Opus 4.7, Claude Sonnet 4.5, GPT-5, GPT-4.1, Gemini 2.5 Pro, o-series, etc. The router refreshes the token automatically. **No Azure account, no PAT, no key files.**
- **Azure AI Foundry / Azure OpenAI** — full feature set. Asks for your endpoint, model deployment name, and resource-level API key. The API key is the only credential local mode ever sees, and it is mounted from a local secret file — it never leaves your machine. Required for Memory Store, agents, evaluations, indexes, and inline Content Safety.
- **GitHub Models** — free, no Azure account needed. Asks only for your GitHub PAT (`models:read` scope). Endpoint is hardcoded to `https://models.github.ai/inference`. Default model is `gpt-4o-mini`. Foundry-only routes return `501`. Smaller context windows than Copilot.

Your choice is saved to `~/.azureclaw/config.json` and reused on subsequent runs.

To switch providers later (or rotate keys), run **`azureclaw credentials`** — the same interactive prompt is exposed there too. The same command also handles channel tokens (Telegram, Slack, Discord) and third-party API keys (Brave, Tavily, Exa, Firecrawl, Perplexity, OpenAI). Or scriptable: `azureclaw credentials set <key> <value>` / `list` / `remove`.

After the provider picker, `azureclaw dev` also prompts for an **agent name** (default `dev-agent` — hit Enter to accept) and offers any saved channel tokens for one-tap wiring.

The CLI then builds (or pulls cached) the local sandbox image and starts a single container. In dev mode the agent runtime and the inference router are co-located in that one image — there is no separate router pod, no init container, no NetworkPolicy. You get the same router code path, the same governance profile, the same audit format.

> 💡 **Picking a model with Copilot.** Claude Opus 4.7 is the largest-context option and the best default for tool-heavy agents. Sonnet 4.5 is faster and cheaper for routine tasks. GPT-5 is comparable on reasoning. Switching is `azureclaw credentials` → re-pick — the saved OAuth token is reused, only the model selection changes.

### 1.3 Talk to the agent

```bash
azureclaw connect dev-agent   # opens the TUI
```

Or drive it from another terminal:

```bash
azureclaw list               # see running sandboxes
azureclaw logs dev-agent -f      # tail logs (router + agent)
azureclaw policy show dev-agent  # what is allowed / denied / approval-gated
azureclaw operator           # live fleet TUI — agents, model, mesh peers, egress, audit
```

When you are done:

```bash
azureclaw destroy dev-agent
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
azureclaw destroy --all                # everything, including the resource group
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
