# Contributing to kars 👋

**Welcome — and thank you!** Whether you're fixing a typo, adding a plugin, or shipping your first-ever open-source pull request, you're exactly the kind of person this project is for. We're genuinely glad you're here.

This guide is built to get you from `git clone` to **your first merged PR in about 10 minutes**. No deep Kubernetes wizardry required to get started. Let's go. 🚀

---

## 🌱 Your First PR in 10 Minutes

New to kars? Start here. Seriously — you don't have to read the whole document.

### 1. Find something to work on (30 seconds)

We curate two label tracks specifically for newcomers:

- 🟢 **[`good first issue`](https://github.com/Azure/kars/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22)** — small, well-scoped, beginner-friendly tasks with clear acceptance criteria.
- 🤝 **[`help wanted`](https://github.com/Azure/kars/issues?q=is%3Aissue+is%3Aopen+label%3A%22help+wanted%22)** — slightly bigger tasks the maintainers would love a hand with.

Pick one, drop a comment saying "I'd like to take this," and it's yours. No need to ask twice — just start.

### 2. Set up locally (5 minutes)

```bash
# 1. Fork the repo on GitHub, then clone YOUR fork:
git clone https://github.com/<your-user>/kars.git
cd kars

# 2. Build everything (Rust controller + router, TypeScript CLI):
make build

# 3. Make sure the baseline is green before you change anything:
make test     # 205 Rust unit tests + 207 CLI tests (vitest)
make lint     # clippy + oxlint
```

> 💡 First time with Rust? Install via [rustup](https://rustup.rs) (we use edition 2024, MSRV 1.88).
> First time with the CLI? You'll need [Node.js 22+](https://nodejs.org).

### 3. Make your change

```bash
git checkout -b my-first-contribution
# ... edit, add a test, save ...
make test && make lint    # keep it green
```

Add the two-line copyright header to any **new** file you create (details in [Code Style](#-code-style)).

### 4. Open your PR 🎉

```bash
git commit -m "Fix: short description of what you did"
git push origin my-first-contribution
```

Then open a pull request with:

1. A clear description of the change
2. Tests for new functionality
3. Passing `make test && make lint`
4. Updated docs, if applicable

A friendly CLA bot will guide you through a one-time [Contributor License Agreement](https://cla.opensource.microsoft.com) on your first PR — it takes seconds.

That's it. The maintainer team (`@KarsTeam`) reviews open PRs **at least weekly**. We'll work with you to get it merged. ❤️

---

## 🧭 What You Can Work On

kars is a **community-supported, best-effort** project, and we love contributions from Azure / AKS operators, OpenClaw / OpenAI Agents SDK adopters, security researchers, and MCP/plugin vendors.

**Great PRs we actively welcome:**

- 🐛 **Bug fixes** against documented behavior (regressions, incorrect error handling, unmet API contracts)
- 🔌 **New MCP servers** that conform to the `McpServer` CRD and keep sandbox isolation intact
- 💬 **New channels** (Telegram / Slack / Discord / WhatsApp pattern) or **web-search plugins** (Brave / Tavily / Exa / Firecrawl / Perplexity / OpenAI pattern)
- 🧩 **New Tier-2 BYO runtime adapters** implementing the multi-runtime architecture per [`docs/runtimes.md`](docs/runtimes.md)
- 🛡️ **Egress allowlist contributions** for the signed-OCI workflow (see [`docs/operations/supply-chain.md`](docs/operations/supply-chain.md) and [`docs/egress-proxy.md`](docs/egress-proxy.md))
- 📚 **Documentation improvements** — use-case blueprints, troubleshooting guides, architecture clarifications
- ✅ **Test coverage improvements** (chaos, conformance, unit tests) that increase confidence in isolation or governance
- ⚡ **Performance fixes** that don't relax security invariants (latency, throughput, resource utilization)

If your idea touches deeper architecture (new CRDs, transports, runtimes, dependencies), that's still very welcome — just see [Core Architecture Changes](#-core-architecture-changes) below for the slightly longer path.

---

## 🔐 Security: Report Privately, Get Credited

We take security seriously, and we want to make reporting **easy and rewarding** for you.

- **Please don't open public issues or PRs for vulnerabilities.** Instead, follow the private reporting process in **[SECURITY.md](SECURITY.md)**.
- Every security fix is **fast-tracked** and **credited to the reporter**. 🏆
- For anything sensitive, the SECURITY.md flow is always the right channel — not a PR.

This keeps users safe while we ship the fix, and makes sure you get the recognition you deserve.

---

## 🛠️ Local Development Guide

### Project structure at a glance

| Directory | Language | What It Is |
|-----------|----------|------------|
| `controller/` | Rust (kube-rs) | K8s operator — reconciles KarsSandbox CRDs into sandboxes |
| `inference-router/` | Rust (axum) | Per-sandbox router — auth, safety, budgets, 18 Foundry APIs, native AGT governance |
| `cli/` | TypeScript | 30+ CLI commands + OpenClaw plugin + 10 Foundry skills |
| `runtimes/openclaw/skills/` | Markdown | 10 SKILL.md files teaching the OpenClaw agent to use kars + Foundry services |
| `cli/profiles/agt/` | YAML | AGT policy profiles (default/offload) inlined by the CLI into `ToolPolicy.spec.agtProfile.inline` |
| `deploy/bicep/` | Bicep | Azure infrastructure (AKS, ACR, KV, AOAI, Monitor) |
| `deploy/helm/` | YAML | Helm chart (CRD, controller, RBAC, seccomp, NetworkPolicy) |
| `deploy/seccomp/` | JSON | seccomp profile (`kars-strict.json`) |
| `sandbox-images/` | Dockerfile | Azure Linux 3 sandbox image + entrypoint |
| `tests/e2e/` | Bash | E2E tests: Kind-based + live AKS infra tests |

### TypeScript CLI (Node.js 22+)

```bash
cd cli
npm ci && npm run build && npm link   # compile + link the global `kars` command
npm test                              # vitest
npm run lint                          # oxlint
npm run typecheck                     # tsc --noEmit
```

### Rust (edition 2024, MSRV 1.88)

```bash
cargo build --release     # builds controller + inference-router
cargo test --all          # 205 unit tests (74 controller + 105 router + 26 integration)
cargo clippy --all-targets -- -D warnings
cargo fmt --all           # format
```

### Local sandbox (kind-first)

The recommended validation loop reproduces the real production pod shape on a
local [kind](https://kind.sigs.k8s.io/) cluster — separate router container,
`NetworkPolicy`, seccomp, the whole sandbox shape:

```bash
kars dev --build --target local-k8s   # build from source, run on a local kind cluster
```

For a faster prompt/tool inner loop, the single-container Docker target builds
and runs everything in one container (no K8s glue):

```bash
kars dev --build          # build + run locally in a single Docker container
```

Validate changes you intend to ship on `--target local-k8s` before opening a PR.

### Push to ACR

```bash
kars push --only sandbox --apply   # build sandbox image, push to ACR, restart pods
kars push                          # build + push all kars images (controller, router, sandbox)
```

### Docker images (Makefile)

```bash
make images               # builds controller + inference-router images
make push                 # pushes to configured ACR
```

### Local end-to-end loop

```bash
kars credentials          # configure Azure OpenAI (or just run `dev`/`up` — prompts inline)
kars dev                  # start local sandbox
kars connect dev-agent
kars destroy dev-agent
```

---

## 🔌 Adding Channels and Plugins

kars uses one consistent pattern for channels (Telegram, Slack, Discord, WhatsApp) and third-party plugins (Brave, Tavily, Exa, Firecrawl, Perplexity, OpenAI):

```
CLI flag → Docker env var → entrypoint auto-config → plugins.allow + plugins.entries
```

### Adding a new channel flag

1. **CLI `add.ts`** — add a flag (e.g., `--myapp-token <token>`) and map it to an env var (e.g., `MYAPP_BOT_TOKEN`) in the deployment spec
2. **CLI `credentials.ts`** — add the flag to the `update` subcommand's `flagToEnv` map
3. **`entrypoint.sh`** — add a block that reads the env var and builds the `CHANNELS_CONFIG`, `PLUGINS_LIST`, and `PLUGINS_ENTRIES` strings (follow the Telegram/Slack pattern)
4. **Controller `reconciler.rs`** — the controller mounts the `<name>-credentials` secret via `envFrom` (`optional: true`), so credential env vars are injected automatically

### Adding a new plugin API key flag

1. **CLI `add.ts`** — add a flag (e.g., `--myplugin-api-key <key>`) and map it to an env var (e.g., `MYPLUGIN_API_KEY`)
2. **CLI `credentials.ts`** — add the flag to the `update` subcommand's `flagToEnv` map
3. **`entrypoint.sh`** — add a `"myplugin:MYPLUGIN_API_KEY"` entry to the `for plugin_pair in ...` loop. OpenClaw reads the env var directly for auth; the entrypoint just registers the plugin in `plugins.allow` + `plugins.entries`

### Credentials secret convention

Credentials live in a K8s secret named `<sandbox-name>-credentials` in the sandbox namespace (`kars-<name>`). The controller mounts it via `envFrom` with `optional: true` so pods start even if no credentials secret exists. Use `kars credentials update <name> --telegram-token <token>` to create/update the secret.

---

## 🎨 Code Style

### Copyright headers

Every kars-authored source file (`.rs`, `.ts`, `.tsx`, `.js`, `.sh`) **must** begin with the two-line Microsoft + MIT copyright header:

```
// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
```

(Use `#` instead of `//` for shell scripts. For shell scripts with a shebang, the shebang stays on line 1 and the header follows on lines 2–3.)

The CI gate `ci/check-copyright-headers.sh` enforces this on every PR. Add the header to any new file before opening your PR. Vendored code under `vendor/` is excluded — don't add Microsoft headers there.

### File size guidelines

<details>
<summary><strong>📏 File size soft limits (Rust & TypeScript)</strong> — click to expand</summary>

#### Rust

Source files **should stay under 1500 LOC**. Files over that threshold tend to hide bugs, slow down reviews, and make refactoring risky. Today the following files are over budget and are candidates for incremental splitting:

| File | LOC | Suggested split |
|---|---|---|
| `inference-router/src/routes.rs` | ~5000 | `routes/{inference,governance,mesh,egress,admin}.rs` |
| `inference-router/src/handoff.rs` | ~2625 | State machine + crypto are separable |
| `controller/src/reconciler.rs` | ~2380 | Per-resource builders are separable |
| `controller/src/mesh_peer.rs` | ~1970 | Discovery vs. connection state |
| `inference-router/src/governance.rs` | ~1250 | Near budget — watch before extending |
| `inference-router/src/spawn.rs` | ~1160 | Near budget — watch before extending |

**Rule of thumb:** a PR that pushes any file over 1500 LOC should either (a) split the file in the same PR, or (b) open a follow-up tracking issue. Reviewers are expected to flag this.

This is a soft rule — edge cases exist (e.g., generated code, large lookup tables). Use judgement; call it out in the PR description when you exceed it intentionally.

#### TypeScript

`cli/src/plugin.ts` is a known outlier (>4000 LOC) tracked separately — the same rule applies to new TS files.

</details>

---

## 🏛️ Core Architecture Changes

Most contributions never need to read this section — but if your idea changes a foundational invariant of kars (isolation, transport, governance, or top-level CRDs), here's the path to getting it merged. These guardrails exist to keep every kars user's runtime secure; we're happy to work through them with you.

<details>
<summary><strong>Deeper changes that require an ADR + RFC first</strong> — click to expand</summary>

### Out of scope (we won't merge these as-is)

To preserve the security and trust boundary that makes kars valuable, these changes can't be merged without the formal process below:

- **New cross-cluster transports** — Microsoft AGT AgentMesh is the only sanctioned transport. Changes to inter-cluster communication require an ADR and CELA review; kars no longer carries a vendored AgentMesh transport fork.
- **Changes to sandbox isolation** — modifications to UID/GID, Landlock rules, seccomp profiles, or NetworkPolicy that weaken pod isolation or increase privilege.
- **Inference router / governance bypass** — any change that routes agent traffic outside the router, skips the governance chain, or adds unauthenticated API endpoints.
- **Direct cloud-side telemetry** — telemetry must remain opt-in via OpenTelemetry (see the Data Collection notice in the README). Direct Azure Monitor / Application Insights SDKs are not accepted.
- **New top-level CRDs** without a published ADR in `docs/adr/` and an RFC issue.

### The path for architecture-level changes

PRs that propose new CRDs, new runtimes, transport changes, or new direct dependencies require:

1. An **Architecture Decision Record (ADR)** in `docs/adr/` (see existing ADRs for format)
2. A public **RFC issue** discussing motivation, design trade-offs, and impact on existing users
3. **Security review notes** captured in the PR (threat model touched, isolation/governance impact) with sign-off from at least one maintainer

The maintainer team will not review implementation PRs for architecture changes without a prior ADR + RFC. Implementation PRs should follow the slice-train pattern (breaking large changes into reviewable chunks).

### AgentMesh provider changes

kars runs exclusively on Microsoft AGT AgentMesh (`@microsoft/agent-governance-sdk` plus the AGT relay/registry deployed by `deploy/agentmesh-agt.yaml`). The historical AgentMesh npm SDK dependency and vendored relay/registry/SDK forks were removed in Phase 5.2 after the gap-closing patches landed upstream.

Changes to mesh transport, identity, signing, or relay/registry behavior must be proposed upstream first when they belong in AGT, and must include an ADR plus security-audit notes when they affect kars's trust boundary.

### Triage cadence & response time

- The maintainer team (`@KarsTeam`) reviews open PRs **at least weekly**.
- PRs without a CLA signature, missing security-audit documentation, or failing CI gates won't be reviewed until those are resolved.
- Support is **community-driven, best-effort** — no SLAs or guaranteed response times. For critical security issues, follow the [SECURITY.md](SECURITY.md) process instead of opening a PR.

</details>

---

## 📦 Pull Request Checklist

1. Clear description of the change
2. Tests for new functionality
3. `make test && make lint` passes
4. Documentation updated if applicable

---

## 🤝 Code of Conduct

This project follows the [Microsoft Open Source Code of Conduct](https://opensource.microsoft.com/codeofconduct/) · [FAQ](https://opensource.microsoft.com/codeofconduct/faq/) · [opencode@microsoft.com](mailto:opencode@microsoft.com).

## ✍️ Contributor License Agreement

Most contributions require a [Contributor License Agreement](https://cla.opensource.microsoft.com). A bot will guide you when you submit a PR — it's a one-time, few-second step.

## 🔒 Security

Report vulnerabilities via [SECURITY.md](SECURITY.md), not GitHub Issues. Every fix is fast-tracked and credited to you.

---

**Still have questions?** Open a [discussion](https://github.com/Azure/kars/discussions) or comment on an issue. We'd genuinely rather you ask than feel stuck. Happy contributing! 🎉
