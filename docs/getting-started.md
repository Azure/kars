# Getting started

This guide takes you from a clean machine to a working kars agent in two steps:

1. **[Local — five minutes](#step-1--local-five-minutes)** — `kars dev` runs a sandbox in one Docker container on your laptop. No Azure subscription, no AKS, no Kubernetes.
2. **[AKS — half an hour](#step-2--deploy-to-aks)** — `kars up` provisions AKS + ACR + Foundry + the kars control plane in your subscription, and runs the same sandbox under Workload Identity, NetworkPolicies, and the egress guard.

The sandbox YAML you wrote in step 1 runs unchanged in step 2. That is the whole point.

> **Want production-shaped Kubernetes on your laptop?** Between these two there is a local-Kubernetes middle ground: `kars dev --target local-k8s` runs the same controller, CRDs, Helm chart, and NetworkPolicies on a [kind](https://kind.sigs.k8s.io/) cluster — no Azure, no AKS. Use it when you are changing the controller, the chart, or the CRDs and want to validate the Kubernetes glue before you touch AKS. Full walkthrough: **[Blueprint 02 — Local Kubernetes dev loop](blueprints/02-local-k8s-dev-loop.md)**.

---

## Prerequisites

| For | You need |
|---|---|
| **Fastest — published images (`kars dev --release`)** | Docker Desktop (or any OCI runtime) · Node.js 22+. **No Rust, no AGT checkout, no local image build.** Plus an inference provider (GitHub Copilot seat — easiest — or a Foundry/Azure OpenAI deployment, or a GitHub PAT with `models:read`). |
| Local mode from source (GitHub Copilot — recommended) | Docker Desktop (or any OCI runtime), Node.js 22+, Rust 1.88+, an active **GitHub Copilot** seat (Individual / Business / Enterprise). One device-code OAuth login at signup. **No Azure account, no PAT, no key files.** |
| Local mode from source (Foundry / Azure OpenAI) | Docker Desktop, Node.js 22+, Rust 1.88+, an Azure AI Foundry (or Azure OpenAI) endpoint + deployment + key. |
| Local mode from source (GitHub Models) | Docker Desktop, Node.js 22+, Rust 1.88+, a GitHub PAT with `models:read` scope. **No Azure account needed.** |
| AKS mode | The above, plus the [Azure CLI](https://learn.microsoft.com/cli/azure/) (`az`), [`kubectl`](https://kubernetes.io/docs/tasks/tools/), [Helm 3.14+](https://helm.sh/), and an Azure subscription where you can create resource groups. |

> **Just want it running?** Use the [fastest path](#10-fastest-path--no-compile-published-images-) — `npm i -g @kars-runtime/cli` then `kars dev --release`. Everything below about building from source and cloning AGT is **only** for contributors hacking on kars itself.

> **AGT mesh prerequisite — source builds only.** Inter-agent E2E
> messaging uses the [Microsoft Agent Governance Toolkit](https://github.com/microsoft/agent-governance-toolkit)
> relay + registry. With `kars dev --release` these are **pulled as
> published images** — nothing to clone. Only when you build from source
> does kars build them locally; clone AGT next to your kars repo first:
> ```bash
> git clone https://github.com/microsoft/agent-governance-toolkit ~/agent-governance-toolkit
> ```
> Or pass `--agt-repo <path>` / set `$KARS_AGT_REPO` if you keep it
> elsewhere. Once the relay + registry images are cached locally,
> kars will not rebuild them on subsequent runs.

The CLI bootstraps everything else (Helm chart install, Foundry resource creation, ACR build/push, federated identity wiring). You do not need to provision any of it by hand.

### Quickest path: GitHub Copilot (no Azure account, no PAT)

If you have a GitHub Copilot seat — Individual, Business, or Enterprise — `kars dev` is a one-step setup:

1. Run `kars dev`. The CLI prints a **device code** and a URL.
2. Open <https://github.com/login/device> in your browser, paste the code, approve the kars client.
3. Pick a model from the catalogue the CLI shows you — current Claude, GPT, Gemini, and reasoning-class models are exposed; run `kars models` to see today's list. The router will use the selected model for every chat completion the agent makes.

That's it. No PAT to rotate, no API key on disk, no subscription to provision. The OAuth token is stored in `~/.kars/` and refreshed automatically.

**Why we recommend Copilot for the inner loop:**

- **Frontier models, large contexts.** Current Claude, GPT, and Gemini frontier tiers through one auth surface — exactly the catalogue you'd compose by hand against three vendors.
- **Native Anthropic shape for Claude.** kars routes Claude requests to Copilot's `/v1/messages` endpoint with no shape translation, preserving full tool-calling fidelity (no lossy OpenAI-to-Anthropic rewrites).
- **One credential, no key sprawl.** The same OAuth token works for the parent agent and every sub-agent it spawns; the router refreshes it on its own.
- **Sub-agent inheritance.** Spawned sub-agents automatically inherit the parent's provider, model, and credentials — no per-agent wiring.

You can switch to Foundry or GitHub Models any time with `kars credentials`.

### Alternative: GitHub Models (no Azure account, smaller scale)

If you don't have a Copilot seat and don't want to provision Foundry, GitHub Models works with just a PAT:

1. Create a fine-grained PAT at <https://github.com/settings/personal-access-tokens/new> with the **`models:read`** scope.
2. Run `kars dev` and pick **GitHub Models** at the provider prompt.
3. Paste your PAT. The CLI verifies it against `https://models.github.ai/catalog/models` and saves it to `~/.kars/`.

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

# 3. Read the values you'll paste into the `kars dev` prompt
az cognitiveservices account show     -n my-foundry -g my-rg --query properties.endpoint -o tsv
az cognitiveservices account keys list -n my-foundry -g my-rg --query key1            -o tsv
```

Use `--kind AIServices` (not `--kind OpenAI`) — Foundry is what kars integrates with end-to-end (Content Safety, Memory Store, the full Foundry data-plane API surface the router proxies). Standalone `--kind OpenAI` accounts work for `dev` mode's model calls too, but you lose the rest of the surface. Full reference: [Azure AI Foundry quickstart](https://learn.microsoft.com/azure/ai-foundry/).

If you'd rather skip provisioning by hand, jump to **[Step 2 — Deploy to AKS](#step-2--deploy-to-aks)** — `kars up` provisions the Foundry resource, project, Content Safety binding, and a model deployment for you.

---

## Step 1 — Local (five minutes)

### 1.0 Fastest path — no compile, works for everyone (macOS & Linux) ⭐

The quickest way to a running agent **on any host with Docker — amd64 Linux,
Intel Mac, or Apple Silicon (M-series)**: install the CLI from the latest
published release and launch from **pre-built, cosign-signed, public images**.
No Rust toolchain, no AGT checkout, no GitHub auth, no waiting on a local build.

**You need only:** Docker (or a Docker-compatible runtime like Podman) · Node.js 22+.

```bash
# 1. Install the kars CLI from npm — public, signed (SLSA provenance), always the latest release
npm i -g @kars-runtime/cli

# 2. Launch a sandbox from the published images (defaults to :latest)
kars dev --release
```

> Prefer not to use npm? A one-line installer fetches the signed CLI tarball
> directly: `curl -fsSL https://raw.githubusercontent.com/Azure/kars/main/install.sh | bash`.

That's it. `kars dev --release` pulls the `openclaw-sandbox` agent image plus
the AGT mesh relay + registry and runs them — skipping the AGT clone and every
local build. The images are public on `ghcr.io/azure`, so anyone can pull them
with no auth. The first run downloads the images once (a few minutes); afterwards
it launches near-instantly. On first launch you'll pick an inference provider —
see the [provider picker](#12-launch-a-sandbox) below (**GitHub Copilot** is
the easiest: one device-code login, no Azure account).

Run the same published images on Kubernetes instead of plain Docker:

```bash
kars dev --release --target local-k8s   # local kind cluster, real K8s posture
```

> **Apple Silicon (M-series) Macs:** fully supported. Every published image
> (sandbox, controller, router, relay, registry) is multi-arch
> (`linux/amd64` + `linux/arm64`, built on native arm64 runners) and
> `kars dev --release` automatically pulls the variant matching your host
> architecture — no Rosetta, no extra flags. Verified end-to-end on both arm64
> and amd64: the full multi-agent exec-brief scenario (parent + 3 mesh
> sub-agents, E2E-encrypted relay) passes on a stock M-series Mac and on AKS.

> **Pin a specific build (optional).** `kars dev --release` follows the newest
> release; pass a tag — `kars dev --release v0.1.1` — to pin a
> specific build for reproducibility.

Want to hack on the controller / router / plugin? Build from source —
**[1.1 Build the CLI](#11-build-the-cli)**.

### 1.1 Build the CLI

```bash
git clone https://github.com/Azure/kars.git
cd kars/cli
npm ci && npm run build
npm link    # exposes `kars` on your PATH
```

The CLI is a Node 22 ESM build with a small Rust dependency for the local router. `npm run build` compiles both.

### 1.2 Launch a sandbox

```bash
kars dev
```

On the first run you are shown a **3-way provider picker**:

```
$ kars dev

  ╭────────────────────────────────────────────────╮
  │  kars · Local Sandbox                     │
  │  Secure AI Agent Runtime on Azure              │
  ╰────────────────────────────────────────────────╯

  👋 First time? Pick an inference provider — no Azure account needed for the GitHub options.
  Copilot is the default (largest context). You can change later with `kars credentials`.

? Which inference provider do you want to use?
❯ GitHub Copilot                    (recommended; needs an active Copilot seat — large context, Claude/GPT/Gemini)
  Azure AI Foundry / Azure OpenAI   (full feature set: Memory Store, agents, Content Safety, etc.)
  GitHub Models                     (free; just need a GitHub PAT — small context, Foundry features disabled)
```

- **GitHub Copilot** *(default — recommended)*. The CLI prints a device code and a URL (`https://github.com/login/device`); you paste it, approve once, and the OAuth token is stored in `~/.kars/`. The CLI then fetches the live model catalogue from the Copilot API and lets you pick — Claude Opus 4.7, Claude Sonnet 4.5, GPT-5, GPT-4.1, Gemini 2.5 Pro, o-series, etc. The router refreshes the token automatically. **No Azure account, no PAT, no key files.**
- **Azure AI Foundry / Azure OpenAI** — full feature set. Asks for your endpoint, model deployment name, and resource-level API key. The API key is the only credential local mode ever sees, and it is mounted from a local secret file — it never leaves your machine. Required for Memory Store, agents, evaluations, indexes, and inline Content Safety.
- **GitHub Models** — free, no Azure account needed. Asks only for your GitHub PAT (`models:read` scope). Endpoint is hardcoded to `https://models.github.ai/inference`. Default model is `gpt-4o-mini`. Foundry-only routes return `501`. Smaller context windows than Copilot.

Your choice is saved to `~/.kars/config.json` and reused on subsequent runs.

To switch providers later (or rotate keys), run **`kars credentials`** — the same interactive prompt is exposed there too. The same command also handles channel tokens (Telegram, Slack, Discord) and third-party API keys (Brave, Tavily, Exa, Firecrawl, Perplexity, OpenAI). Or scriptable: `kars credentials set <key> <value>` / `list` / `remove`.

After the provider picker, `kars dev` also prompts for an **agent name** (default `dev-agent` — hit Enter to accept) and offers any saved channel tokens for one-tap wiring.

The CLI then builds (or pulls cached) the local sandbox image and starts a single container. In dev mode the agent runtime and the inference router are co-located in that one image — there is no separate router pod, no init container, no NetworkPolicy. You get the same router code path, the same governance profile, the same audit format.

> 💡 **Picking a model with Copilot.** Claude Opus 4.7 is the largest-context option and the best default for tool-heavy agents. Sonnet 4.5 is faster and cheaper for routine tasks. GPT-5 is comparable on reasoning. Switching is `kars credentials` → re-pick — the saved OAuth token is reused, only the model selection changes.

### 1.3 Talk to the agent

```bash
kars connect dev-agent   # opens the TUI
```

Or drive it from another terminal:

```bash
kars list               # see running sandboxes
kars logs dev-agent -f      # tail logs (router + agent)
kars policy show dev-agent  # what is allowed / denied / approval-gated
kars operator           # live fleet TUI — agents, model, mesh peers, egress, audit
```

When you are done:

```bash
kars destroy dev-agent
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

You need permission to create resource groups, AKS clusters, ACRs, Foundry resources, and federated credentials in your subscription. `Contributor` + `User Access Administrator` is sufficient.

For **per-sandbox Entra Agent IDs**, you also need the **Agent ID Developer** Entra directory role. Activate it through PIM or ask your tenant admin to assign it. Without it (and without `--mesh-trust=entra`), `kars up` skips the agent-identity setup and the cluster falls back to the AGT anonymous tier — see [permissions.md](permissions.md#tenant-level-entra-id-considerations) for the full breakdown.

### 2.2 Bring it up

```bash
# Anonymous tier (default) — zero Entra prerequisites, shared cluster MI.
# --release pulls the public, signed images (no local build / Rust toolchain).
kars up --name prod-agent --region swedencentral --release

# Entra tier — full per-sandbox Entra Agent IDs + verified mesh trust
kars up --name prod-agent --region swedencentral --release --mesh-trust=entra

# Microsoft-corp users: also pass your ServiceTree GUID
kars up --name prod-agent --region swedencentral --release --mesh-trust=entra --service-tree <guid>
```

> **`--release` vs `--build`:** `--release` imports the public, cosign-signed
> `ghcr.io/azure/*` images into your ACR — no Rust, no Docker build, no source
> checkout to compile (bare `--release` = latest, or pin `--release v0.1.4`).
> Drop it to import from a source ACR, or pass `--build` to compile from source
> (developer mode; compiles Rust in-Docker on macOS/arm64).

The `--mesh-trust=entra` flag turns on Phase 5b (per-sandbox typed
agent identity SPs + Foundry RBAC + federated credentials) plus
Phase 6.b/6.c (AGT mesh relay/registry verify peer JWTs against
Entra's JWKS). One flag, full chain. See
[docs/architecture/entra-agent-id/](architecture/entra-agent-id/)
for the architecture.

What this does, in order:

1. Runs preflight: subscription RBAC, resource providers, **Entra Agent ID directory role** (skipped when `--mesh-trust=anonymous`), preview features.
2. Creates a resource group `kars-<name>-rg`.
3. Creates an ACR (your private registry) and an AKS cluster with Workload Identity and OIDC issuer enabled.
4. Creates an Azure AI Foundry project, Content Safety binding, and a model deployment.
5. **Gets the images into your ACR** — with `--release`, imports the public, cosign-signed `ghcr.io/azure/*` images (no build); with `--build`, compiles the controller, inference-router, A2A gateway, and sandbox images from source and pushes them; otherwise imports from `--source-acr`.
6. Installs the kars Helm chart (controller + AgentMesh relay/registry + A2A gateway + CRDs).
7. **(--mesh-trust=entra only)** Provisions the Entra Agent ID trust anchor (idempotent): blueprint application + service principal in your tenant, controller managed identity in your subscription, and a federated identity credential trusting the controller MI. Writes a `KarsAuthConfig/default` CR to the cluster. Wires `AGENTMESH_ENTRA_AUDIENCE` + `AGENTMESH_ENTRA_TENANT_ID` env on the AGT relay+registry deployments for verified-tier mesh registration.
8. Submits your first `KarsSandbox` and waits until it is `Ready`. With `--mesh-trust=entra`, the controller mints a per-sandbox **Entra Agent ID** (`kars-<cluster>-<sandbox>`) and Foundry sees that agent identity as the calling principal. With `--mesh-trust=anonymous`, sandboxes share the cluster's workload identity.

The whole flow is idempotent. If it fails halfway through (a quota error, an IAM hiccup), re-running picks up where it left off. To deploy fresh and ignore any cached partial state from a previous run, pass `--from-scratch`. The tenant-wide blueprint is **reused** across `kars up` invocations — only the per-cluster controller MI is recreated when you target a new cluster name.

### 2.3 The pod you get

In AKS mode the sandbox is a multi-container pod, **not** a single container:

- `init: egress-guard` — installs iptables rules so only the router can reach the cluster network.
- `agent` — your runtime (OpenClaw, OpenAI Agents, MAF, LangGraph, Anthropic, Pydantic-AI, or BYO), running as **UID 1000** with no direct egress.
- `inference-router` — the Rust router, running as **UID 1001** on `127.0.0.1:8443`. It is the only container in the pod with network egress — it brokers identity/auth for Foundry calls and **WebSocket-bridges opaque mesh ciphertext** between the agent and the AgentMesh relay (the Signal session itself is owned plugin-side inside the agent container — see [Architecture → The mesh](architecture.md#the-mesh)).

A NetworkPolicy on the namespace pins the pod's allowed egress to exactly: cluster DNS, Foundry, the AgentMesh relay, the A2A gateway. Nothing else. See **[Architecture diagrams](architecture-diagrams.md)** for the full picture.

### 2.4 Talk to the AKS sandbox

```bash
kars connect prod-agent      # tunnels the TUI through kubectl port-forward
kars list                     # all sandboxes in your AKS cluster
kars logs prod-agent -f       # router + agent logs
kars operator                 # full-fleet TUI
```

### 2.5 Add another sandbox

```bash
kars add another-agent --runtime LangGraph --model gpt-4.1
```

`kars add` reuses the existing AKS cluster and Foundry project — only the pod is new. See **[CLI reference](cli-reference.md)** for the full surface.

### 2.5a Try a Hermes-runtime sandbox instead

The same `kars add` works for [Hermes](https://github.com/NousResearch/hermes-agent), a channels-first agent harness with native MCP support — useful when you want a Telegram or Slack-driven agent without writing the integration:

```bash
# Mesh-only Hermes agent (no channels — talks to other agents via the kars mesh).
kars add hermes-helper --runtime Hermes --model gpt-4.1

# Hermes agent fronted by a Telegram bot.
kars add hermes-helper --runtime Hermes --model gpt-4.1 \
  --channels telegram --telegram-token "$TELEGRAM_BOT_TOKEN"
```

The Hermes adapter ships its own plugin (mesh tools, governance hook, Foundry tool wrappers, sub-agent spawn) and joins the AGT mesh identically to OpenClaw — so `kars_mesh_send` works in either direction between OpenClaw and Hermes peers. Full reference: **[Hermes plugin](hermes-plugin.md)**.

### 2.6 Tear it down

```bash
kars destroy prod-agent           # one sandbox
kars destroy --all                # everything, including the resource group
```

---

## Bring your own AKS / Foundry / ACR

If you already have an AKS cluster and a Foundry project, you can install kars into them directly with the Helm chart:

```bash
helm install kars deploy/helm/kars \
  --namespace kars-system --create-namespace \
  --set acr.loginServer=<youracr>.azurecr.io \
  --set foundry.endpoint=https://<your>.openai.azure.com \
  --set foundry.deploymentName=gpt-4.1 \
  --set workloadIdentity.clientId=<federated-mi-client-id>
```

Then submit `KarsSandbox` resources directly with `kubectl apply` — see the [minimal example](api/crd-reference.md#minimal-example) for the smallest valid sandbox + `InferencePolicy` pair. The CLI is convenient but optional — every action it takes is a Helm value, a Kubernetes resource, or an `az` call you can perform yourself. See **[Operations / GitOps](operations/gitops.md)**.

---

## What to read next

- **[Architecture](architecture.md)** — the design in 15 minutes.
- **[CRD reference](api/crd-reference.md)** — every spec field of every CRD.
- **[Runtimes](runtimes.md)** — choosing between the seven adapters and BYO.
- **[Blueprints](blueprints/00-index.md)** — six reference deployment shapes (developer inner loop → sovereign air-gapped).
- **[Security model](security.md)** — what each layer enforces and what it does not.

---

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `kars dev` hangs on first run | Docker Desktop is not running | Start Docker. |
| `kars up` fails on `az login` | Stale CLI session | `az logout && az login --use-device-code`. |
| `kars connect` fails with `address already in use` | Leftover `kubectl port-forward` from a previous session is still holding the local port | `lsof -ti:18789 \| xargs kill` (or restart your terminal). Then retry. |
| `kars dev` errors with `Unsupported engine` on `npm ci` | Node.js < 22 | Install Node 22+ (we test against the LTS line; see [`cli/package.json`](../cli/package.json) for the exact engines pin). |
| `kars dev` aborts with `dyld: Library not loaded: …libllhttp.X.Y.dylib` | Homebrew Node was linked against a `llhttp` dylib that `brew cleanup` later removed (common after `brew install rust`/`brew upgrade`) | `brew reinstall node`. Node itself crashes before any kars code runs — preflight cannot catch this. |
| `kars <cmd>` exits with `✗ No kubectl current-context set` | You have multiple kubeconfig clusters (e.g. prod + staging + dev) and never picked one. kars deliberately refuses to guess — auto-discovery against the wrong cluster is too risky for write commands. | Pick one explicitly: `export KARS_KUBE_CONTEXT=<name>` (per-shell, kars-only, never touches your real kubeconfig) OR `kubectl config use-context <name>` (persistent, affects every kubectl invocation). The error message lists every available context. |
| `kind create cluster` fails with `cluster "kind" already exists` | A previous `kars dev --target local-k8s` run did not clean up | `kind delete cluster --name <name>` and retry. |
| GitHub Copilot provider returns `401` | The token is a classic PAT, not a Copilot-enabled OAuth token; or your Copilot seat is inactive | Verify your seat at [github.com/settings/copilot](https://github.com/settings/copilot). See [`cli-reference.md#kars-dev`](cli-reference.md#kars-dev) for the OAuth flow. |
| Sandbox stays `Pending` | Foundry quota / model not deployed | `kubectl describe karssandbox <name>` — the controller surfaces the cause as a `Condition`. |
| Agent gets `403` on tool call | `ToolPolicy` denies it | `kars policy show <name>` and adjust. See [`cli-reference.md#kars-policy`](cli-reference.md#kars-policy). |
| Mesh KNOCK fails | Trust score below threshold | See **[AGT boundary](architecture/agt-boundary.md#trust-scoring)**. |

The complete operational runbook is in **[`docs/operations/`](operations/)**.
