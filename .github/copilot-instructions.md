# AzureClaw — Copilot Instructions

## What is AzureClaw?

A secure AI agent runtime on Azure AKS. OpenClaw agents run in isolated K8s sandbox pods with E2E encrypted inter-agent communication (Signal Protocol via AgentMesh). Each agent gets its own namespace, NetworkPolicy, seccomp profile, and inference router.

## Architecture

Four components, two languages:

| Component | Language | Package Name | Role |
|-----------|----------|-------------|------|
| **Controller** | Rust (kube-rs) | `azureclaw-controller` | K8s operator — reconciles `ClawSandbox` CRDs into isolated sandboxes (namespace, deployment, service, NetworkPolicy, ConfigMap) |
| **Inference Router** | Rust (axum) | `azureclaw-inference-router` | Per-sandbox proxy — the **only** network path for agents. Handles IMDS auth, Content Safety, token budgets, 18 Foundry API groups, AGT governance, sub-agent spawn |
| **CLI** | TypeScript | `@azure/azureclaw` | 18 CLI commands (`azureclaw up/add/dev/connect/handoff/mesh/...`) + OpenClaw plugin + 10 Foundry skills |
| **Policy Engine** | YAML profiles | — | AGT governance policy profiles (allow/deny/approval/rate-limit) |

