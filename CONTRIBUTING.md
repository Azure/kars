# Contributing to AzureClaw

Thank you for your interest in contributing to AzureClaw! This project welcomes contributions and suggestions.

## Getting Started

1. Fork the repository
2. Clone your fork: `git clone https://github.com/<your-user>/azureclaw.git`
3. Create a branch: `git checkout -b my-feature`
4. Make your changes
5. Run tests: `npm test` (CLI), `go test ./...` (controller)
6. Submit a pull request

## Development Setup

### CLI (TypeScript — OpenClaw plugin)

```bash
cd cli
npm install
npm run build        # tsc + copy seccomp profiles
npm link             # makes 'azureclaw' available globally
```

### Controller & Inference Router (Rust)

```bash
cargo build --release   # builds both crates
cargo test --all
cargo clippy --all-targets
```

### Sandbox Image (Docker + Azure Linux 4)

```bash
# Requires Azure Linux 4 Alpha base image (see README for access)
docker build --build-arg AZURELINUX_BASE=azlpubstagingacroxz2o4gw.azurecr.io/azurelinux/base/core:4.0 \
  -t azureclaw-sandbox:dev -f sandbox-images/openclaw/Dockerfile .
```

### Full E2E Test

```bash
azureclaw onboard                    # configure Azure OpenAI (once)
azureclaw dev                        # start sandbox
azureclaw connect dev-agent          # chat with agent via OpenClaw TUI
azureclaw status dev-agent           # check health + metrics
azureclaw destroy dev-agent          # tear down
```

## Code of Conduct

This project has adopted the [Microsoft Open Source Code of Conduct](https://opensource.microsoft.com/codeofconduct/).
For more information see the [Code of Conduct FAQ](https://opensource.microsoft.com/codeofconduct/faq/)
or contact [opencode@microsoft.com](mailto:opencode@microsoft.com) with questions.

## Contributor License Agreement

Most contributions require you to agree to a Contributor License Agreement (CLA) declaring that you have the right to, and actually do, grant us the rights to use your contribution. For details, visit https://cla.opensource.microsoft.com.

When you submit a pull request, a CLA bot will automatically determine whether you need to provide a CLA and decorate the PR appropriately.

## Pull Request Process

1. Ensure your PR has a clear description of the change
2. Include tests for new functionality
3. Update documentation if applicable
4. Ensure CI passes
5. Request review from maintainers

## Reporting Issues

Use GitHub Issues for bug reports and feature requests. For security vulnerabilities, see [SECURITY.md](SECURITY.md).
