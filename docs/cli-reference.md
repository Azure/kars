# kars CLI Reference

kars ships **dozens of top-level commands** organised by purpose: **Lifecycle**,
**Operations**, **Configuration**, **Observability**, and the
**Multi-Agent / Federation** family (Agent mobility, Interop, Governance).
Everything you need to go from zero to a production-hardened, E2E-encrypted
agent sandbox is expressed through these commands.

See [README.md](../README.md) for the five-minute quick-start with
`kars dev`, and [getting-started.md](getting-started.md) for the
full walkthrough including `kars up` against AKS. Architecture details
live in [docs/architecture.md](architecture.md). CRD field reference is in
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

- [up](#kars-up)
- [dev](#kars-dev)
- [add](#kars-add)
- [destroy](#kars-destroy)
- [push](#kars-push)
- [convert](#kars-convert)
- [migrate](#kars-migrate)

### Operations

- [operator](#kars-operator)
- [connect](#kars-connect)
- [handoff](#kars-handoff)
- [status](#kars-status)
- [list](#kars-list)
- [logs](#kars-logs)
- [inspect](#kars-inspect)
- [audit](#kars-audit)
- [attest](#kars-attest)

### Configuration

- [credentials](#kars-credentials)
- [config](#kars-config)
- [model](#kars-model)
- [policy](#kars-policy)
- [egress](#kars-egress)

### Observability

- [trace](#kars-trace)
- [eval](#kars-eval)

### Agent mobility

- [mesh](#kars-mesh)
- [pair](#kars-pair)

### Interop

- [a2a](#kars-a2a)
- [a2a-agent](#kars-a2a-agent)

### Governance

- [toolpolicy](#kars-toolpolicy)
- [inferencepolicy](#kars-inferencepolicy)
- [mcp](#kars-mcp)
- [memory](#kars-memory)

---

## Lifecycle

### `kars up`

One-command bootstrap: provisions Azure resources (AKS cluster, ACR, Key
Vault, Workload Identity), deploys the kars Helm chart, and creates a
first sandbox — all from a single invocation. Ideal for new deployments and
for CI pipelines. Use `--upgrade` to skip infra-provisioning and just re-run
Helm + RBAC against an existing cluster.

**Usage:**
```
kars up [options]
```

**Options:**
| Flag | Default | Description |
|---|---|---|
| `--name <name>` | `my-assistant` | Sandbox name |
| `--model <model>` | `gpt-4.1` | AI model deployment name |
| `--policy <preset>` | `developer` | Policy preset: `minimal`, `developer`, `web`, `azure` |
| `--region <region>` | `eastus2` | Azure region |
| `--cluster-name <name>` | `kars` | AKS cluster name |
| `--isolation <level>` | `enhanced` | Pod isolation: `standard` (runc), `enhanced` (runc + strict seccomp), `confidential` (Kata VM) |
| `-g, --resource-group <name>` | — | Resource group name |
| `--skip-infra` | `false` | Skip infrastructure provisioning (reuse existing cluster) |
| `--force-infra` | `false` | Force Bicep deployment even if AKS cluster already exists |
| `--source-acr <server>` | `karsacr.azurecr.io` | Source ACR for pre-built images (customer deployments) |
| `--build` | `false` | Build images locally and push to ACR (developer mode) |
| `--skip-runtime-images` | `false` | Skip building/importing the 7 multi-runtime adapter images (faster first deploy; only OpenClaw + BYO will be runnable) |
| `--foundry-endpoint <url>` | — | Existing Azure AI Foundry project endpoint (`services.ai.azure.com`) |
| `--openai-endpoint <url>` | — | Existing Azure OpenAI endpoint (`openai.azure.com`; derived from Foundry if omitted) |
| `--service-tree <guid>` | — | ServiceTree / `serviceManagementReference` GUID for the Entra blueprint. Required only in Microsoft-style enterprise tenants. Falls back to `KARS_SERVICE_TREE` env var. |
| `--dry-run` | `false` | Show what would be done without executing |
| `--upgrade` | `false` | Fast upgrade: skip prompts, reuse cached context, re-run Helm + RBAC only |
| `--from-scratch` | `false` | Ignore any partial state from a prior failed run and start over |
| `--mesh-peer` | `true` | Enable mesh federation peer (`--no-mesh-peer` to disable) |
| `--global-registry <url>` | — | Use an external AgentMesh registry (skip local registry deployment) |
| `--expose-registry` | `false` | Deploy AGIC Ingress to expose this cluster's registry publicly |
| `--skip-preflight` | `false` | Skip upfront RBAC & provider checks (advanced) |

**Examples:**
```bash
# Full bootstrap with defaults — provisions Azure, deploys controller, creates sandbox
kars up

# Production deployment with Confidential VM isolation in a named resource group
kars up --name prod-agent --isolation confidential -g my-rg --region westus3

# Fast upgrade (skip infra, re-run Helm only)
kars up --upgrade

# Dry run to preview what would be created
kars up --dry-run

# Developer — build images locally, connect to Foundry
kars up --build --foundry-endpoint https://my-project.services.ai.azure.com

# Microsoft-corporate tenant (or any tenant that mandates ServiceTree)
kars up --service-tree 1c826d4f-22b0-4c67-b755-778a05d7ffc9

# Force a clean run (discard any auto-resume state)
kars up --from-scratch
```

**Auto-resume:** If `kars up` fails mid-flight (e.g. a transient quota
error during image push), the next run automatically picks up where the
previous one left off. State lives in `~/.kars/context.json` and tracks
which phases (`rg`, `infra`, `network`, `kubectl`, `images`, `helm`, `mesh`,
`sandbox`) succeeded. On resume, the slow `network` and `images` phases are
skipped if they already completed; everything else is re-run idempotently.
The state is invalidated automatically when:
- Topology changes (`--region`, `--resource-group`, `--cluster-name`, `--name`, `--source-acr`)
- The saved state is older than 7 days
- The previous run completed successfully (`phase: complete`)
- You pass `--from-scratch`

**See also:** [README quick-start](../README.md), [docs/architecture.md](architecture.md)

---

### `kars dev`

Runs a fully-policy-enforced sandbox locally via Docker for inner-loop
development. Same model routing, same egress policies, and the same
AGT governance layer as AKS — but on your laptop.

**Three inference providers** are supported. On first run you'll be asked
to pick one; your choice is saved to `~/.kars/config.json` and
reused on subsequent runs:

| Provider | Requires | Saved as | Trade-offs |
|---|---|---|---|
| **GitHub Copilot** *(default)* | An active GitHub Copilot seat (Individual / Business / Enterprise). Auth is interactive **device-code OAuth** — no PAT to manage | `provider: "github-copilot"` | Frontier model catalogue (Claude Opus / Sonnet, GPT-5, GPT-4.1, Gemini, o-series), large context windows, native Anthropic-shape passthrough for Claude. Foundry-only routes (Memory Store, agents, evaluations, indexes, Content Safety inline) return `501`. Inline `prompt_filter_results` not enforced (Copilot doesn't return them). Subject to Copilot quota on your seat. |
| **Azure AI Foundry / Azure OpenAI** | Existing Foundry or Azure OpenAI resource + API key | `provider: "foundry"` | Full feature set: Memory Store, agents, evaluations, Content Safety inline, indexes |
| **GitHub Models** | A GitHub PAT with `models:read` scope | `provider: "github-models"` | Free, no Azure subscription needed. Smaller context windows. Foundry-only routes (Memory Store, agents, evaluations, indexes, Content Safety inline) return `501`. Subject to GitHub Models rate limits. |

**Usage:**
```
kars dev [options]
```

**Options:**
| Flag | Default | Description |
|---|---|---|
| `--name <name>` | `dev-agent` | Sandbox name |
| `--model <model>` | `claude-opus-4.7` (Copilot) / `gpt-4.1` (Foundry) / `gpt-4o-mini` (GitHub Models) | Model deployment / catalogue name |
| `--policy <preset>` | `developer` | Policy preset: `minimal`, `developer`, `web`, `azure` |
| `--target <target>` | `docker` | Where to run the sandbox: `docker` (fast inner loop) or `local-k8s` (kind + Helm, mirrors AKS layout). |
| `--cluster-name <name>` | `kars-dev` | Kind cluster name (only used with `--target local-k8s`). |
| `--ephemeral` | `false` | (local-k8s only) destroy the kind cluster on exit. |
| `--github-token <pat>` | — | One-off GitHub Models override (does NOT save). Use for ephemeral runs that shouldn't overwrite your saved provider. To save Copilot/GitHub-Models as your default, run `kars dev` (or `kars credentials`) without this flag and pick at the prompt. |
| `--image <image>` | `kars-sandbox:dev` | Sandbox container image |
| `--build` | `false` | Build sandbox image locally from Dockerfile |
| `--build-base` | `false` | Rebuild the sandbox base image (heavy deps; only needed when upgrading OpenClaw/Python/Go) |
| `--base-image <image>` | `mcr.microsoft.com/azurelinux/base/core:3.0` | Azure Linux base image for building sandbox |
| `--mesh-provider <provider>` | `agt` | Mesh stack. Only `agt` is supported (the vendored Rust relay/registry were removed once their upstream AGT equivalents reached parity). Flag retained for existing scripts. |
| `--agt-repo <path>` | `$KARS_AGT_REPO` | Path to the agent-governance-toolkit checkout (used to build relay/registry images). |
| `--agt-sdk-tarball <path>` | — | Path to a locally-packed `@microsoft/agent-governance-sdk` `.tgz` to install in the sandbox image. Requires `--build`. |
| `--no-mesh` | — | (local-k8s only) skip mesh relay/registry deployment. Sandboxes lose KNOCK/E2E. Use only for pure controller smoke tests. |
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

**Subcommand: `kars dev down`**

Tears down a `--target local-k8s` dev environment (Kind cluster +
Headlamp port-forward). For Docker targets, `kars destroy <name>`
is the right command — `dev down` is local-k8s-specific.

| Flag | Default | Description |
|---|---|---|
| `--target <target>` | `local-k8s` | Only `local-k8s` is currently supported. |
| `--cluster-name <name>` | `kars-dev` | Kind cluster name to delete. |
| `--keep-cluster` | `false` | Stop the port-forward and uninstall Headlamp, but keep the kind cluster running. |

**Examples:**
```bash
# Start a local sandbox with default settings (prompts for credentials on first run)
kars dev

# Ephemeral GitHub Models run — does not change your saved Foundry creds
kars dev --github-token $GITHUB_PAT

# Named sandbox with Telegram channel
kars dev --name my-bot --channels telegram --telegram-token 123456:ABC-DEF

# Enable web-browsing skill with Brave Search
kars dev --skills browser --brave-api-key $BRAVE_KEY

# Build the image from scratch before starting
kars dev --build

# Spin up the full Kind-based mirror of AKS (controller, relay, registry, Headlamp)
kars dev --target local-k8s --build

# Tear it back down (deletes the Kind cluster)
kars dev down
```

**See also:** [docs/channels-plugins.md](channels-plugins.md)

---

### `kars add`

Adds a new sandboxed agent to an **existing** kars cluster. Creates a
`KarsSandbox` CR which the controller reconciles into an isolated namespace,
NetworkPolicy, and inference-router deployment. Supports all 8 wired runtime
kinds (openclaw, openai-agents, microsoft-agent-framework, langgraph,
anthropic, pydantic-ai, byo).

**Usage:**
```
kars add <name> [options]
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
| `--learn-egress` | `false` | Enable egress learn mode: observe all domains, then review with `kars egress` |
| `--runtime <kind>` | `openclaw` | Runtime: `openclaw`, `hermes`, `openai-agents`, `microsoft-agent-framework`, `langgraph`, `anthropic`, `pydantic-ai`, `byo` |
| `--byo-image <image>` | — | Container image for `--runtime byo` (must declare `org.kars.runtime.contract=v1`) |
| `--byo-contract-version <version>` | `v1` | BYO contract version |
| `--maf-language <lang>` | `python` | Microsoft Agent Framework language (`python`; `dotnet` is tracked in the [roadmap](roadmap.md)) |
| `--dry-run` | `false` | Print the KarsSandbox YAML without applying |

**Examples:**
```bash
# Add a second agent with a 100k token/day budget
kars add researcher --model gpt-4.1 --token-budget-daily 100000

# Add a Telegram-connected agent with enhanced isolation
kars add support-bot --channels telegram --telegram-token $TOKEN --isolation enhanced

# Add a BYO-runtime agent
kars add my-agent --runtime byo --byo-image myacr.azurecr.io/my-agent:latest

# Dry-run: inspect the KarsSandbox YAML before applying
kars add reviewer --dry-run
```

**See also:** [docs/api/crd-reference.md](api/crd-reference.md), [docs/runtimes.md](runtimes.md), [docs/channels-plugins.md](channels-plugins.md)

---

### `kars destroy`

Tears down sandbox(es) or the entire kars deployment. Without `--all`
it removes just the named sandbox (or all sandboxes if `<name>` is omitted).
With `--all` it deletes the entire resource group including AKS, ACR, and Key
Vault — use with care.

**Usage:**
```
kars destroy [name] [options]
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
kars destroy my-agent

# Destroy without prompting
kars destroy my-agent -y

# Destroy all sandboxes without touching infrastructure
kars destroy -y

# Destroy everything, including the resource group
kars destroy --all -y -g my-rg
```

---

### `kars push`

Builds and pushes kars images (controller, inference router, sandbox,
relay, registry) to ACR using the cached context from the last `kars up`
run. Use `--apply` to restart deployments so pods immediately pick up new
images. Use `--only sandbox` + `--apply` after modifying `entrypoint.sh`,
plugins, or skills.

**Usage:**
```
kars push [options]
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
kars push --apply

# Push only the sandbox image and restart pods (common after plugin changes)
kars push --only sandbox --apply

# Push only the controller image without restarting
kars push --only controller
```

**See also:** [docs/architecture.md](architecture.md)

---

### `kars convert`

Translates manifests between `KarsSandbox` and the upstream
`agents.x-k8s.io/v1alpha1 Sandbox` format (and the `overlay` variant). Hard-fails
on lossy translations by default; pass `--allow-lossy` to proceed with
warnings. The full field-mapping table is maintained alongside the translator source in `cli/src/commands/migrate/`
for the normative field mapping.

**Usage:**
```
kars convert [options]
```

**Options:**
| Flag | Default | Description |
|---|---|---|
| `-f, --file <path>` | *(required)* | Source manifest YAML |
| `--to <target>` | `karssandbox` | Target kind: `karssandbox`, `upstream-sandbox`, `overlay` |
| `--sandbox-ref <ns/name>` | — | For `--to overlay`: reference to an existing Sandbox CR |
| `--dry-run` | `false` | Validate + translate without emitting the converted manifest |
| `--allow-lossy` | `false` | Proceed even when translation drops fields with no analog |

**Examples:**
```bash
# Convert an upstream Sandbox YAML to a KarsSandbox
kars convert -f sandbox.yaml --to karssandbox > karssandbox.yaml

# Convert a KarsSandbox to upstream format, allowing lossy translation
kars convert -f karssandbox.yaml --to upstream-sandbox --allow-lossy

# Convert to overlay mode referencing an existing Sandbox CR
kars convert -f sandbox.yaml --to overlay --sandbox-ref=prod/web
```



---

### `kars migrate`

Switches a `KarsSandbox` between upstream-compatibility modes (`native`,
`overlay`, `translate`, `observe`) by wrapping a `kubectl patch` with
validation, before/after summary, and dry-run support. Also provides
`from-kagent` to translate a `kagent.dev/v1alpha2` Agent YAML into an
kars resource bundle.

**Usage:**
```
kars migrate <subcommand> [arguments] [options]
```

**Subcommands:**
| Subcommand | Description |
|---|---|
| `to-overlay <name>` | Flip to overlay mode; kars provides governance overlay; upstream CR owns the Pod. Requires `--upstream-ref`. |
| `from-overlay <name>` | Leave overlay mode; revert to native kars (controller resumes ownership). |
| `to-translate <name>` | Accept upstream SandboxClaim semantics on inbound (schema-only translation). |
| `to-observe <name>` | Mirror status of an upstream Sandbox CR without overlay. |
| `to-native <name>` | Reset to default native mode (kars owns the workload). |
| `from-kagent <input>` | Translate a `kagent.dev/v1alpha2` Agent YAML into an kars resource bundle. Use `-` to read from stdin. |

**Common options (all subcommands except `from-kagent`):**
| Flag | Default | Description |
|---|---|---|
| `-n, --namespace <ns>` | `kars-system` | Namespace where the KarsSandbox CR lives |
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
| `--isolation <mode>` | `enhanced` | KarsSandbox isolation mode: `standard`, `enhanced`, `confidential` |
| `--image <image>` | — | Override `spec.runtime.openclaw.image` |
| `--allow-lossy` | `false` | Waive the hard-fail on lossy translation |
| `--out-dir <dir>` | — | Write each emitted resource to `<dir>/<kind>-<name>.yaml` |
| `--force` | `false` | With `--out-dir`, overwrite existing files |
| `--format <fmt>` | `yaml` | Output format: `yaml` (multi-doc) or `json` (List) |
| `--dry-run` | `false` | Print summary + warnings; emit no resources |

**Examples:**
```bash
# Switch to overlay mode
kars migrate to-overlay my-agent --upstream-ref upstream-sandbox

# Revert to native mode (dry-run first)
kars migrate to-native my-agent --dry-run
kars migrate to-native my-agent

# Import from kagent YAML
kars migrate from-kagent agent.yaml --isolation enhanced --out-dir ./manifests

# Import from stdin
cat kagent-agent.yaml | kars migrate from-kagent -
```



---

## Operations

### `kars operator`

Live operator dashboard — a full-screen TUI that shows all sandboxes,
their policy state, inference stats, and logs from a single screen.
Supports both AKS (K8s pods) and local Docker (dev mode). Panels can
be filtered, grouped per sandbox, or rendered as a one-shot snapshot
for scripting and CI.

**Usage:**
```
kars operator [options]
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
kars operator

# Local dev mode, faster refresh
kars operator --dev --refresh 3

# Capture a one-shot snapshot for a status page
kars operator --snapshot

# Show specific panels grouped per sandbox
kars operator --panels status,logs --per-sandbox
```

**See also:** [docs/operator-tui.md](operator-tui.md)

---

### `kars connect`

Connects to a running sandbox — either as a shell (bash), as the OpenClaw
TUI, or via WebUI (port-forwarded to a local port). Defaults to the
OpenClaw TUI on Docker and to WebUI on AKS.

**Usage:**
```
kars connect <name> [options]
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
kars connect my-agent

# Open WebUI in browser (port-forwarded)
kars connect my-agent --web

# Drop into a bash shell for debugging
kars connect my-agent --shell

# Connect to the local Docker sandbox explicitly
kars connect my-agent --local
```

---

### `kars handoff`

Live-migrates an agent between local Docker and AKS (bidirectional handoff).
Uses the AgentMesh relay to transfer session state with no dropped requests.
Requires a shared registry (either `--global-registry` or a promoted AKS
registry reachable from both sides).

**Usage:**
```
kars handoff <name> [options]
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
kars handoff my-agent --to cloud

# Handoff from AKS back to local
kars handoff my-agent --to local

# Check handoff status
kars handoff my-agent --status

# Abort an in-progress handoff
kars handoff my-agent --abort
```

**See also:** [docs/architecture.md](architecture.md)

---

### `kars status`

Shows sandbox health, policy state, and inference configuration in a
human-readable summary. Includes the pod phase, readiness, active policy
profile, model configuration, and recent condition transitions.

**Usage:**
```
kars status <name>
```

**Arguments:**
| Name | Required | Description |
|---|---|---|
| `<name>` | Yes | Sandbox name |

**Examples:**
```bash
kars status my-agent
```

---

### `kars list`

Lists all kars sandboxes across both Docker (local) and AKS (cloud)
environments. Shows name, runtime, status, and model for each sandbox.

**Usage:**
```
kars list [options]
```

**Options:**
| Flag | Default | Description |
|---|---|---|
| `--aks-only` | `false` | Only show AKS sandboxes |
| `--docker-only` | `false` | Only show local Docker sandboxes |

**Examples:**
```bash
# List all sandboxes
kars list

# List AKS sandboxes only
kars list --aks-only
```

---

### `kars logs`

Streams agent and platform logs from a sandbox. Can tail logs from all
services or filter to a specific component: the inference router, OpenClaw
gateway, or the node host process.

**Usage:**
```
kars logs <name> [options]
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
kars logs my-agent

# Stream router logs in real time
kars logs my-agent --service router -f

# Show last 200 lines from the OpenClaw gateway
kars logs my-agent --service openclaw --tail 200
```

---

### `kars inspect`

Prints the controller's view of a single sandbox: the compiled
InferencePolicy digest, the attached ToolPolicies, EgressApproval
state, the Memory binding (if any), and recent `Reconciled` /
`AwaitingRouterEnforcement` conditions. Use this when `kars
status` says "Ready" but you want to confirm the router echoed the
exact policy revision you expect.

**Usage:**
```
kars inspect <sandbox> [options]
```

**Arguments:**
| Name | Required | Description |
|---|---|---|
| `<sandbox>` | Yes | Sandbox name (the `metadata.name` of the `KarsSandbox`). |

**Options:**
| Flag | Description |
|---|---|
| `-n, --namespace <ns>` | Override the default controller namespace (`kars-system`). |
| `--json` | Emit raw JSON instead of the formatted tree. |

**Examples:**
```bash
kars inspect my-agent
kars inspect my-agent --json | jq .policy.inferenceDigest
```

---

### `kars audit`

Tails the inference router's structured audit log for a sandbox.
Every governance decision (allow, deny, approval-required) is one
JSON row in the router's stdout; this command shells into the pod
and surfaces those rows with pretty formatting + filters.

**Usage:**
```
kars audit tail <sandbox> [options]
```

**Options:**
| Flag | Description |
|---|---|
| `-n, --namespace <ns>` | Namespace (default: `kars-<sandbox>`). |
| `--tail <N>` | Start from the last N rows (default: 200). |
| `-f, --follow` | Keep streaming new rows as they arrive. |
| `--decision <kind>` | Filter by decision: `allow`, `deny`, `approval`. |
| `--agent <id>` | Filter by exact `agent_id`. |
| `--tool <name>` | Filter by tool / capability name. |
| `--since <duration>` | Only rows newer than this (e.g. `15m`, `2h`). |
| `--json` | Emit each row as raw JSON instead of the pretty table. |

**Examples:**
```bash
# Pretty-print the last 200 governance decisions
kars audit tail my-agent

# Follow only denials for the search tool
kars audit tail my-agent --decision deny --tool web.search -f
```

---

### `kars attest`

Prints a deterministic attestation receipt for a sandbox: spec hash, SSA
field owners, referenced policy versions, and reconcile trace. Pass
`--baseline` to diff against a previously saved attestation; exit code
reflects drift (0 = match, 2 = drift, 3 = baseline missing).

Full signature and AGT receipt are tracked in the [roadmap](roadmap.md).

**Usage:**
```
kars attest <name> [options]
```

**Arguments:**
| Name | Required | Description |
|---|---|---|
| `<name>` | Yes | Sandbox name |

**Options:**
| Flag | Default | Description |
|---|---|---|
| `-n, --namespace <ns>` | `kars-system` | Namespace where the KarsSandbox CR lives |
| `--format <fmt>` | `human` | Output format: `human` or `json` |
| `--baseline <path>` | — | Path to a previously-emitted attestation JSON to diff against |

**Examples:**
```bash
# Print attestation receipt in human-readable form
kars attest my-agent

# Save attestation as a JSON baseline
kars attest my-agent --format json > attestation-2026-04-30.json

# Diff against saved baseline (exits 2 on drift)
kars attest my-agent --format json --baseline attestation-2026-04-30.json
echo $?  # 0=match 2=drift 3=missing baseline
```

**See also:** [docs/api/crd-reference.md](api/crd-reference.md)

---

## Configuration

### `kars credentials`

Manages kars credentials (inference provider, channel tokens,
third-party API keys). Invoking without a subcommand opens an interactive
guided prompt that lets you pick between **GitHub Copilot** *(default,
recommended)*, **Azure AI Foundry / Azure OpenAI**, and **GitHub Models**
for inference, save channel tokens (Telegram, Slack, Discord), and configure
third-party API keys (Brave, Tavily, Exa, Firecrawl, Perplexity, OpenAI).
Use `credentials set` / `list` / `remove` for scripting. Use
`credentials update` to patch a running AKS sandbox's K8s Secret without
restarting the pod (unless you want a restart).

The inference provider you pick is saved to `~/.kars/config.json`
(field `provider: "github-copilot" | "foundry" | "github-models"`); the
credential is saved alongside in `~/.kars/secrets.json` under the key
`azure-openai-key`. For Copilot the value is a GitHub OAuth token obtained
through an interactive **device-code flow** (the CLI prints a code and
opens `https://github.com/login/device` in your browser); the router
exchanges it for a short-lived Copilot JWT at runtime — you never see or
manage the JWT yourself. Switch providers any time by re-running this
command and picking another option.

**Usage:**
```
kars credentials [subcommand] [arguments] [options]
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
kars credentials

# Store a Telegram token
kars credentials set telegram-token 123456:ABC-DEF

# List stored secrets
kars credentials list

# Update a running sandbox's Telegram token and restart
kars credentials update my-agent --telegram-token 999999:NEW-TOKEN

# Update without restarting the pod
kars credentials update my-agent --brave-api-key $KEY --no-restart
```

**See also:** [docs/channels-plugins.md](channels-plugins.md)

---

### `kars config`

Inspects and edits the local CLI configuration at `~/.kars/config.json`. This is the file `kars dev` and `kars credentials` write to, holding your provider choice, endpoint, and default model. The command is a thin viewer + per-provider model picker — it doesn't touch secrets (use `kars credentials` for those) and it doesn't talk to your cluster (it's purely local).

**Usage:**
```
kars config <subcommand> [arguments]
```

**Subcommands:**
| Subcommand | Description |
|---|---|
| `show` | Print the effective local configuration (provider, endpoint, model). For `github-models`, also validates the saved model against the live catalog. |
| `model [model-id]` | Pick or set the local default inference model. Provider-aware: presents the curated Copilot catalog for `github-copilot`, the live tool-capable catalog for `github-models`, or accepts a free-form deployment name for `foundry`. Omit the argument for an interactive picker. |
| `reset` | Clear the local configuration file (does not touch saved secrets). |

**Examples:**
```bash
# Show what's currently saved
kars config show

# Switch the local default model interactively
kars config model

# Set it directly (Copilot / Models style id)
kars config model claude-opus-4.7

# Set it directly (Foundry deployment name)
kars config model gpt-4.1
```

**See also:** [`kars credentials`](#kars-credentials), [`kars model`](#kars-model) (per-sandbox).

---

### `kars model`

Manages the AI model for a sandbox. The `set` subcommand switches models
instantly without a pod restart (the change is applied via a hot ConfigMap
patch). Use `list` to discover available models from your Foundry project.

**Usage:**
```
kars model <subcommand> [arguments]
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
kars model set my-agent Phi-4

# Show the current model
kars model get my-agent

# List available Foundry models
kars model list my-agent
```

---

### `kars policy`

Manages sandbox network and security policies. Hot-reload capable: `allow`
and `deny` take effect without a pod restart. `learn` is an alias for
`kars egress <name> --learned`.

**Usage:**
```
kars policy <subcommand> [arguments] [options]
```

**Subcommands:**
| Subcommand | Description |
|---|---|
| `allow <name> <host>` | Add an allowed egress endpoint (hot-reload) |
| `get <name>` | Show the active policy for a sandbox |
| `deny <name> <host>` | Remove an allowed endpoint from a running sandbox |
| `learn <name>` | Alias for `kars egress <name> --learned` |
| `sign --kind <kind> --file <path> --registry <r> --repository <repo>` | Sign a canonical-form policy artifact (any of the 6 signed kinds), push it to OCI, cosign-sign the manifest, and optionally print the `bundleRef` snippet for the consuming CRD. This is the operator-authoring half of the trust loop documented in [security/crd-trust-model.md](security/crd-trust-model.md). |

**Arguments for `allow` / `deny`:**
| Name | Required | Description |
|---|---|---|
| `<name>` | Yes | Sandbox name |
| `<host>` | Yes | Hostname (e.g. `api.github.com`) |

**Options for `allow`:**
| Flag | Default | Description |
|---|---|---|
| `--port <port>` | `443` | Port |

**Options for `learn`:**
| Flag | Default | Description |
|---|---|---|
| `--apply` | `false` | Apply learned domains as the sandbox allowlist |
| `--clear` | `false` | Clear learned domains after export |

**Options for `sign`:**
| Flag | Default | Description |
|---|---|---|
| `--kind <kind>` | *(required)* | One of: `egress-allowlist`, `agt-profile`, `inference-policy`, `memory-binding`, `mcp-server-bundle`, `eval-corpus` |
| `--file <path>` | *(required)* | Path to the **canonical-form** bytes for the kind (see [docs/api/policy-canonical-format.md](api/policy-canonical-format.md)) |
| `--registry <host>` | *(required)* | OCI registry hostname (e.g. `myacr.azurecr.io`) |
| `--repository <repo>` | *(required)* | OCI repository path under the registry |
| `--tag <tag>` | `latest` | Tag to push under (informational — cosign signs by digest) |
| `--sign-mode <mode>` | *(auto-detect)* | `keyless` / `identity-token` / `keyed` |
| `--sign-key <ref>` | — | Cosign key reference (required for `--sign-mode keyed`, e.g. `azurekms://...`) |
| `--print-bundle-ref` | `false` | Emit a YAML `bundleRef` snippet ready to paste into the consuming CRD |
| `--json` | `false` | Emit a JSON envelope instead of human-readable output |

**Examples:**
```bash
# Allow GitHub API for a sandbox
kars policy allow my-agent api.github.com

# Allow on a non-standard port
kars policy allow my-agent internal.corp.com --port 8443

# Show current policy
kars policy get my-agent

# Remove a domain
kars policy deny my-agent api.github.com

# Sign a pre-built canonical InferencePolicy bundle and emit the bundleRef
kars policy sign \
  --kind inference-policy \
  --file ./inference.canonical.json \
  --registry myacr.azurecr.io \
  --repository policy/inference/my-agent \
  --print-bundle-ref
```

**See also:** [docs/egress-proxy.md](egress-proxy.md)

---

### `kars egress`

Full egress lifecycle management: learn mode (observe domains without
blocking), pending approvals, allowlist management, and signed OCI artifact
generation + cosign signing. With `--enforce`, all learned domains are
promoted to the allowlist and enforcement mode is activated. Signing is
on by default when combined with `--enforce` or `--approve`; the controller
will refuse to use unsigned artifacts in authoritative mode
(`SignerPolicyMissing`).

**Usage:**
```
kars egress [name] [options]
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
| `--emit-manifest <path>` | — | GitOps mode: write the KarsSandbox patch to `<path>` instead of running `kubectl patch` |
| `--force` | `false` | With `--emit-manifest`, overwrite an existing file |

**Examples:**
```bash
# Enable learn mode
kars egress my-agent --learn

# Review discovered domains
kars egress my-agent --learned

# Approve a domain (signs the updated allowlist automatically)
kars egress my-agent --approve api.github.com

# Graduate to enforcement mode (signs + patches)
kars egress my-agent --enforce

# GitOps mode: emit patch file instead of applying
kars egress my-agent --enforce --emit-manifest ./patches/egress-my-agent.yaml

# Sign with a KMS key
kars egress my-agent --approve api.github.com --sign-mode keyed --sign-key azurekms://myvault.vault.azure.net/keys/cosign
```

**See also:** [docs/egress-proxy.md](egress-proxy.md)

---

## Observability

### `kars trace`

Live eBPF trace using `kubectl-gadget` — surfaces network connections, file
access, and process executions in the sandbox container in real time. Requires
`kubectl-gadget` to be installed. Without filter flags all event types are shown.

**Usage:**
```
kars trace <name> [options]
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
kars trace my-agent

# Show only outbound network connections
kars trace my-agent --network

# Show DNS lookups in real time
kars trace my-agent --dns
```

---

### `kars eval`

Operator surface for the **`KarsEval`** CRD — a policy-conformance
runner driven by signed corpora and the in-tree
`conformance-runner` image. Replaces the legacy Foundry-Evals
wrapper.

| Subcommand | What it does |
|---|---|
| `list` | List all `KarsEval` resources across the controller namespace. |
| `show <name>` | Print spec, last-run summary, drift status, and conditions. |
| `run <name>` | Trigger an immediate run (sets the `kars.azure.com/run-now=true` annotation). |
| `diff <name>` | Diff the two most recent runs from `status.history[]`. |

All commands hit the apiserver via `kubectl`; no router admin token
required (operator can still see the CR even when the router is
unhealthy).

**Examples:**
```bash
# Tabular list across the controller namespace
kars eval list

# Schema + last run summary
kars eval show nightly-regression

# Trigger a one-shot run
kars eval run nightly-regression

# Diff the last two runs
kars eval diff nightly-regression
```

Authoring new corpora and signing them is covered separately:
- **[`docs/api/karseval.md`](api/karseval.md)** — operator workflows (run-now, schedule, drift, GC).
- **[`docs/api/crd-reference.md#karseval`](api/crd-reference.md#karseval--reproducible-evaluation-run)** — the CRD schema.

---

## Multi-Agent / Federation

> Three command families:
> - **Agent mobility** — `mesh`, `pair`: identity, authentication, federation pairings
> - **Interop** — `a2a`, `a2a-agent`: A2A ingress surfacing and per-agent trust anchors
> - **Governance** — `toolpolicy`, `inferencepolicy`, `mcp`: cluster-wide CRD policy management

### `kars mesh`

Manages AgentMesh identity and authentication for cross-environment agent
handoff and federation. Controls the Ed25519 mesh identity (stored
AES-256-GCM encrypted at `~/.kars/mesh-identity.json`), relay
registration enforcement, and cluster federation peer state.

**Usage:**
```
kars mesh <subcommand> [arguments] [options]
```

**Subcommands:**
| Subcommand | Description |
|---|---|
| `auth` | Authenticate with an AgentMesh registry via OAuth |
| `setup-trust` | Provision the tenant-wide `api://agentmesh` Entra app registration so sandboxes register as the AGT verified tier |
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

**Options for `setup-trust`:**
| Flag | Default | Description |
|---|---|---|
| `--display-name <name>` | `kars AgentMesh` | Display name for the Entra app registration |
| `--dry-run` | `false` | Print what would be created without making changes |

> Tenant-wide one-time operation. Requires Application Administrator (or higher) at tenant scope. Idempotent — re-running on a tenant where the app reg already exists is safe (just prints the existing IDs and exits).

**Options for `security`:**
| Flag | Default | Description |
|---|---|---|
| `-n, --namespace <ns>` | `agentmesh` | AgentMesh namespace |
| `--deployment <name>` | `relay` | Relay deployment name |

**Options for `peer`:**
| Flag | Default | Description |
|---|---|---|
| `-n, --namespace <ns>` | `kars-system` | Controller namespace |
| `--deployment <name>` | `kars-controller` | Controller deployment name |

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
kars mesh auth

# Provision the tenant-wide api://agentmesh app reg (one-time, per-tenant)
kars mesh setup-trust

# Check current mesh identity
kars mesh status

# List pairings on the cluster
kars mesh list

# Enable strict registration on the relay
kars mesh security strict

# Enable controller federation
kars mesh peer enable

# Promote cluster registry to public endpoint
kars mesh promote --port-forward

# Demote back to cluster-local
kars mesh demote

# Delete a specific pairing
kars mesh unpair --name my-peer
```



---

### `kars pair`

Manages federation pairings for external agent cloud offload. Generate a
one-time token that an external agent (e.g., running `kars dev` on
another machine) can use to register as a federation peer and offload
sandboxes into this cluster.

**Usage:**
```
kars pair <subcommand> [arguments] [options]
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
kars pair generate --expires 30d

# Generate a token with a tighter token budget
kars pair generate --token-budget 100000 --slots 2

# List pairings
kars pair list

# Inspect a pairing
kars pair inspect my-peer

# Revoke a pairing
kars pair revoke my-peer
```



---

### `kars a2a`

A2A (Agent-to-Agent) ingress surfacing commands. `list-exposed` shows every
sandbox currently exposed for inbound A2A traffic so operators can verify the
blast radius at a glance. Today no sandboxes opt into A2A ingress by default,
so `list-exposed` typically returns an empty table; it populates as soon as
sandboxes are configured to accept inbound A2A traffic.

**Usage:**
```
kars a2a <subcommand> [options]
```

**Subcommands:**
| Subcommand | Description |
|---|---|
| `list-exposed` | List sandboxes currently exposed for inbound A2A traffic (allowed callers, expiry, advertised skills, rate limits) |
| `schema` | Print the AgentCard JSON shape this cluster publishes per the A2A spec |

**Options for `list-exposed`:**
| Flag | Default | Description |
|---|---|---|
| `-n, --namespace <ns>` | *(all sandbox namespaces)* | Restrict to a single namespace |
| `-o, --output <fmt>` | `table` | Output format: `table`, `json`, `yaml` |

**Examples:**
```bash
# List exposed sandboxes
kars a2a list-exposed

# Machine-readable output
kars a2a list-exposed --output json

# Show the AgentCard schema this cluster publishes
kars a2a schema
```

**See also:** [`kars a2a-agent`](#kars-a2a-agent) — manage A2AAgent CRs (signing-key trust anchors).

---

### `kars a2a-agent`

Manages **A2AAgent** custom resources — the trust anchors that authorise
inbound A2A traffic. Each A2AAgent CR pins one or more signing keys and
optionally points at a `ToolPolicy` for per-call authorisation.

**Usage:**
```
kars a2a-agent <subcommand> [options]
```

**Subcommands:**
| Subcommand | Description |
|---|---|
| `apply <name>` | Create or update an A2AAgent |
| `get <name>` | Show an A2AAgent by name |
| `list` | List A2AAgents in a namespace |
| `delete <name>` | Delete an A2AAgent |

**Options for `apply`:**
| Flag | Default | Description |
|---|---|---|
| `-n, --namespace <ns>` | `default` | Namespace |
| `--from-file <path>` | — | Read full spec from a YAML/JSON file (overrides flags below) |
| `--endpoint-url <url>` | — | Agent endpoint URL |
| `--production-mode` | `false` | Reject unauthenticated traffic; require `https://` |
| `--signing-key <kid:alg:b64u>` | — | Signing key entry `kid:alg:publicKeyB64u[:notAfter]` (repeatable, ≥1 required) |
| `--capability <s>` | — | Advertised capability (repeatable) |
| `--description <s>` | — | AgentCard description |
| `--display-name <s>` | — | Human-readable display name |
| `--policy-toolpolicy <name>` | — | ToolPolicy CR name to join at request time |
| `--require-signed` | `false` | Reject unsigned inbound A2A requests |
| `--min-signatures <n>` | — | Minimum independent valid signatures required |
| `--max-skew-seconds <n>` | — | Maximum tolerated clock skew (seconds) |

**Examples:**
```bash
# Register an external partner agent with one signing key
kars a2a-agent apply partner-bot \
  --endpoint-url https://partner.example.com/a2a \
  --signing-key key1:Ed25519:MCowBQYDK2VwAyEAxxx... \
  --require-signed --production-mode

# Apply from a YAML spec
kars a2a-agent apply partner-bot --from-file partner.yaml

# List A2AAgents in a namespace
kars a2a-agent list -n acme
```

**See also:** [docs/api/crd-reference.md](api/crd-reference.md#a2aagent), [`kars toolpolicy`](#kars-toolpolicy)

---

### `kars toolpolicy`

Manages **ToolPolicy** custom resources — per-tool gating, rate limits, and
AP2 commerce caps applied to every dispatched tool call. Aliased as `tp`.

**Usage:**
```
kars toolpolicy <subcommand> [options]
kars tp <subcommand> [options]
```

**Subcommands:**
| Subcommand | Description |
|---|---|
| `apply <name>` | Create or update a ToolPolicy |
| `get <name>` | Show a ToolPolicy by name |
| `list` | List ToolPolicies in a namespace |
| `delete <name>` | Delete a ToolPolicy by name |

**Options for `apply`:**
| Flag | Default | Description |
|---|---|---|
| `-n, --namespace <ns>` | `default` | Namespace |
| `--from-file <path>` | — | Read spec from a YAML/JSON file |
| `--tool <name>` | — | Tool name selector (use `*` for all tools) |
| `--mcp-server <name>` | — | Restrict to a specific MCP server |
| `--sandbox-label <kv>` | — | Sandbox match label `key=value` (repeatable) |
| `--rps <n>` | — | Rate limit: requests per second |
| `--burst <n>` | — | Rate limit: token-bucket burst |
| `--window <s>` | — | Rate limit window, e.g. `1m`, `24h` |
| `--daily-cap <s>` | — | AP2 daily cap, e.g. `'USD 100.00'` |
| `--monthly-cap <s>` | — | AP2 monthly cap |
| `--per-transfer-cap <s>` | — | AP2 per-transfer cap |
| `--counterparty <s>` | — | AP2 counterparty allow-list entry (repeatable) |
| `--approval-mode <mode>` | — | Approval mode: `never`, `always`, `aboveThreshold` |
| `--approval-threshold <s>` | — | Approval threshold value |
| `--approval-channel <s>` | — | Approval channel reference |
| `--display-name <s>` | — | Human-readable display name |

**Examples:**
```bash
# Rate-limit all tools to 10 rps with 20-burst
kars tp apply rate-limit-default --tool '*' --rps 10 --burst 20

# AP2 cap on a payment tool
kars tp apply payments-cap --tool send-payment \
  --daily-cap 'USD 500.00' --per-transfer-cap 'USD 100.00' \
  --approval-mode aboveThreshold --approval-threshold 'USD 50.00'

# Apply from YAML
kars tp apply complex-policy --from-file policy.yaml
```

**See also:** [docs/api/crd-reference.md](api/crd-reference.md#toolpolicy), [`kars inferencepolicy`](#kars-inferencepolicy)

---

### `kars inferencepolicy`

Manages **InferencePolicy** custom resources — token budgets, model
preference, and Content Safety severity floor applied to every inference
call. Aliased as `ip`.

**Usage:**
```
kars inferencepolicy <subcommand> [options]
kars ip <subcommand> [options]
```

**Subcommands:**
| Subcommand | Description |
|---|---|
| `apply <name>` | Create or update an InferencePolicy |
| `get <name>` | Show an InferencePolicy by name |
| `list` | List InferencePolicies in a namespace |
| `delete <name>` | Delete an InferencePolicy |

**Options for `apply`:**
| Flag | Default | Description |
|---|---|---|
| `-n, --namespace <ns>` | `default` | Namespace |
| `--from-file <path>` | — | Read spec from a YAML/JSON file |
| `--sandbox <name>` | — | Restrict to a specific sandbox |
| `--action <kind>` | — | Inference action: `chat`, `responses`, `image`, `embeddings`, or `*` |
| `--sandbox-label <kv>` | — | Sandbox match label `key=value` (repeatable) |
| `--token-budget <n>` | — | Daily token cap (input + output) |
| `--monthly-tokens <n>` | — | Monthly token cap |
| `--per-request-tokens <n>` | — | Per-request token cap |
| `--model <deployment>` | — | Primary model deployment name |
| `--provider <name>` | `azure-openai` | Provider tag for `--model` |
| `--fallback <provider:deployment>` | — | Fallback route (repeatable) |
| `--content-safety-severity <sev>` | — | Severity floor for all CS categories: `Safe`, `Low`, `Medium`, `High` |
| `--require-prompt-shields` | `false` | Require Prompt Shields annotations from upstream |
| `--display-name <s>` | — | Human-readable display name |

**Examples:**
```bash
# Daily cap for one sandbox with a fallback model
kars ip apply daily-budget \
  --sandbox my-bot \
  --token-budget 100000 \
  --model gpt-4.1 \
  --fallback azure-openai:gpt-4o-mini

# Cluster-wide Content Safety floor
kars ip apply cs-floor --action '*' --content-safety-severity Medium

# Apply from YAML
kars ip apply complex --from-file policy.yaml
```

**See also:** [docs/api/crd-reference.md](api/crd-reference.md#inferencepolicy), [`kars toolpolicy`](#kars-toolpolicy)

---

### `kars mcp`

Manages **McpServer** custom resources — registers external MCP (Model Context
Protocol) servers that sandboxes are allowed to reach. The router proxies
`/v1/mcp/*` requests through OAuth 2.1 + JWS verification when
`productionMode=true`.

**Usage:**
```
kars mcp <subcommand> [options]
```

**Subcommands:**
| Subcommand | Description |
|---|---|
| `apply <name>` | Create or update an McpServer |
| `get <name>` | Show an McpServer by name |
| `list` | List McpServers in a namespace |
| `delete <name>` | Delete an McpServer |

**Options for `apply`:**
| Flag | Default | Description |
|---|---|---|
| `-n, --namespace <ns>` | `default` | Namespace |
| `--from-file <path>` | — | Read spec from a YAML/JSON file |
| `--url <url>` | — | Server endpoint URL (`https://` required in production mode) |
| `--production-mode` | `false` | Require OAuth 2.1 + HTTPS |
| `--oauth-issuer <url>` | — | OAuth issuer URL |
| `--oauth-audience <s>` | — | OAuth audience claim |
| `--oauth-resource <s>` | — | OAuth resource indicator |
| `--scope <s>` | — | OAuth scope (repeatable) |
| `--allowed-tool <s>` | — | Allowed tool name (repeatable; use `*` for any) |
| `--allowed-sandbox-label <kv>` | — | Sandbox match label `key=value` (repeatable) |
| `--display-name <s>` | — | Human-readable display name |

**Examples:**
```bash
# Register a public MCP server (dev mode)
kars mcp apply github-mcp --url https://mcp.github.com --allowed-tool '*'

# Production mode with OAuth + tool allowlist
kars mcp apply prod-mcp \
  --url https://mcp.example.com \
  --production-mode \
  --oauth-issuer https://login.example.com \
  --oauth-audience mcp-api \
  --scope mcp.read --scope mcp.write \
  --allowed-tool search --allowed-tool fetch

# Apply from YAML
kars mcp apply complex --from-file mcp.yaml
```

**See also:** [docs/api/crd-reference.md](api/crd-reference.md#mcpserver), [`kars toolpolicy`](#kars-toolpolicy)

---

### `kars memory`

Operator surface for the **`KarsMemory`** CRD — the binding between a
sandbox and a Foundry Memory Store (scope, retention floor, delete-on-
sandbox-delete sweep). Mirrors `kubectl get/apply/delete` patterns.

| Subcommand | What it does |
|---|---|
| `apply <name>` | Create or update a KarsMemory binding (from flags or `--from-file`). |
| `get <name>` | Show a KarsMemory by name (`-o pretty|yaml|json`). |
| `list` | List KarsMemory bindings in a namespace. |
| `delete <name>` | Delete a binding (`--no-prompt` to skip confirmation). |

**Common flags on `apply`:**
| Flag | Description |
|---|---|
| `-n, --namespace <ns>` | Namespace (use `kars-<sandbox>`). |
| `--from-file <path>` | Read full spec from a YAML/JSON file (mutually exclusive with the flags below). |
| `--sandbox <name>` | Sandbox to bind (`spec.sandboxRef.name`). |
| `--store <name>` | Foundry Memory Store name (DNS-label). |
| `--scope <key>` | Scope key under which this sandbox reads/writes (e.g. `agent:my-agent`). |
| `--retention-days <n>` | Retention floor for the `delete_scope` sweep (must be > 0). |
| `--display-name <s>` | Human-readable display label. |
| `--no-delete-on-sandbox-delete` | Keep store contents when the sandbox is deleted (default: cleanup on delete). |

**Examples:**
```bash
# Bind a sandbox to a Memory Store, scoped per-agent, 30-day floor
kars memory apply my-agent-mem \
  -n kars-my-agent \
  --sandbox my-agent \
  --store prod-shared-memory \
  --scope agent:my-agent \
  --retention-days 30

# List bindings in a namespace
kars memory list -n kars-my-agent

# Delete (defaults to cleaning up scope contents)
kars memory delete my-agent-mem -n kars-my-agent
```

**See also:** [docs/api/crd-reference.md#karsmemory](api/crd-reference.md#karsmemory--foundry-memory-binding)