**External dependencies:** [OpenClaw](https://openclaw.ai) (agent framework), [Azure AI Foundry](https://learn.microsoft.com/azure/ai-studio/) (managed AI services), [AGT](https://github.com/microsoft/agent-governance-toolkit) (governance layer).

### Sandbox pod structure

Each sandbox pod has 2 containers + 1 init container:
- **init: egress-guard** — iptables rules restricting UID 1000 to localhost + DNS only
- **openclaw** (UID 1000) — runs the OpenClaw agent with the AzureClaw plugin
- **inference-router** (UID 1001) — Rust router on port 8443, all agent traffic flows through it

Agents never see API keys. The router authenticates via IMDS/Workload Identity.

### Three AgentMesh repos (important context)

- `amitayks/agentmesh` — actual protocol implementation (relay, registry, SDK), published as `@agentmesh/sdk`
- `microsoft/agent-governance-toolkit` (AGT) — governance layer depending on `@agentmesh/sdk`
- Our `vendor/` directory contains patched forks of relay, registry, and SDK (8 bug fixes documented in `vendor/*/README.md`)
- The sandbox Docker build installs `@agentmesh/sdk` via npm, then **overlays** with our vendored dist files

## Build, Test, and Lint

### Rust (edition 2024, MSRV 1.88)

```bash
make build                # builds controller + router + CLI
cargo build --release     # Rust only (both crates)
cargo test --all          # all Rust tests (74 controller + 105 router + 26 integration)

# Single crate:
cargo build --release --package azureclaw-controller
cargo build --release --package azureclaw-inference-router

# Single test:
cargo test --package azureclaw-controller -- test_name
cargo test --package azureclaw-inference-router -- test_name

# Lint:
cargo clippy --all-targets -- -D warnings
cargo fmt --all           # format
```

### TypeScript CLI (Node.js 22+)

```bash
cd cli
npm ci && npm run build    # compile + copy policy profiles to dist/
npm test                        # vitest
npm run lint                    # oxlint
npm run typecheck               # tsc --noEmit
```

### Docker images

```bash
make images               # builds controller + router images
make push                 # pushes to ACR

# Sandbox image (must use repo root as context):
docker build -f sandbox-images/openclaw/Dockerfile .
```

### E2E tests

```bash
make test-e2e             # requires Docker + Kind
```

## Key Conventions

### Image tags: always use `:latest`

Never hardcode version tags. The controller defaults to `:latest` (reconciler.rs ~line 945). Previous version tag drift (v11–v25) caused hard-to-debug issues. Don't manually set `SANDBOX_IMAGE`/`INFERENCE_ROUTER_IMAGE` env vars or CRD `openclaw.image` fields — let the controller use defaults.

### Plugin singleton guard

OpenClaw loads the plugin twice (tool registry + agent session). The singleton guard `process.env.__AGT_INITIALIZED = '1'` ensures only the first load creates the AGT client. Don't remove this.

### Sub-agent container lifecycle

`entrypoint.sh` starts the OpenClaw gateway (port 18789) in the background, then starts a persistent `openclaw agent --local` session. This background session loads the plugin → connects to AGT relay → receives/replies to E2E messages. Without it, the sub-agent can't receive relay messages.

### Vendored SDK patches

`vendor/` contains 8 bug fixes against upstream `@agentmesh/sdk` v0.1.2. The sandbox Dockerfile installs npm deps normally, then overlays vendored SDK files:
```dockerfile
COPY vendor/agentmesh-sdk/dist/ .../node_modules/@agentmesh/sdk/dist/
```
Vendored relay/registry need Rust 1.94+ (upstream's 1.83 is too old). All patches are documented in `vendor/*/README.md`.

### Rust workspace

Two crates in one workspace. Shared dependencies are declared in the root `Cargo.toml` under `[workspace.dependencies]` and consumed with `.workspace = true` in each crate's `Cargo.toml`.

### Azure identity

No Azure SDK dependency — authentication is via REST (IMDS token exchange with Workload Identity). See `inference-router/src/auth.rs`.

## Foundry Memory Store Auth

Memory Store operations that internally call models (update_memories, search_memories with items) fail with 403 "Authentication failed" even when client-side RBAC is correct. CRUD and empty searches work fine.

**Root cause:** Memory Store authenticates internally as the **project's** managed identity, not the AI Services account MI.

**Fix:**
1. Enable system-assigned MI on the **project** (Portal: Project → Resource Management → Identity)
2. Assign **Azure AI User** role to the **project MI** on the **resource group** (not the AI Services resource)

Two identities matter:
- **User/Workload Identity** calling the API → needs Azure AI User on the AI Services resource
- **Project MI** (internal model calls) → needs Azure AI User on the **resource group**

Token audience must be `https://ai.azure.com/` (not `cognitiveservices.azure.com`).

## Channel / Plugin Pattern

Channels and third-party plugins follow the same flow:

```
CLI flag → Docker env var → entrypoint.sh auto-config → plugins.allow + plugins.entries
```

- **Channels** (Telegram, Slack, Discord, WhatsApp): CLI flag sets env var (e.g., `TELEGRAM_BOT_TOKEN`). `entrypoint.sh` reads it and builds the `channels.*` block + registers in `plugins.allow` + `plugins.entries`.
- **Plugins** (Brave, Tavily, Exa, Firecrawl, Perplexity, OpenAI): CLI flag sets env var (e.g., `BRAVE_API_KEY`). `entrypoint.sh` registers the plugin in `plugins.allow` + `plugins.entries`. OpenClaw reads the env var directly for auth.

### Credentials Secret Convention

Credentials are stored in a K8s secret named `<name>-credentials` in namespace `azureclaw-<name>`. The controller mounts it via `envFrom` with `optional: true` — pods start even without the secret. Update with `azureclaw credentials update <name> --telegram-token <token>`.

### Foundry Bing Web Search

Bing Grounding is auto-discovered via the Foundry `/connections` API. The router uses the full resource ID (not just the connection name) when calling the Bing search tool. No manual config is needed when a Bing Grounding resource is connected to the Foundry project.

### Deploying Plugin Changes

After modifying the sandbox image (entrypoint, plugins, skills):
```bash
azureclaw push --only sandbox --apply   # build, push to ACR, restart pods
```

### Node.js 22 Proxy Issue

Node.js 22's built-in `fetch()` ignores `HTTPS_PROXY`. The sandbox uses `proxy-bootstrap.js` to explicitly configure an HTTPS agent when `HTTPS_PROXY` is set. If you see network timeouts in environments with a proxy, check that `proxy-bootstrap.js` is loaded via `--require` or `NODE_OPTIONS`.

## Common Issues

| Symptom | Cause | Fix |
|---------|-------|-----|
| "Cannot find module '@agentmesh/sdk'" | npm install failed silently in Docker | Check vendored SDK overlay in Dockerfile |
| "SignatureVerificationFailed" in relay | Timestamp format mismatch (Z vs +00:00) | Apply `vendor/agentmesh-relay` patches |
| "wrong secret key for the given ciphertext" | Ratchet key mismatch | Check `vendor/agentmesh-sdk` session.ts |
| Duplicate messages in inbox | Plugin loaded twice without singleton | Check `__AGT_INITIALIZED` env guard |
| Sub-agent doesn't receive relay messages | No background agent session | Check `entrypoint.sh` relay listener |
| Old image served despite `:latest` push | AKS node cache | Use `imagePullPolicy: Always` or restart pods |
| "Invalid character" in base64 | `x25519:`/`ed25519:` key prefix | Apply vendored base64Decode fix |
| Node.js 22 fetch ignores HTTPS_PROXY | Built-in fetch doesn't use proxy env | Load `proxy-bootstrap.js` via `NODE_OPTIONS` |
