# Contributing to AzureClaw

This project welcomes contributions and suggestions.

## External Contributions — Scope & Goals

AzureClaw is a **community-supported, best-effort** project. We are grateful for contributions from the Azure / AKS operator community, OpenClaw / OpenAI Agents SDK adopters, security researchers, and MCP/plugin vendors.

### Audience

We expect external contributions from:

- **Azure / AKS operators** running AzureClaw in production, troubleshooting edge cases or adding new observability/governance features
- **OpenClaw / OpenAI Agents SDK / Microsoft Agent Framework users** adopting AzureClaw for AI governance, security, or sandboxing
- **Security researchers** reviewing the runtime, auditing isolation guarantees, or reporting vulnerabilities
- **MCP server vendors** and plugin authors (Brave, Tavily, Firecrawl, Perplexity, OpenAI, custom web-search providers) adding new providers or channels (Telegram, Slack, Discord, WhatsApp)

### In Scope — We Welcome PRs For

- **Bug fixes** against documented behavior (regressions, incorrect error handling, unmet API contracts)
- **New MCP servers** that conform to the `McpServer` CRD and do not relax sandbox isolation
- **New channels** (Telegram/Slack/Discord/WhatsApp pattern) or **web-search plugins** (Brave/Tavily/Exa/Firecrawl/Perplexity/OpenAI pattern)
- **New Tier-2 BYO runtime adapters** that implement the multi-runtime architecture per `docs/architecture.md` (or `docs/runtime-contract.md` if present)
- **Egress allowlist contributions** for the signed-OCI workflow (per S12 / `docs/internal/security-audits/`)
- **Documentation improvements**, especially for use-case blueprints, troubleshooting guides, and architecture clarifications
- **Test coverage improvements** (chaos tests, conformance tests, unit tests) that increase confidence in isolation or governance
- **Performance fixes** that do not relax security invariants (latency, throughput, resource utilization)

### Out of Scope — We Will Not Merge

- **New cross-cluster transports** — Microsoft AGT AgentMesh is the only sanctioned transport. Changes to inter-cluster communication require an ADR and CELA review; AzureClaw no longer carries a vendored AgentMesh transport fork.
- **Changes to sandbox isolation** — modifications to UID/GID, Landlock rules, seccomp profiles, or NetworkPolicy that weaken pod isolation or increase privilege
- **Inference router / governance bypass** — any change that routes agent traffic outside the router, skips the governance chain, or adds unauthenticated API endpoints
- **Direct cloud-side telemetry** — telemetry must remain opt-in via OpenTelemetry (see Data Collection notice in README). Direct Azure Monitor / Application Insights SDKs are not accepted.
- **New top-level CRDs** without a published ADR in `docs/adr/` and an RFC issue

### Triage Cadence & Response Time

- The maintainer team (`@AzureClawTeam`) reviews open PRs **at least weekly**
- PRs without CLA signature, missing security-audit documentation, or failing CI gates will not be reviewed until those issues are resolved
- Support is **community-driven, best-effort** — no SLAs or guaranteed response times. For critical security issues, follow the SECURITY.md vulnerability reporting process instead of opening a PR.

### Architecture-Level Changes

PRs that propose new CRDs, new runtimes, transport changes, or new direct dependencies require:

1. An **Architecture Decision Record (ADR)** in `docs/adr/` (see existing ADRs for format)
2. A public **RFC issue** discussing the motivation, design trade-offs, and impact on existing users
3. **Security audit documentation** in `docs/internal/security-audits/` with `Signed-off-by:` from at least one maintainer

The maintainer team will not review implementation PRs for architecture changes without prior ADR + RFC. Implementation PRs must follow the slice-train pattern (breaking large changes into reviewable chunks). See existing examples in `docs/internal/security-audits/2026-04-*` for the audit format.

### Security Disclosures

**Do not open public issues or PRs for vulnerabilities.** See [SECURITY.md](SECURITY.md) for the vulnerability reporting process. All security fixes will be fast-tracked and credited to the reporter.

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
azureclaw push                             # build + push all AzureClaw images (controller, router, sandbox)
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

## AgentMesh provider changes

AzureClaw runs exclusively on Microsoft AGT AgentMesh (`@microsoft/agent-governance-sdk` plus the AGT relay/registry deployed by `deploy/agentmesh-agt.yaml`). The historical AgentMesh npm SDK dependency and vendored relay/registry/SDK forks were removed in Phase 5.2 after the gap-closing patches landed upstream.

Changes to mesh transport, identity, signing, or relay/registry behavior must be proposed upstream first when they belong in AGT, and must include an ADR plus security-audit notes when they affect AzureClaw's trust boundary.

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

### Copyright Headers

Every AzureClaw-authored source file (`.rs`, `.ts`, `.tsx`, `.js`, `.sh`) **must** begin with the two-line Microsoft + MIT copyright header:

```
// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
```

(Use `#` instead of `//` for shell scripts.) For shell scripts with a shebang, the shebang stays on line 1 and the header follows immediately on lines 2–3.

The CI gate `ci/check-copyright-headers.sh` enforces this on every PR. Add the header to any new file before opening your PR. Vendored code under `vendor/` is excluded — do not add Microsoft headers there.

## Code of Conduct

[Microsoft Open Source Code of Conduct](https://opensource.microsoft.com/codeofconduct/) · [FAQ](https://opensource.microsoft.com/codeofconduct/faq/) · [opencode@microsoft.com](mailto:opencode@microsoft.com)

## CLA

Most contributions require a [Contributor License Agreement](https://cla.opensource.microsoft.com). A bot will guide you when you submit a PR.

## Security

Report vulnerabilities via [SECURITY.md](SECURITY.md), not GitHub Issues.
