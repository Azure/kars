# Contributing to AzureClaw

This project welcomes contributions and suggestions.

## Quick Start

```bash
git clone https://github.com/<your-user>/azureclaw.git
cd azureclaw
make build    # Rust (controller + router) + TypeScript CLI
make test     # 14 unit tests (Rust)
make lint     # clippy + oxlint
```

## Project Structure

| Directory | Language | What It Is |
|-----------|----------|------------|
| `controller/` | Rust (kube-rs) | K8s operator — reconciles ClawSandbox CRDs into sandboxes |
| `inference-router/` | Rust (axum) | Per-sandbox sidecar — auth, safety, budgets, 18 Foundry APIs, AGT governance |
| `cli/` | TypeScript | 13 CLI commands + OpenClaw plugin + 9 Foundry skills |
| `cli/skills/` | Markdown | 9 SKILL.md files teaching the agent to use Foundry services |
| `cli/policies/` | YAML | AGT policy profiles (shell-safety, approval, rate-limit) |
| `deploy/bicep/` | Bicep | Azure infrastructure (AKS, ACR, KV, AOAI, Monitor) |
| `deploy/helm/` | YAML | Helm chart (CRD, controller, RBAC, seccomp, NetworkPolicy) |
| `deploy/seccomp/` | JSON | seccomp profile (`azureclaw-strict.json`) |
| `sandbox-images/` | Dockerfile | Azure Linux 3 sandbox image + entrypoint |
| `tests/e2e/` | Bash | E2E tests: Kind-based + live AKS infra tests |

## Development

### Rust (edition 2024, MSRV 1.88)

```bash
cargo build --release     # builds controller + inference-router
cargo test --all          # 14 unit tests (9 controller + 5 budget)
cargo clippy --all-targets -D warnings
```

### CLI (Node.js 22, TypeScript)

```bash
cd cli
npm install && npm run build && npm link
```

### Docker Images

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

## Pull Requests

1. Clear description of the change
2. Tests for new functionality
3. `make test && make lint` passes
4. Documentation updated if applicable

## Code of Conduct

[Microsoft Open Source Code of Conduct](https://opensource.microsoft.com/codeofconduct/) · [FAQ](https://opensource.microsoft.com/codeofconduct/faq/) · [opencode@microsoft.com](mailto:opencode@microsoft.com)

## CLA

Most contributions require a [Contributor License Agreement](https://cla.opensource.microsoft.com). A bot will guide you when you submit a PR.

## Security

Report vulnerabilities via [SECURITY.md](SECURITY.md), not GitHub Issues.
