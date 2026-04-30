# Getting Started with AzureClaw

By the end of this guide you will have a working AzureClaw sandbox — either
running locally on Docker in about 5 minutes (Path A), or deployed on AKS in
about 15–20 minutes (Path B). You will be able to chat with a governed AI
agent, spawn a sub-agent, send an E2E encrypted mesh message, and hand off a
session from laptop to cloud. **Estimated total time: 30 minutes or less.**

Prerequisites at a glance: Node.js 22+, Docker, a terminal. For the AKS path,
add Azure CLI, kubectl, Helm, and an Azure subscription where you hold
Contributor + User Access Administrator (or Owner).

---

## Prerequisites

### All paths

| Tool | Version | Install |
|------|---------|---------|
| **Node.js** | 22+ | [nodejs.org](https://nodejs.org) or `nvm install 22` |
| **Docker Desktop** | Latest | [docker.com/get-docker](https://www.docker.com/get-docker) |
| **git** | Any | [git-scm.com](https://git-scm.com) |

### Path B (AKS) only

| Tool | Version | Install |
|------|---------|---------|
| **Azure CLI** | 2.60+ | `curl -sL https://aka.ms/InstallAzureCLIDeb \| sudo bash` |
| **kubectl** | 1.28+ | `az aks install-cli` |
| **Helm** | 3.14+ | [helm.sh/docs/intro/install](https://helm.sh/docs/intro/install) |

### Azure RBAC (Path B)

`azureclaw up` provisions AKS, ACR, Key Vault, Foundry, Workload Identity,
and role assignments in one shot. It needs:

```bash
# Contributor + User Access Administrator at subscription scope
SUB=$(az account show --query id -o tsv)
USER=$(az account show --query user.name -o tsv)
az role assignment create --assignee "$USER" --role "Contributor" \
  --scope "/subscriptions/$SUB"
az role assignment create --assignee "$USER" \
  --role "User Access Administrator" \
  --scope "/subscriptions/$SUB"
```

`Owner` at subscription scope covers both. See
[`docs/permissions.md`](permissions.md) for the full list of required
actions, a least-privilege custom role JSON, required resource providers, and
the preview feature (`EncryptionAtHost`, `KataVMIsolationPreview`) registration
steps. The CLI runs a preflight check automatically and fails fast in ≤30 s if
anything is missing — use `azureclaw up --dry-run` to check without deploying.

---

## Step 0 — Install the CLI

Both paths need the CLI:

```bash
git clone https://github.com/Azure/azureclaw.git
cd azureclaw/cli

npm ci
npm run build
npm link          # puts `azureclaw` on your $PATH

# Verify
azureclaw --version
azureclaw --help
```

> **Tip:** `npm link` writes a symlink into your global Node.js bin directory
> (e.g. `~/.nvm/versions/node/v22.x.x/bin/azureclaw`). If `azureclaw` is not
> found after this step, ensure that directory is on your `$PATH`.

---

## Path A — Local development with Docker (≈5 minutes)

No Azure account needed. You get the same AGT governance, the same egress
guard, and the same inference router — just pointed at an Azure OpenAI
resource you already have (not AKS).

### Step A-1: Build the sandbox image

```bash
# Run from the repo root (not cli/)
cd ..
azureclaw dev --build
```

The first `--build` takes 2–3 minutes to pull the Azure Linux base image and
compile the OpenClaw runtime layer. Subsequent runs skip the rebuild unless
you pass `--build` again or `--build-base` when upgrading heavy deps.

### Step A-2: Provide Azure OpenAI credentials

On first run the CLI prompts interactively:

```
  No Azure OpenAI credentials found. Let's set them up.
  You need an existing Azure OpenAI resource with a deployed model (e.g. gpt-4.1).

  ? Azure OpenAI endpoint: https://my-resource.openai.azure.com
  ? Model deployment name: gpt-4.1
  ? API key: ********
```

To provide them non-interactively instead:

```bash
azureclaw credentials set endpoint https://my-resource.openai.azure.com
azureclaw credentials set model gpt-4.1
azureclaw credentials set api-key "YOUR_KEY"
```

Credentials are stored encrypted in `~/.azureclaw/credentials.json` and
re-used on every subsequent `azureclaw dev` run.

### Step A-3: Start a sandbox

If you already ran `--build` in A-1, the image is cached — this starts
immediately:

```bash
azureclaw dev
```

You are now connected to the OpenClaw TUI. The agent is fully governed: every
inference call flows through the Rust inference router, which applies the
`developer` policy preset (Content Safety, token budgets, domain blocklist)
before forwarding to Azure OpenAI.

**End state:**

```
  ✔  Credentials loaded
  ✔  Sandbox image resolved
  ✔  AgentMesh relay/registry started
  ✔  Sandbox container running

  🦞 AzureClaw local sandbox › dev-agent
  You:
```

### Step A-4: Optional enhancements

```bash
# Connect via Telegram instead of TUI
azureclaw dev --channels telegram --telegram-token "123456:ABC-DEF..."

# Enable Brave + Tavily search plugins
azureclaw dev --brave-api-key "$BRAVE_KEY" --tavily-api-key "$TAVILY_KEY"

# Use a different model
azureclaw dev --model gpt-5-mini

# Name your sandbox
azureclaw dev --name research-local
```

---

## Path B — Production AKS deployment (≈15–20 minutes)

`azureclaw up` is a single command that provisions the entire Azure stack and
drops you into a running cluster.

### Step B-1: Log in to Azure

```bash
az login
az account set --subscription "YOUR_SUBSCRIPTION_ID"
```

### Step B-2: Run `azureclaw up`

```bash
# From the repo root (azureclaw/)
azureclaw up
```

The CLI runs interactive prompts first:

```
  ? Azure region   [eastus2]
  ? Cluster name   [azureclaw]
  ? Sandbox name   [my-assistant]
  ? Isolation      [enhanced] (standard / enhanced / confidential)
  ? Foundry backend  [provision new] (or enter an existing endpoint)
```

Then the 9-phase deployment:

```
  [1/9] Setting up resource group 'azureclaw-rg'...         ✔
  [2/9] Running Bicep deployment...                          ✔  (~8 min)
  [3/9] Configuring network / ACR attach...                  ✔
  [4/9] Fetching AKS credentials...                          ✔
  [5/9] Importing images from azureclawacr.azurecr.io...     ✔
  [6/9] Helm install (controller + seccomp DaemonSet)...     ✔
  [7/9] Deploying AgentMesh relay + registry...              ✔
  [8/9] Creating ClawSandbox CR (my-assistant)...            ✔
  [9/9] Waiting for sandbox Running...                       ✔
```

Total elapsed: ~15–20 minutes on a fresh subscription.

### Step B-3: Open the operator TUI

```bash
azureclaw operator
```

The live TUI shows all sandboxes with status, model, policy preset, egress
domains learned, and a scrolling inference log per sandbox.

### Step B-4: Connect to the sandbox

```bash
# TUI session (interactive chat)
azureclaw connect my-assistant

# Web UI on localhost:18789 (browser)
azureclaw connect my-assistant --web
```

### Step B-5: Add a second agent with Telegram

```bash
# Add a sandbox with Telegram channel enabled
azureclaw add research-bot \
  --model gpt-4.1 \
  --channels telegram \
  --telegram-token "123456:ABC-DEF..." \
  --learn-egress

# The agent is now reachable via your Telegram bot
```

See [`docs/channels-plugins.md`](channels-plugins.md) for Slack, Discord,
WhatsApp, and third-party plugin (Brave, Tavily, Exa) setup.

### Preflight and dry-run

```bash
# Check permissions without deploying
azureclaw up --dry-run

# Re-deploy after an upgrade (skips prompts and infra, reruns Helm only)
azureclaw up --upgrade

# Skip preflight for CI / federated identity environments
azureclaw up --skip-preflight
```

### Troubleshooting Path B

See [Troubleshooting common errors](#troubleshooting-common-errors) below.

---

## Trying out a non-default runtime

AzureClaw ships first-class adapters for three runtimes. The same
governance, isolation, and audit chain apply regardless of which you pick.

### OpenAI Agents (Python)

```bash
# On an existing AKS cluster (after azureclaw up)
azureclaw add oai-bot \
  --runtime openai-agents \
  --model gpt-4.1 \
  --isolation enhanced

# Dry-run to inspect the ClawSandbox YAML
azureclaw add oai-bot --runtime openai-agents --dry-run
```

The controller patches the sandbox pod to run the `openai-agents` Python
adapter instead of OpenClaw. The inference router, egress guard, AGT
governance, and AgentMesh mesh endpoint are identical to the default runtime.

### Microsoft Agent Framework (MAF)

```bash
azureclaw add maf-bot \
  --runtime microsoft-agent-framework \
  --maf-language python \
  --model gpt-4.1 \
  --isolation enhanced
```

`dotnet` language support is planned for Phase 3. Until then, use `python`.

### BYO (Bring Your Own)

Any container image that declares the AzureClaw runtime contract label can
be hosted as a first-class sandbox:

```bash
azureclaw add custom-agent \
  --runtime byo \
  --byo-image myacr.azurecr.io/my-agent:latest \
  --byo-contract-version v1
```

The contract requires `LABEL org.azureclaw.runtime.contract=v1` in the
Dockerfile and a small HTTP health endpoint. Full specification:
[`docs/runtime-contract.md`](runtime-contract.md).

### What changes vs the OpenClaw default

| Aspect | openclaw (default) | openai-agents | microsoft-agent-framework | byo |
|--------|--------------------|---------------|---------------------------|-----|
| Agent entrypoint | `entrypoint.sh` (Node.js) | OpenAI Agents Python SDK | MAF Python SDK | Custom image |
| Plugin auto-config | ✅ via entrypoint | ❌ manage in image | ❌ manage in image | ❌ manage in image |
| Inference router | ✅ same | ✅ same | ✅ same | ✅ same |
| AGT governance | ✅ same | ✅ same | ✅ same | ✅ same |
| AgentMesh E2E | ✅ same | ✅ same | ✅ same | ✅ same |
| Egress guard | ✅ same | ✅ same | ✅ same | ✅ same |

---

## Hello-world workflows

These workflows work on both local Docker (`azureclaw dev`) and AKS
(`azureclaw add`/`azureclaw up`). Where commands differ, the AKS variant is
shown; swap `my-assistant` for the name you chose.

### 1 — Spawn a sub-agent

Sub-agents are isolated sandboxes spawned programmatically by the parent
agent. They inherit the parent's isolation level and cannot downgrade it.

```bash
# From TUI or a tool call, the agent issues a ClawSandbox spawn. You can
# also trigger it directly from the CLI (AKS):
azureclaw add worker-1 \
  --model gpt-5-mini \
  --token-budget-per-request 8000 \
  --isolation enhanced

# Watch the controller reconcile it
azureclaw operator --panels status
```

The controller creates namespace `azureclaw-worker-1`, deploys the sandbox
pod, and the parent agent can reach `worker-1` via the E2E encrypted mesh.

### 2 — Send an E2E encrypted mesh message between two agents

```bash
# Check mesh status (shows registered agents and trust scores)
azureclaw mesh status

# List agents on the mesh
azureclaw mesh list

# Set the relay to strict mode (only registered agents can send)
azureclaw mesh security strict
```

Messages flow through the AgentMesh relay over X3DH + Double Ratchet. The
relay sees only ciphertext; decryption happens inside each sandbox pod. The
trust-gated KNOCK handshake requires trust score ≥ 500 (configurable via
`--trust-threshold`).

To observe a mesh exchange live:

```bash
# In terminal 1 — connect to agent alpha
azureclaw connect my-assistant

# In terminal 2 — connect to research-bot (if running)
azureclaw connect research-bot
```

Ask `my-assistant` to "send a task to research-bot" and watch the E2E
encrypted round-trip in both TUI windows.

### 3 — Approve a previously-blocked egress endpoint

By default, `networkPolicy.approvalRequired: true` blocks all new endpoints
not in the allowlist. With `--learn-egress`, the router observes but does not
block; review and promote to the allowlist:

```bash
# See what the agent tried to reach (learn mode must have been enabled)
azureclaw egress my-assistant --learned

# Approve a specific domain permanently
azureclaw policy allow my-assistant api.github.com

# Or with a specific port
azureclaw policy allow my-assistant internal.corp.com --port 8443

# Enforce the approved list (blocks everything else)
azureclaw egress my-assistant --enforce
```

### 4 — Hot-swap models

Switch the active model without restarting the sandbox:

```bash
# List available model deployments in the Foundry project
azureclaw model list my-assistant

# Switch to a smaller model for cost control
azureclaw model set my-assistant gpt-5-mini

# Switch back to the full model
azureclaw model set my-assistant gpt-4.1

# Verify
azureclaw model get my-assistant
```

The inference router picks up the new model on the next request — no pod
restart, no session interruption.

### 5 — Live-handoff agent from local Docker to AKS

Transfer a running local session to the cloud cluster with no dropped
requests. Both sides must share an AgentMesh registry:

```bash
# 1. On the AKS side — expose the registry so the laptop can reach it
azureclaw mesh promote --port-forward

# 2. On the laptop — start local dev pointing at the shared registry
azureclaw dev --global-registry "http://localhost:8080" --name my-assistant

# 3. Initiate handoff to cloud (from the laptop)
azureclaw handoff my-assistant --to cloud

# 4. Check status
azureclaw handoff my-assistant --status
```

The AgentMesh relay transfers session state; the local container is torn
down only after the cloud sandbox confirms it has taken over. To reverse:

```bash
azureclaw handoff my-assistant --to local
```

---

## Where to go next

| Document | What you'll find |
|----------|-----------------|
| [`docs/use-cases.md`](use-cases.md) | Four end-to-end scenarios — AzureClaw-native agent, any-OpenClaw cloud offload, AzureClaw ↔ AzureClaw mesh, and the roadmap for non-OpenClaw runtimes |
| [`docs/blueprints/00-index.md`](blueprints/00-index.md) | Five deployment shapes (developer inner-loop, enterprise self-hosted, managed public offload, cross-org federation, sovereign/air-gapped) with topology + trust-boundary + flow diagrams |
| [`docs/architecture.md`](architecture.md) | Deep-dive into all four components, the four-seam provider architecture, and how the defense-in-depth layers compose |
| [`docs/api/crd-reference.md`](api/crd-reference.md) | Schema reference for all 8 CRDs: `ClawSandbox`, `ClawPairing`, `McpServer`, `ToolPolicy`, `InferencePolicy`, `A2AAgent`, `ClawMemory`, `ClawEval` |
| [`docs/cli-reference.md`](cli-reference.md) | Every flag of every command across all 23 CLI commands |
| [`docs/security.md`](security.md) | Defense-in-depth breakdown: egress guard, NetworkPolicy, seccomp, Workload Identity, Content Safety, AGT governance, Kata confidential isolation |
| [`docs/channels-plugins.md`](channels-plugins.md) | Telegram, Slack, Discord, WhatsApp setup; Brave, Tavily, Exa, Firecrawl, Perplexity plugin config |
| [`docs/permissions.md`](permissions.md) | Detailed Azure RBAC requirements, custom role JSON, resource provider checklist |
| [`docs/runtime-contract.md`](runtime-contract.md) | BYO runtime contract — labels, health endpoint, inference API shape |
| [`CONTRIBUTING.md`](../CONTRIBUTING.md) | How to contribute: build environment, test commands, PR process |

---

## Troubleshooting common errors

### "address already in use" on port-forward

```
Error: listen tcp 127.0.0.1:18789: bind: address already in use
```

A previous `azureclaw dev` or port-forward is still running:

```bash
# Find the process holding the port
lsof -ti tcp:18789

# Kill it (replace PID with the value from lsof output)
kill <PID>

# Then retry
azureclaw dev
```

### `AADSTS500011` — scope not found in tenant

```
AADSTS500011: The resource principal named api://agentmesh was not found
```

The `api://agentmesh` Entra application is not registered in your tenant.
Sandboxes still start and fall back to the AGT **anonymous tier** (dev/test
only). For production, a tenant admin must run:

```bash
az ad app create --display-name "AgentMesh" --identifier-uris "api://agentmesh"
APP_ID=$(az ad app list --display-name AgentMesh --query "[0].appId" -o tsv)
az ad sp create --id "$APP_ID"
```

See [`docs/permissions.md`](permissions.md#tenant-level-entra-id-considerations) for full details.

### `ImagePullBackOff` in sandbox pod

```bash
kubectl get events -n azureclaw-my-assistant --field-selector reason=Failed
```

Common causes:

1. **ACR credentials expired** — re-attach ACR to the cluster:
   ```bash
   az aks update --name azureclaw --resource-group azureclaw-rg \
     --attach-acr $(az acr list -g azureclaw-rg --query "[0].name" -o tsv)
   ```

2. **Wrong image tag** — AzureClaw always uses `:latest`. Never hardcode
   version tags or set `SANDBOX_IMAGE` / `INFERENCE_ROUTER_IMAGE` env vars
   manually; let the controller resolve them.

3. **Node image cache** — AKS nodes cache `:latest`. Force a fresh pull:
   ```bash
   kubectl rollout restart deployment/my-assistant -n azureclaw-my-assistant
   ```

### "wrong secret key for the given ciphertext"

```
wrong secret key for the given ciphertext
```

The AgentMesh SDK ratchet key is mismatched — a session was started before the
vendored SDK patch was applied. Patch files are in `vendor/agentmesh-sdk/`;
they are overlaid automatically in the sandbox Dockerfile. If you are running
custom images, ensure the overlay step is present:

```dockerfile
COPY vendor/agentmesh-sdk/dist/ \
  /app/node_modules/@agentmesh/sdk/dist/
```

To reset all mesh sessions (drops all ratchet state):

```bash
azureclaw mesh reset
```

### `AuthorizationFailed` during `azureclaw up`

```
The client '...' does not have authorization to perform action
'Microsoft.Authorization/roleAssignments/write'
```

`User Access Administrator` is missing. Grant it (see
[Prerequisites — Azure RBAC](#prerequisites) above) and retry. The preflight
check (`azureclaw up --dry-run`) will catch this without deploying.

### `FeatureNotRegistered` during Bicep

```
Feature 'Microsoft.Compute/EncryptionAtHost' is not registered
```

Register and wait for propagation (5–15 minutes):

```bash
az feature register --namespace Microsoft.Compute --name EncryptionAtHost
az provider register -n Microsoft.Compute
```

Poll with `az feature show --namespace Microsoft.Compute --name EncryptionAtHost`
until `"state": "Registered"`, then re-run `azureclaw up`.

### Duplicate messages in agent inbox

The OpenClaw plugin is being loaded twice without the singleton guard.
Check that `process.env.__AGT_INITIALIZED` is set on first load in
`runtimes/openclaw/src/index.ts`. If you are using a custom plugin build,
do not remove the singleton guard.

### Sub-agent does not receive mesh messages

The background `openclaw agent --local` session in `entrypoint.sh` is not
running. Check the `openclaw` container logs:

```bash
kubectl logs -n azureclaw-my-assistant \
  deployment/my-assistant -c openclaw --tail=50
```

Look for `relay listener started`. If it is absent, the entrypoint likely
exited early. Re-run with `azureclaw push --only sandbox --apply` to redeploy
the sandbox image.

### Node.js 22 fetch ignores `HTTPS_PROXY`

Node.js 22's built-in `fetch()` does not read `HTTPS_PROXY`. The sandbox
ships `proxy-bootstrap.js` to handle this; it is loaded via `NODE_OPTIONS`.
If you see network timeouts in a proxy environment, verify the container
has `NODE_OPTIONS=--require /app/proxy-bootstrap.js` set.
