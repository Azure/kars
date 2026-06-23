# kars CLI — `@kars-runtime/cli`

The command-line interface for **[kars](https://github.com/Azure/kars)** — a
secure AI agent runtime on Azure. Run OpenClaw (and other) agents in isolated
sandboxes with end-to-end encrypted inter-agent messaging, egress control, and
policy governance — locally on Docker/kind or on AKS.

## Install

```bash
npm i -g @kars-runtime/cli
```

This installs the `kars` command. (Node.js 22+ required.)

> No-compile alternative — the public installer pulls the latest signed release:
> ```bash
> curl -fsSL https://raw.githubusercontent.com/Azure/kars/main/install.sh | bash
> ```

## Quick start

```bash
# Run a sandboxed agent from the published, cosign-signed images (no compile)
kars dev --release

# …or on a local Kubernetes (kind) cluster — real K8s posture
kars dev --release --target local-k8s
```

On first launch you pick an inference provider — **GitHub Copilot** is easiest
(one device-code login, no Azure account). Then talk to your agent:

```bash
kars connect dev-agent
```

When you're ready for a managed cluster:

```bash
kars up --name prod-agent --region swedencentral --release   # provisions AKS + Foundry + the full stack from signed public images (no build)
```

## Common commands

| Command | What it does |
|---|---|
| `kars dev --release` | Run a sandbox from published images (Docker), no compile |
| `kars dev --release --target local-k8s` | Same, on a local kind cluster |
| `kars connect <name>` | Open the agent chat TUI |
| `kars up --release` | Provision AKS + ACR + Foundry from signed public images (no build) |
| `kars add` | Add a sandbox / runtime to an existing deployment |
| `kars operator` | Live operator dashboard (agents, mesh, security posture) |
| `kars --help` | Full command list |

## Security & provenance

Every published kars artefact — container images **and** this CLI tarball — is
**cosign keyless-signed**, ships an **SPDX SBOM**, and carries a **SLSA build
provenance attestation**. The CLI is published to npm via **OIDC trusted
publishing** (no long-lived tokens). Verify with `npm audit signatures`,
`cosign verify`, and `gh attestation verify`.

## Links

- **Repository & docs:** https://github.com/Azure/kars
- **Getting started:** https://github.com/Azure/kars/blob/main/docs/getting-started.md
- **License:** MIT

> kars is an open-source reference implementation maintained under the Azure
> GitHub organization. Not an officially supported Microsoft product.
