# Contributing to AzureClaw

This project welcomes contributions and suggestions.

## Quick Start

```bash
git clone https://github.com/<your-user>/azureclaw.git
cd azureclaw
make build    # Rust (controller + router) + TypeScript CLI
make test     # 205 unit tests (Rust) + 207 CLI tests (vitest)
make lint     # clippy + oxlint
```

## Project Structure

| Directory | Language | What It Is |
|-----------|----------|------------|
| `controller/` | Rust (kube-rs) | K8s operator — reconciles ClawSandbox CRDs into sandboxes |
| `inference-router/` | Rust (axum) | Per-sandbox router — auth, safety, budgets, 18 Foundry APIs, native AGT governance |
| `cli/` | TypeScript | 18 CLI commands + OpenClaw plugin + 10 Foundry skills |
| `runtimes/openclaw/skills/` | Markdown | 10 SKILL.md files teaching the OpenClaw agent to use AzureClaw + Foundry services |
| `cli/policies/` | YAML | AGT policy profiles (shell-safety, approval, rate-limit) |
| `deploy/bicep/` | Bicep | Azure infrastructure (AKS, ACR, KV, AOAI, Monitor) |
| `deploy/helm/` | YAML | Helm chart (CRD, controller, RBAC, seccomp, NetworkPolicy) |
| `deploy/seccomp/` | JSON | seccomp profile (`azureclaw-strict.json`) |
| `sandbox-images/` | Dockerfile | Azure Linux 3 sandbox image + entrypoint |
| `vendor/` | Rust + TS | Patched forks of AgentMesh relay, registry, SDK (8 bug fixes) |
| `tests/e2e/` | Bash | E2E tests: Kind-based + live AKS infra tests |

## Development

### TypeScript CLI (Node.js 22+)

```bash
cd cli
npm ci && npm run build && npm link   # compile + link global `azureclaw` command
npm test                                    # vitest
npm run lint                                # oxlint
npm run typecheck                           # tsc --noEmit
```

### Rust (edition 2024, MSRV 1.88)

```bash
cargo build --release     # builds controller + inference-router
cargo test --all          # 205 unit tests (74 controller + 105 router + 26 integration)
cargo clippy --all-targets -- -D warnings
cargo fmt --all           # format
```

### Docker Sandbox Image

```bash
azureclaw dev --build                      # build + run locally via Docker
```

### Push to ACR

```bash
azureclaw push --only sandbox --apply      # build sandbox image, push to ACR, restart pods
azureclaw push                             # build + push all images (controller, router, sandbox, relay, registry)
```

### Docker Images (Makefile)

```bash
make images               # builds controller + inference-router images
make push                 # pushes to configured ACR
```

### Local E2E

```bash
azureclaw credentials     # configure Azure OpenAI (or just run `dev`/`up` — prompts inline)
azureclaw dev             # start local sandbox
azureclaw connect dev-agent
azureclaw destroy dev-agent
```

## Adding Channels and Plugins

AzureClaw uses a consistent pattern for channels (Telegram, Slack, Discord, WhatsApp) and third-party plugins (Brave, Tavily, Exa, Firecrawl, Perplexity, OpenAI):

```
CLI flag → Docker env var → entrypoint auto-config → plugins.allow + plugins.entries
```

### Adding a New Channel Flag

1. **CLI `add.ts`** — add a flag (e.g., `--myapp-token <token>`) and map it to an env var (e.g., `MYAPP_BOT_TOKEN`) in the deployment spec
2. **CLI `credentials.ts`** — add the flag to the `update` subcommand's `flagToEnv` map
3. **`entrypoint.sh`** — add a block that reads the env var and builds the `CHANNELS_CONFIG`, `PLUGINS_LIST`, and `PLUGINS_ENTRIES` strings (follow the Telegram/Slack pattern)
4. **Controller `reconciler.rs`** — the controller mounts `<name>-credentials` secret via `envFrom` (optional: true), so credential env vars are injected automatically

### Adding a New Plugin API Key Flag

1. **CLI `add.ts`** — add a flag (e.g., `--myplugin-api-key <key>`) and map it to an env var (e.g., `MYPLUGIN_API_KEY`)
2. **CLI `credentials.ts`** — add the flag to the `update` subcommand's `flagToEnv` map
3. **`entrypoint.sh`** — add a `"myplugin:MYPLUGIN_API_KEY"` entry to the `for plugin_pair in ...` loop. OpenClaw reads the env var directly for auth; the entrypoint just registers the plugin in `plugins.allow` + `plugins.entries`

### Credentials Secret Convention

Credentials are stored in a K8s secret named `<sandbox-name>-credentials` in the sandbox namespace (`azureclaw-<name>`). The controller mounts it via `envFrom` with `optional: true` so pods start even if no credentials secret exists. Use `azureclaw credentials update <name> --telegram-token <token>` to create/update the secret.

## Pull Requests

1. Clear description of the change
2. Tests for new functionality
3. `make test && make lint` passes
4. Documentation updated if applicable

## Code Style Policies

### File size (Rust)

Source files **should stay under 1500 LOC**. Files over that threshold
tend to hide bugs, slow down reviews, and make refactoring risky. Today
the following files are over budget and are candidates for incremental
splitting (see plan.md Q1):

| File | LOC | Suggested split |
|---|---|---|
| `inference-router/src/routes.rs` | ~5000 | `routes/{inference,governance,mesh,egress,admin}.rs` |
| `inference-router/src/handoff.rs` | ~2625 | State machine + crypto are separable |
| `controller/src/reconciler.rs` | ~2380 | Per-resource builders are separable |
| `controller/src/mesh_peer.rs` | ~1970 | Discovery vs. connection state |
| `inference-router/src/governance.rs` | ~1250 | Near budget — watch before extending |
| `inference-router/src/spawn.rs` | ~1160 | Near budget — watch before extending |

**Rule of thumb:** a PR that pushes any file over 1500 LOC should either
(a) split the file in the same PR, or (b) include a follow-up tracking
item in plan.md. Reviewers are expected to flag this.

This is a soft rule — edge cases exist (e.g., generated code, large
lookup tables). Use judgement; call it out in the PR description when
you exceed it intentionally.

### File size (TypeScript)

`cli/src/plugin.ts` is a known outlier (>4000 LOC) tracked separately —
same rule applies to new TS files.

## Code of Conduct

[Microsoft Open Source Code of Conduct](https://opensource.microsoft.com/codeofconduct/) · [FAQ](https://opensource.microsoft.com/codeofconduct/faq/) · [opencode@microsoft.com](mailto:opencode@microsoft.com)

## CLA

Most contributions require a [Contributor License Agreement](https://cla.opensource.microsoft.com). A bot will guide you when you submit a PR.

## Security

Report vulnerabilities via [SECURITY.md](SECURITY.md), not GitHub Issues.
