# AzureClaw CLI Reference

AzureClaw ships 23 top-level commands organised by purpose: **Lifecycle**,
**Operations**, **Configuration**, **Observability**, and **Multi-Agent /
Federation**. Everything you need to go from zero to a production-hardened,
E2E-encrypted agent sandbox is expressed through these commands.

See [README.md](../README.md) for the quick-start (< 5 minutes with
`azureclaw up`). Architecture details live in
[docs/architecture.md](architecture.md). CRD field reference is in
[docs/api/crd-reference.md](api/crd-reference.md).

---

## Global flags

The following flags are applied to the program itself (defined in
`cli/src/cli.ts`):

| Flag | Description |
|---|---|
| `-V, --version` | Print version string and exit |
| `-h, --help` | Show help for the program or any subcommand |

All commands also inherit Commander.js built-in `--help`.

---

## Commands

### Lifecycle

- [up](#azureclaw-up)
- [dev](#azureclaw-dev)
- [add](#azureclaw-add)
- [destroy](#azureclaw-destroy)
- [push](#azureclaw-push)
- [convert](#azureclaw-convert)
- [migrate](#azureclaw-migrate)

### Operations

- [operator](#azureclaw-operator)
- [connect](#azureclaw-connect)
- [handoff](#azureclaw-handoff)
- [status](#azureclaw-status)
- [list](#azureclaw-list)
- [logs](#azureclaw-logs)
- [attest](#azureclaw-attest)

### Configuration

- [credentials](#azureclaw-credentials)
- [model](#azureclaw-model)
- [policy](#azureclaw-policy)
- [egress](#azureclaw-egress)

### Observability

- [trace](#azureclaw-trace)
- [eval](#azureclaw-eval)

### Multi-Agent / Federation

- [mesh](#azureclaw-mesh)
- [pair](#azureclaw-pair)
- [a2a](#azureclaw-a2a)

---

## Lifecycle

### `azureclaw up`

One-command bootstrap: provisions Azure resources (AKS cluster, ACR, Key
Vault, Workload Identity), deploys the AzureClaw Helm chart, and creates a
first sandbox — all from a single invocation. Ideal for new deployments and
for CI pipelines. Use `--upgrade` to skip infra-provisioning and just re-run
Helm + RBAC against an existing cluster.

**Usage:**
```
azureclaw up [options]
```

**Options:**
| Flag | Default | Description |
|---|---|---|
| `--name <name>` | `my-assistant` | Sandbox name |
| `--model <model>` | `gpt-4.1` | AI model deployment name |
| `--policy <preset>` | `developer` | Policy preset: `minimal`, `developer`, `web`, `azure` |
| `--region <region>` | `eastus2` | Azure region |
| `--cluster-name <name>` | `azureclaw` | AKS cluster name |
| `--isolation <level>` | `enhanced` | Pod isolation: `standard` (runc), `enhanced` (runc + strict seccomp), `confidential` (Kata VM) |
| `-g, --resource-group <name>` | — | Resource group name |
| `--skip-infra` | `false` | Skip infrastructure provisioning (reuse existing cluster) |
| `--force-infra` | `false` | Force Bicep deployment even if AKS cluster already exists |
| `--source-acr <server>` | `azureclawacr.azurecr.io` | Source ACR for pre-built images (customer deployments) |
| `--build` | `false` | Build images locally and push to ACR (developer mode) |
| `--foundry-endpoint <url>` | — | Existing Azure AI Foundry project endpoint (`services.ai.azure.com`) |
| `--openai-endpoint <url>` | — | Existing Azure OpenAI endpoint (`openai.azure.com`; derived from Foundry if omitted) |
| `--dry-run` | `false` | Show what would be done without executing |
| `--upgrade` | `false` | Fast upgrade: skip prompts, reuse cached context, re-run Helm + RBAC only |
| `--mesh-peer` | `true` | Enable mesh federation peer (`--no-mesh-peer` to disable) |
| `--global-registry <url>` | — | Use an external AgentMesh registry (skip local registry deployment) |
| `--expose-registry` | `false` | Deploy AGIC Ingress to expose this cluster's registry publicly |
| `--skip-preflight` | `false` | Skip upfront RBAC & provider checks (advanced) |

**Examples:**
```bash
# Full bootstrap with defaults — provisions Azure, deploys controller, creates sandbox
azureclaw up

# Production deployment with Confidential VM isolation in a named resource group
azureclaw up --name prod-agent --isolation confidential -g my-rg --region westus3

# Fast upgrade (skip infra, re-run Helm only)
azureclaw up --upgrade

# Dry run to preview what would be created
azureclaw up --dry-run

# Developer — build images locally, connect to Foundry
azureclaw up --build --foundry-endpoint https://my-project.services.ai.azure.com
```

**See also:** [README quick-start](../README.md), [docs/architecture.md](architecture.md)

---

### `azureclaw dev`

Runs a fully-policy-enforced sandbox locally via Docker for inner-loop
development. Same model routing, same egress policies, and the same
AGT governance layer as AKS — but on your laptop. Requires an existing
Azure OpenAI resource with at least one model deployment. On first run,
prompts for your endpoint, deployment name, and API key.

**Usage:**
```
azureclaw dev [options]
```

**Options:**
| Flag | Default | Description |
|---|---|---|
| `--name <name>` | `dev-agent` | Sandbox name |
| `--model <model>` | `gpt-4.1` | Azure OpenAI model deployment name |
| `--policy <preset>` | `developer` | Policy preset: `minimal`, `developer`, `web`, `azure` |
| `--image <image>` | `azureclaw-sandbox:dev` | Sandbox container image |
| `--build` | `false` | Build sandbox image locally from Dockerfile |
| `--build-base` | `false` | Rebuild the sandbox base image (heavy deps; only needed when upgrading OpenClaw/Python/Go) |
| `--global-registry <url>` | — | Use a shared external registry (enables handoff); skips local relay/registry/postgres |
| `--channels <channels>` | — | Channels to enable: `telegram,slack,discord,whatsapp` (comma-separated) |
| `--telegram-token <token>` | — | Telegram bot token (from BotFather) |
| `--telegram-allow-from <ids>` | — | Telegram user IDs allowed to DM (comma-separated numeric IDs) |
| `--slack-token <token>` | — | Slack bot OAuth token |
| `--discord-token <token>` | — | Discord bot token |
| `--skills <skills>` | — | Skills to activate: `browser,github,summarize,weather` (comma-separated) |
| `--brave-api-key <key>` | — | Brave Search API key |
| `--tavily-api-key <key>` | — | Tavily search API key |
| `--exa-api-key <key>` | — | Exa search API key |
| `--firecrawl-api-key <key>` | — | Firecrawl web scraping API key |
| `--perplexity-api-key <key>` | — | Perplexity API key |
| `--openai-api-key <key>` | — | OpenAI API key (for dual-provider setups) |
| `--base-image <image>` | `mcr.microsoft.com/azurelinux/base/core:3.0` | Azure Linux base image for building sandbox |

**Examples:**
```bash
# Start a local sandbox with default settings (prompts for credentials on first run)
azureclaw dev

# Named sandbox with Telegram channel
azureclaw dev --name my-bot --channels telegram --telegram-token 123456:ABC-DEF

# Enable web-browsing skill with Brave Search
azureclaw dev --skills browser --brave-api-key $BRAVE_KEY

# Build the image from scratch before starting
azureclaw dev --build
```

**See also:** [docs/channels-plugins.md](channels-plugins.md)

---

### `azureclaw add`

Adds a new sandboxed agent to an **existing** AzureClaw cluster. Creates a
`ClawSandbox` CR which the controller reconciles into an isolated namespace,
NetworkPolicy, and inference-router deployment. Supports all four runtime
kinds (openclaw, openai-agents, microsoft-agent-framework, byo).

**Usage:**
```
azureclaw add <name> [options]
```

**Arguments:**
| Name | Required | Description |
|---|---|---|
| `<name>` | Yes | Name for the new sandbox agent |

**Options:**
| Flag | Default | Description |
|---|---|---|
| `--model <model>` | `gpt-4.1` | AI model deployment name |
| `--isolation <level>` | `enhanced` | Isolation level: `standard`, `enhanced`, `confidential` |
| `--token-budget-daily <tokens>` | `0` | Daily token budget (0 = unlimited) |
| `--token-budget-per-request <tokens>` | `0` | Per-request token limit (0 = unlimited) |
| `--agent-instructions <instructions>` | — | System prompt for the Foundry agent |
| `--agent-tools <tools>` | — | Foundry tools: `file_search,web_search,code_interpreter` (comma-separated) |
| `--image <image>` | — | Custom sandbox image (default: from Helm values) |
| `--governance` | `true` | Enable AGT governance (tool policy, trust, audit) |
| `--no-governance` | — | Disable AGT governance |
| `--trust-threshold <score>` | `500` | AGT trust threshold (0–1000) |
| `--policy-profile <profile>` | `default` | AGT policy profile name |
| `--channels <channels>` | — | Channels: `telegram,slack,discord,whatsapp` |
| `--telegram-token <token>` | — | Telegram bot token |
| `--telegram-allow-from <ids>` | — | Allowed Telegram user IDs (comma-separated) |
| `--slack-token <token>` | — | Slack bot OAuth token |
| `--discord-token <token>` | — | Discord bot token |
| `--skills <skills>` | — | Skills: `browser,github,summarize,weather` |
| `--brave-api-key <key>` | — | Brave Search API key |
| `--tavily-api-key <key>` | — | Tavily search API key |
| `--exa-api-key <key>` | — | Exa search API key |
| `--firecrawl-api-key <key>` | — | Firecrawl web scraping API key |
| `--perplexity-api-key <key>` | — | Perplexity API key |
| `--openai-api-key <key>` | — | OpenAI API key (for dual-provider setups) |
| `--learn-egress` | `false` | Enable egress learn mode: observe all domains, then review with `azureclaw egress` |
| `--runtime <kind>` | `openclaw` | Runtime: `openclaw`, `openai-agents`, `microsoft-agent-framework`, `byo` |
| `--byo-image <image>` | — | Container image for `--runtime byo` (must declare `org.azureclaw.runtime.contract=v1`) |
| `--byo-contract-version <version>` | `v1` | BYO contract version |
| `--maf-language <lang>` | `python` | Microsoft Agent Framework language (`python`; `dotnet` is Phase 3) |
| `--dry-run` | `false` | Print the ClawSandbox YAML without applying |

**Examples:**
```bash
# Add a second agent with a 100k token/day budget
azureclaw add researcher --model gpt-4.1 --token-budget-daily 100000

# Add a Telegram-connected agent with enhanced isolation
azureclaw add support-bot --channels telegram --telegram-token $TOKEN --isolation enhanced

# Add a BYO-runtime agent
azureclaw add my-agent --runtime byo --byo-image myacr.azurecr.io/my-agent:latest

# Dry-run: inspect the ClawSandbox YAML before applying
azureclaw add reviewer --dry-run
```

**See also:** [docs/api/crd-reference.md](api/crd-reference.md), [docs/runtime-contract.md](runtime-contract.md), [docs/channels-plugins.md](channels-plugins.md)

---

### `azureclaw destroy`

Tears down sandbox(es) or the entire AzureClaw deployment. Without `--all`
it removes just the named sandbox (or all sandboxes if `<name>` is omitted).
With `--all` it deletes the entire resource group including AKS, ACR, and Key
Vault — use with care.

**Usage:**
```
azureclaw destroy [name] [options]
```

**Arguments:**
| Name | Required | Description |
|---|---|---|
| `[name]` | No | Sandbox name (omit to destroy all sandboxes) |

**Options:**
| Flag | Default | Description |
|---|---|---|
| `-y, --yes` | `false` | Skip confirmation prompt |
| `--local` | `false` | Destroy local Docker sandbox only (skip AKS) |
| `--cloud` | `false` | Destroy AKS cloud sandbox only (skip Docker) |
| `--all` | `false` | Destroy ALL resources (AKS, ACR, KV, AOAI — deletes the resource group) |
| `-g, --resource-group <name>` | — | Resource group name |
| `--region <region>` | `eastus2` | Azure region (used to derive resource group) |

**Examples:**
```bash
# Destroy a single sandbox (prompts for confirmation)
azureclaw destroy my-agent

# Destroy without prompting
azureclaw destroy my-agent -y

# Destroy all sandboxes without touching infrastructure
azureclaw destroy -y

# Destroy everything, including the resource group
azureclaw destroy --all -y -g my-rg
```

---

### `azureclaw push`

Builds and pushes AzureClaw images (controller, inference router, sandbox,
relay, registry) to ACR using the cached context from the last `azureclaw up`
run. Use `--apply` to restart deployments so pods immediately pick up new
images. Use `--only sandbox` + `--apply` after modifying `entrypoint.sh`,
plugins, or skills.

**Usage:**
```
azureclaw push [options]
```

**Options:**
| Flag | Default | Description |
|---|---|---|
| `--acr <name>` | *(from last deploy)* | ACR name |
| `--only <image>` | — | Build only one image: `controller`, `router`, `sandbox`, `sandbox-base`, `relay`, `registry` |
| `--include-base` | `false` | Include `sandbox-base` in a full push (skipped by default — rebuild only when upgrading OpenClaw/Python/Go) |
| `--apply` | `false` | Restart deployments after push so pods pick up new images |

**Examples:**
```bash
# Push all images and restart pods
azureclaw push --apply

# Push only the sandbox image and restart pods (common after plugin changes)
azureclaw push --only sandbox --apply

# Push only the controller image without restarting
azureclaw push --only controller
```

**See also:** [docs/architecture.md](architecture.md)

---

### `azureclaw convert`

Translates manifests between `ClawSandbox` and the upstream
`agents.x-k8s.io/v1alpha1 Sandbox` format (and the `overlay` variant). Hard-fails
on lossy translations by default; pass `--allow-lossy` to proceed with
warnings. See [docs/internal/sigs-agent-sandbox-compat.md](sigs-agent-sandbox-compat.md)
for the normative field mapping.

**Usage:**
```
azureclaw convert [options]
```

**Options:**
| Flag | Default | Description |
|---|---|---|
| `-f, --file <path>` | *(required)* | Source manifest YAML |
| `--to <target>` | `clawsandbox` | Target kind: `clawsandbox`, `upstream-sandbox`, `overlay` |
| `--sandbox-ref <ns/name>` | — | For `--to overlay`: reference to an existing Sandbox CR |
| `--dry-run` | `false` | Validate + translate without emitting the converted manifest |
| `--allow-lossy` | `false` | Proceed even when translation drops fields with no analog |

**Examples:**
```bash
# Convert an upstream Sandbox YAML to a ClawSandbox
azureclaw convert -f sandbox.yaml --to clawsandbox > clawsandbox.yaml

# Convert a ClawSandbox to upstream format, allowing lossy translation
azureclaw convert -f clawsandbox.yaml --to upstream-sandbox --allow-lossy

# Convert to overlay mode referencing an existing Sandbox CR
azureclaw convert -f sandbox.yaml --to overlay --sandbox-ref=prod/web
```

**See also:** [docs/internal/sigs-agent-sandbox-compat.md](sigs-agent-sandbox-compat.md)

---

### `azureclaw migrate`

Switches a `ClawSandbox` between upstream-compatibility modes (`native`,
`overlay`, `translate`, `observe`) by wrapping a `kubectl patch` with
validation, before/after summary, and dry-run support. Also provides
`from-kagent` to translate a `kagent.dev/v1alpha2` Agent YAML into an
AzureClaw resource bundle.

**Usage:**
```
azureclaw migrate <subcommand> [arguments] [options]
```

**Subcommands:**
| Subcommand | Description |
|---|---|
| `to-overlay <name>` | Flip to overlay mode; AzureClaw provides governance overlay; upstream CR owns the Pod. Requires `--upstream-ref`. |
| `from-overlay <name>` | Leave overlay mode; revert to native AzureClaw (controller resumes ownership). |
| `to-translate <name>` | Accept upstream SandboxClaim semantics on inbound (Phase 1 schema-only). |
| `to-observe <name>` | Mirror status of an upstream Sandbox CR without overlay. |
| `to-native <name>` | Reset to default native mode (AzureClaw owns the workload). |
| `from-kagent <input>` | Translate a `kagent.dev/v1alpha2` Agent YAML into an AzureClaw resource bundle. Use `-` to read from stdin. |

**Common options (all subcommands except `from-kagent`):**
| Flag | Default | Description |
|---|---|---|
| `-n, --namespace <ns>` | `azureclaw-system` | Namespace where the ClawSandbox CR lives |
| `--dry-run` | `false` | Print the JSON merge patch without applying |
| `--format <fmt>` | `human` | Output format: `human` or `json` |

**Additional options for `to-overlay`:**
| Flag | Default | Description |
|---|---|---|
| `--upstream-ref <name>` | *(required)* | Name of the upstream Sandbox CR in the same namespace |

**Options for `from-kagent`:**
| Flag | Default | Description |
|---|---|---|
| `-n, --namespace <ns>` | — | Override `metadata.namespace` on emitted resources |
| `--isolation <mode>` | `enhanced` | ClawSandbox isolation mode: `standard`, `enhanced`, `confidential` |
| `--image <image>` | — | Override `spec.runtime.openclaw.image` |
| `--allow-lossy` | `false` | Waive the hard-fail on lossy translation |
| `--out-dir <dir>` | — | Write each emitted resource to `<dir>/<kind>-<name>.yaml` |
| `--force` | `false` | With `--out-dir`, overwrite existing files |
| `--format <fmt>` | `yaml` | Output format: `yaml` (multi-doc) or `json` (List) |
| `--dry-run` | `false` | Print summary + warnings; emit no resources |

**Examples:**
```bash
# Switch to overlay mode
azureclaw migrate to-overlay my-agent --upstream-ref upstream-sandbox

# Revert to native mode (dry-run first)
azureclaw migrate to-native my-agent --dry-run
azureclaw migrate to-native my-agent

# Import from kagent YAML
azureclaw migrate from-kagent agent.yaml --isolation enhanced --out-dir ./manifests

# Import from stdin
cat kagent-agent.yaml | azureclaw migrate from-kagent -
```

**See also:** [docs/internal/sigs-agent-sandbox-compat.md](sigs-agent-sandbox-compat.md)

---

## Operations

### `azureclaw operator`

Live operator dashboard — a full-screen TUI that shows all sandboxes,
their policy state, inference stats, and logs from a single screen.
Supports both AKS (K8s pods) and local Docker (dev mode). Panels can
be filtered, grouped per sandbox, or rendered as a one-shot snapshot
for scripting and CI.

**Usage:**
```
azureclaw operator [options]
```

**Options:**
| Flag | Default | Description |
|---|---|---|
| `--refresh <seconds>` | `10` | Auto-refresh interval (seconds) |
| `--context <name>` | — | Kubernetes context to use |
| `--dev` | `false` | Dev mode — discover Docker containers instead of K8s pods |
| `--panels <list>` | *(all)* | Comma-separated panel IDs to show |
| `--per-sandbox` | `false` | Group panels vertically per sandbox name |
| `--snapshot` | `false` | Render one snapshot to stdout and exit (non-interactive) |

**Examples:**
```bash
# Open full operator TUI
azureclaw operator

# Local dev mode, faster refresh
azureclaw operator --dev --refresh 3

# Capture a one-shot snapshot for a status page
azureclaw operator --snapshot

# Show specific panels grouped per sandbox
azureclaw operator --panels status,logs --per-sandbox
```

**See also:** [docs/operator-tui.md](operator-tui.md)

---

### `azureclaw connect`

Connects to a running sandbox — either as a shell (bash), as the OpenClaw
TUI, or via WebUI (port-forwarded to a local port). Defaults to the
OpenClaw TUI on Docker and to WebUI on AKS.

**Usage:**
```
azureclaw connect <name> [options]
```

**Arguments:**
| Name | Required | Description |
|---|---|---|
| `<name>` | Yes | Sandbox name |

**Options:**
| Flag | Default | Description |
|---|---|---|
| `--shell` | `false` | Drop to bash shell instead of OpenClaw TUI |
| `--web` | `false` | Open WebUI via port-forward (default for AKS) |
| `--local` | `false` | Connect to local Docker sandbox (skip AKS) |
| `--cloud` | `false` | Connect to AKS cloud sandbox (skip Docker) |
| `--port <port>` | `18789` | Local port for WebUI |

**Examples:**
```bash
# Connect to the OpenClaw TUI
azureclaw connect my-agent

# Open WebUI in browser (port-forwarded)
azureclaw connect my-agent --web

# Drop into a bash shell for debugging
azureclaw connect my-agent --shell

# Connect to the local Docker sandbox explicitly
azureclaw connect my-agent --local
```

---

### `azureclaw handoff`

Live-migrates an agent between local Docker and AKS (bidirectional handoff).
Uses the AgentMesh relay to transfer session state with no dropped requests.
Requires a shared registry (either `--global-registry` or a promoted AKS
registry reachable from both sides).

**Usage:**
```
azureclaw handoff <name> [options]
```

**Arguments:**
| Name | Required | Description |
|---|---|---|
| `<name>` | Yes | Sandbox name |

**Options:**
| Flag | Default | Description |
|---|---|---|
| `--to <target>` | — | Handoff target: `cloud` or `local` |
| `--status` | `false` | Show current handoff status |
| `--abort` | `false` | Abort an in-progress handoff |

**Examples:**
```bash
# Handoff from local Docker to AKS
azureclaw handoff my-agent --to cloud

# Handoff from AKS back to local
azureclaw handoff my-agent --to local

# Check handoff status
azureclaw handoff my-agent --status

# Abort an in-progress handoff
azureclaw handoff my-agent --abort
```

**See also:** [docs/architecture.md](architecture.md), [docs/internal/e2e-encryption-proof.md](e2e-encryption-proof.md)

---

### `azureclaw status`

Shows sandbox health, policy state, and inference configuration in a
human-readable summary. Includes the pod phase, readiness, active policy
profile, model configuration, and recent condition transitions.

**Usage:**
```
azureclaw status <name>
```

**Arguments:**
| Name | Required | Description |
|---|---|---|
| `<name>` | Yes | Sandbox name |

**Examples:**
```bash
azureclaw status my-agent
```

---

### `azureclaw list`

Lists all AzureClaw sandboxes across both Docker (local) and AKS (cloud)
environments. Shows name, runtime, status, and model for each sandbox.

**Usage:**
```
azureclaw list [options]
```

**Options:**
| Flag | Default | Description |
|---|---|---|
| `--aks-only` | `false` | Only show AKS sandboxes |
| `--docker-only` | `false` | Only show local Docker sandboxes |

**Examples:**
```bash
# List all sandboxes
azureclaw list

# List AKS sandboxes only
azureclaw list --aks-only
```

---

### `azureclaw logs`

Streams agent and platform logs from a sandbox. Can tail logs from all
services or filter to a specific component: the inference router, OpenClaw
gateway, or the node host process.

**Usage:**
```
azureclaw logs <name> [options]
```

**Arguments:**
| Name | Required | Description |
|---|---|---|
| `<name>` | Yes | Sandbox name |

**Options:**
| Flag | Default | Description |
|---|---|---|
| `-f, --follow` | `false` | Follow log output (stream continuously) |
| `--tail <lines>` | `100` | Number of lines to show from the end |
| `--service <svc>` | `all` | Service: `router`, `gateway`, `openclaw`, `node-host`, `all` |

**Examples:**
```bash
# Show the last 100 lines from all services
azureclaw logs my-agent

# Stream router logs in real time
azureclaw logs my-agent --service router -f

# Show last 200 lines from the OpenClaw gateway
azureclaw logs my-agent --service openclaw --tail 200
```

---

### `azureclaw attest`

Prints a deterministic attestation receipt for a sandbox: spec hash, SSA
field owners, referenced policy versions, and reconcile trace. Pass
`--baseline` to diff against a previously saved attestation; exit code
reflects drift (0 = match, 2 = drift, 3 = baseline missing).

Full signature and AGT receipt are Phase 3 deliverables.

**Usage:**
```
azureclaw attest <name> [options]
```

**Arguments:**
| Name | Required | Description |
|---|---|---|
| `<name>` | Yes | Sandbox name |

**Options:**
| Flag | Default | Description |
|---|---|---|
| `-n, --namespace <ns>` | `azureclaw-system` | Namespace where the ClawSandbox CR lives |
| `--format <fmt>` | `human` | Output format: `human` or `json` |
| `--baseline <path>` | — | Path to a previously-emitted attestation JSON to diff against |

**Examples:**
```bash
# Print attestation receipt in human-readable form
azureclaw attest my-agent

# Save attestation as a JSON baseline
azureclaw attest my-agent --format json > attestation-2026-04-30.json

# Diff against saved baseline (exits 2 on drift)
azureclaw attest my-agent --format json --baseline attestation-2026-04-30.json
echo $?  # 0=match 2=drift 3=missing baseline
```

**See also:** [docs/api/crd-reference.md](api/crd-reference.md)

---

## Configuration

### `azureclaw credentials`

Manages AzureClaw credentials (Azure OpenAI endpoint/key, channel tokens,
third-party API keys). Invoking without a subcommand opens an interactive
guided prompt. Use `credentials set` / `list` / `remove` for scripting.
Use `credentials update` to patch a running AKS sandbox's K8s Secret without
restarting the pod (unless you want a restart).

**Usage:**
```
azureclaw credentials [subcommand] [arguments] [options]
```

**Subcommands:**
| Subcommand | Description |
|---|---|
| *(no subcommand)* | Interactive guided credential setup |
| `set <key> [value]` | Store a secret locally (prompts if value is omitted) |
| `list` | List all stored secrets (values masked) |
| `remove <key>` | Remove a stored secret |
| `update <name>` | Update credentials for a running AKS sandbox (updates K8s Secret + optionally restarts pod) |

**Arguments for `set`:**
| Name | Required | Description |
|---|---|---|
| `<key>` | Yes | Secret key (e.g. `telegram-token`, `brave-api-key`) |
| `[value]` | No | Secret value (omit for masked prompt) |

**Arguments for `remove`:**
| Name | Required | Description |
|---|---|---|
| `<key>` | Yes | Secret key to remove |

**Arguments for `update`:**
| Name | Required | Description |
|---|---|---|
| `<name>` | Yes | Sandbox name |

**Options for `update`:**
| Flag | Default | Description |
|---|---|---|
| `--telegram-token <token>` | — | New Telegram bot token |
| `--telegram-allow-from <ids>` | — | Allowed Telegram user IDs (comma-separated) |
| `--slack-token <token>` | — | New Slack bot token |
| `--discord-token <token>` | — | New Discord bot token |
| `--brave-api-key <key>` | — | New Brave Search API key |
| `--tavily-api-key <key>` | — | New Tavily API key |
| `--exa-api-key <key>` | — | New Exa API key |
| `--firecrawl-api-key <key>` | — | New Firecrawl API key |
| `--perplexity-api-key <key>` | — | New Perplexity API key |
| `--openai-api-key <key>` | — | New OpenAI API key |
| `--no-restart` | — | Update secret without restarting the pod |

**Examples:**
```bash
# Interactive guided setup
azureclaw credentials

# Store a Telegram token
azureclaw credentials set telegram-token 123456:ABC-DEF

# List stored secrets
azureclaw credentials list

# Update a running sandbox's Telegram token and restart
azureclaw credentials update my-agent --telegram-token 999999:NEW-TOKEN

# Update without restarting the pod
azureclaw credentials update my-agent --brave-api-key $KEY --no-restart
```

**See also:** [docs/channels-plugins.md](channels-plugins.md)

---

### `azureclaw model`

Manages the AI model for a sandbox. The `set` subcommand switches models
instantly without a pod restart (the change is applied via a hot ConfigMap
patch). Use `list` to discover available models from your Foundry project.

**Usage:**
```
azureclaw model <subcommand> [arguments]
```

**Subcommands:**
| Subcommand | Description |
|---|---|
| `set <name> <model>` | Switch the AI model for a sandbox (instant, no restart) |
| `get <name>` | Show the current model for a sandbox |
| `list [name]` | List available models from Foundry (queries live if sandbox name is provided) |

**Arguments for `set`:**
| Name | Required | Description |
|---|---|---|
| `<name>` | Yes | Sandbox name |
| `<model>` | Yes | Model name (e.g. `gpt-4.1`, `Phi-4`, `Meta-Llama-3.1-405B-Instruct`) |

**Arguments for `get` / `list`:**
| Name | Required | Description |
|---|---|---|
| `<name>` | Yes (`get`) / No (`list`) | Sandbox name |

**Examples:**
```bash
# Switch a sandbox to Phi-4
azureclaw model set my-agent Phi-4

# Show the current model
azureclaw model get my-agent

# List available Foundry models
azureclaw model list my-agent
```

---

### `azureclaw policy`

Manages sandbox network and security policies. Hot-reload capable: `allow`
and `deny` take effect without a pod restart. `learn` is deprecated in favour
of `azureclaw egress <name> --learned`.

**Usage:**
```
azureclaw policy <subcommand> [arguments] [options]
```

**Subcommands:**
| Subcommand | Description |
|---|---|
| `allow <name> <host>` | Add an allowed egress endpoint (hot-reload) |
| `get <name>` | Show the active policy for a sandbox |
| `deny <name> <host>` | Remove an allowed endpoint from a running sandbox |
| `learn <name>` | *(Deprecated)* Use `azureclaw egress <name> --learned` instead |

**Arguments for `allow` / `deny`:**
| Name | Required | Description |
|---|---|---|
| `<name>` | Yes | Sandbox name |
| `<host>` | Yes | Hostname (e.g. `api.github.com`) |

**Options for `allow`:**
| Flag | Default | Description |
|---|---|---|
| `--port <port>` | `443` | Port |

**Options for `learn` (deprecated):**
| Flag | Default | Description |
|---|---|---|
| `--apply` | `false` | Apply learned domains as the sandbox allowlist |
| `--clear` | `false` | Clear learned domains after export |

**Examples:**
```bash
# Allow GitHub API for a sandbox
azureclaw policy allow my-agent api.github.com

# Allow on a non-standard port
azureclaw policy allow my-agent internal.corp.com --port 8443

# Show current policy
azureclaw policy get my-agent

# Remove a domain
azureclaw policy deny my-agent api.github.com
```

**See also:** [docs/egress-proxy.md](egress-proxy.md)

---

### `azureclaw egress`

Full egress lifecycle management: learn mode (observe domains without
blocking), pending approvals, allowlist management, and signed OCI artifact
generation + cosign signing. With `--enforce`, all learned domains are
promoted to the allowlist and enforcement mode is activated. Signing is
on by default when combined with `--enforce` or `--approve`; the controller
will refuse to use unsigned artifacts in authoritative mode
(`SignerPolicyMissing`).

**Usage:**
```
azureclaw egress [name] [options]
```

**Arguments:**
| Name | Required | Description |
|---|---|---|
| `[name]` | No | Sandbox name (default: `demo-agent`) |

**Options:**
| Flag | Default | Description |
|---|---|---|
| `--namespace <ns>` | — | Kubernetes namespace |
| `--learn` | — | Enable learn mode (log all accessed domains) |
| `--no-learn` | — | Disable learn mode |
| `--learned` | — | Show domains discovered during learn mode |
| `--pending` | — | Show domains pending operator approval |
| `--approve <domain>` | — | Approve a domain for egress |
| `--deny <domain>` | — | Deny and remove a pending domain request |
| `--allowlist` | — | Show currently approved domains |
| `--enforce` | — | Graduate: promote all learned domains to allowlist, switch to enforcement mode |
| `--status` | — | Show blocklist and learn mode status |
| `--sign` | *(on with `--enforce`/`--approve`)* | Build canonical allowlist artifact, push to OCI registry, sign with cosign, patch `allowlistRef` |
| `--no-sign` | — | Skip signing (controller refuses the artifact in authoritative mode) |
| `--sign-mode <mode>` | *(auto-detect)* | Cosign mode: `keyless`, `identity-token`, `keyed` |
| `--sign-key <ref>` | — | Cosign key reference (path or KMS URI like `azurekms://...`) — required for `--sign-mode keyed` |
| `--registry <fqdn>` | *(auto-discover)* | Override target ACR for the artifact push |
| `--repository <repo>` | `policy/egress-allowlist/<sandbox>` | Repository path within the registry |
| `--emit-manifest <path>` | — | GitOps mode: write the ClawSandbox patch to `<path>` instead of running `kubectl patch` |
| `--force` | `false` | With `--emit-manifest`, overwrite an existing file |

**Examples:**
```bash
# Enable learn mode
azureclaw egress my-agent --learn

# Review discovered domains
azureclaw egress my-agent --learned

# Approve a domain (signs the updated allowlist automatically)
azureclaw egress my-agent --approve api.github.com

# Graduate to enforcement mode (signs + patches)
azureclaw egress my-agent --enforce

# GitOps mode: emit patch file instead of applying
azureclaw egress my-agent --enforce --emit-manifest ./patches/egress-my-agent.yaml

# Sign with a KMS key
azureclaw egress my-agent --approve api.github.com --sign-mode keyed --sign-key azurekms://myvault.vault.azure.net/keys/cosign
```

**See also:** [docs/egress-proxy.md](egress-proxy.md), [docs/internal/policy-canonical-format.md](policy-canonical-format.md)

---

## Observability

### `azureclaw trace`

Live eBPF trace using `kubectl-gadget` — surfaces network connections, file
access, and process executions in the sandbox container in real time. Requires
`kubectl-gadget` to be installed. Without filter flags all event types are shown.

**Usage:**
```
azureclaw trace <name> [options]
```

**Arguments:**
| Name | Required | Description |
|---|---|---|
| `<name>` | Yes | Sandbox name |

**Options:**
| Flag | Default | Description |
|---|---|---|
| `--network` | `false` | Show network connections only |
| `--files` | `false` | Show file operations only |
| `--exec` | `false` | Show process executions only |
| `--dns` | `false` | Show DNS lookups only |

**Examples:**
```bash
# Trace all events
azureclaw trace my-agent

# Show only outbound network connections
azureclaw trace my-agent --network

# Show DNS lookups in real time
azureclaw trace my-agent --dns
```

---

### `azureclaw eval`

Runs Azure AI Foundry evaluations against a sandbox agent using JSONL
test datasets and built-in or custom evaluators. Useful for measuring
relevance, coherence, and fluency before promoting a model or system
prompt change.

**Usage:**
```
azureclaw eval <name> [options]
```

**Arguments:**
| Name | Required | Description |
|---|---|---|
| `<name>` | Yes | Sandbox name |

**Options:**
| Flag | Default | Description |
|---|---|---|
| `--model <model>` | `gpt-4.1` | Model to evaluate |
| `--evaluator <id>` | — | Evaluator ID (e.g. `relevance`, `coherence`, `fluency`) |
| `--dataset <path>` | — | JSONL dataset file with test cases |
| `--list-evaluators` | — | List available evaluators in the Foundry project |
| `--list-runs` | — | List existing evaluation runs |

**Examples:**
```bash
# List available evaluators
azureclaw eval my-agent --list-evaluators

# Run relevance evaluation against a dataset
azureclaw eval my-agent --evaluator relevance --dataset ./tests/eval-set.jsonl

# List past evaluation runs
azureclaw eval my-agent --list-runs
```

---

## Multi-Agent / Federation

### `azureclaw mesh`

Manages AgentMesh identity and authentication for cross-environment agent
handoff and federation. Controls the Ed25519 mesh identity (stored
AES-256-GCM encrypted at `~/.azureclaw/mesh-identity.json`), relay
registration enforcement, and cluster federation peer state.

**Usage:**
```
azureclaw mesh <subcommand> [arguments] [options]
```

**Subcommands:**
| Subcommand | Description |
|---|---|
| `auth` | Authenticate with an AgentMesh registry via OAuth |
| `status` | Show current mesh identity |
| `list` | List mesh pairings and offload sandboxes on the cluster |
| `reset` | Delete mesh identity (requires re-authentication) |
| `security <mode>` | Toggle relay `REQUIRE_REGISTRATION` (mode: `open`, `strict`, `status`) |
| `peer <mode>` | Toggle controller mesh federation (mode: `enable`, `disable`, `status`) |
| `promote` | Promote the AKS cluster registry to a public global endpoint |
| `unpair` | Delete mesh pairings from the AKS cluster |
| `demote` | Demote the registry back to cluster-local (remove public endpoints) |

**Options for `auth`:**
| Flag | Default | Description |
|---|---|---|
| `--provider <provider>` | `github` | OAuth provider: `github`, `entra` |
| `--no-browser` | — | Print URL instead of opening browser |

**Options for `security`:**
| Flag | Default | Description |
|---|---|---|
| `-n, --namespace <ns>` | `agentmesh` | AgentMesh namespace |
| `--deployment <name>` | `relay` | Relay deployment name |

**Options for `peer`:**
| Flag | Default | Description |
|---|---|---|
| `-n, --namespace <ns>` | `azureclaw-system` | Controller namespace |
| `--deployment <name>` | `azureclaw-controller` | Controller deployment name |

**Options for `promote`:**
| Flag | Default | Description |
|---|---|---|
| `--allow-ip <cidr>` | — | Restrict access to this IP/CIDR (LoadBalancer mode) |
| `--port-forward` | — | Use `kubectl port-forward` instead of LoadBalancer (recommended for Cilium clusters) |
| `--registry-port <port>` | `18080` | Local port for registry (port-forward mode) |
| `--relay-port <port>` | `18765` | Local port for relay (port-forward mode) |

**Options for `unpair`:**
| Flag | Default | Description |
|---|---|---|
| `--all` | — | Delete all pairings without prompting |
| `--name <name>` | — | Delete a specific pairing by name |

**Examples:**
```bash
# Authenticate with GitHub OAuth
azureclaw mesh auth

# Check current mesh identity
azureclaw mesh status

# List pairings on the cluster
azureclaw mesh list

# Enable strict registration on the relay
azureclaw mesh security strict

# Enable controller federation
azureclaw mesh peer enable

# Promote cluster registry to public endpoint
azureclaw mesh promote --port-forward

# Demote back to cluster-local
azureclaw mesh demote

# Delete a specific pairing
azureclaw mesh unpair --name my-peer
```

**See also:** [docs/internal/e2e-encryption-proof.md](e2e-encryption-proof.md)

---

### `azureclaw pair`

Manages federation pairings for external agent cloud offload. Generate a
one-time token that an external agent (e.g., running `azureclaw dev` on
another machine) can use to register as a federation peer and offload
sandboxes into this cluster.

**Usage:**
```
azureclaw pair <subcommand> [arguments] [options]
```

**Subcommands:**
| Subcommand | Description |
|---|---|
| `generate` | Generate a one-time pairing token for an external agent |
| `list` | List all federation pairings |
| `revoke <name>` | Revoke a pairing (blocks future offloads) |
| `inspect <name>` | Show detailed info about a pairing |

**Arguments for `revoke` / `inspect`:**
| Name | Required | Description |
|---|---|---|
| `<name>` | Yes | Name of the pairing |

**Options for `generate`:**
| Flag | Default | Description |
|---|---|---|
| `--expires <duration>` | `90d` | Expiry duration (e.g. `90d`, `30d`, `7d`) |
| `--token-budget <tokens>` | `500000` | Maximum tokens for offloads |
| `--slots <n>` | `1` | Maximum concurrent offload sandboxes |
| `--capabilities <list>` | `offload,handoff` | Capabilities: `offload,handoff` |
| `--relay-url <url>` | `ws://host.docker.internal:18765` | AgentMesh relay URL |
| `--registry-url <url>` | `http://host.docker.internal:18080` | AgentMesh registry URL |

**Examples:**
```bash
# Generate a 30-day pairing token
azureclaw pair generate --expires 30d

# Generate a token with a tighter token budget
azureclaw pair generate --token-budget 100000 --slots 2

# List pairings
azureclaw pair list

# Inspect a pairing
azureclaw pair inspect my-peer

# Revoke a pairing
azureclaw pair revoke my-peer
```

**See also:** [docs/internal/e2e-encryption-proof.md](e2e-encryption-proof.md)

---

### `azureclaw a2a`

A2A 1.0.0 ingress surfacing commands (per
[docs/adr/0001-a2a-ingress-front-edge.md](adr/0001-a2a-ingress-front-edge.md)).
`list-exposed` shows every sandbox currently exposed for inbound A2A traffic
so operators can verify the blast radius at a glance. The full A2A routing
ConfigMap (`azureclaw-a2a-routes`) lands in `phase1/a2a-controller-revocation`;
until then `list-exposed` returns an empty table (correct: no agents are
exposed in current builds).

**Usage:**
```
azureclaw a2a <subcommand> [options]
```

**Subcommands:**
| Subcommand | Description |
|---|---|
| `list-exposed` | List sandboxes currently exposed for inbound A2A traffic (allowed callers, expiry, advertised skills, rate limits) |
| `schema` | Print the AgentCard JSON shape this cluster will publish per A2A 1.0.0 §4.4 |

**Options for `list-exposed`:**
| Flag | Default | Description |
|---|---|---|
| `-n, --namespace <ns>` | *(all sandbox namespaces)* | Restrict to a single namespace |
| `-o, --output <fmt>` | `table` | Output format: `table`, `json`, `yaml` |

**Examples:**
```bash
# List exposed sandboxes
azureclaw a2a list-exposed

# Machine-readable output
azureclaw a2a list-exposed --output json

# Show the AgentCard schema this cluster will publish
azureclaw a2a schema
```

**See also:** [docs/adr/0001-a2a-ingress-front-edge.md](adr/0001-a2a-ingress-front-edge.md)
