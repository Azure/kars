# Quickstart

Get a governed, sandboxed agent running on your laptop in **three commands** — no Azure account, no Rust, no clone.

> 📋 **You need:** the [`docker` CLI](https://docs.docker.com/get-docker/) (or Podman's `docker`-compatible shim) · [Node.js 22+](https://nodejs.org/) · a **GitHub Copilot** seat (any tier). Nothing else. *(For the production-shaped kind loop, see the tip below — it also accepts Podman and nerdctl.)*

```bash
# 1. Install the CLI (public, signed, SLSA-attested)
npm i -g @kars-runtime/cli

# 2. Launch a sandboxed agent from the published images
kars dev --release

# 3. Chat with it
kars connect dev-agent
```

On first run, `kars dev` asks you to pick an inference provider — choose **GitHub Copilot** (one device-code login, no Azure account). That's it: you now have an agent whose every model call, tool call, and network request is brokered by the in-pod Rust router.

> 💡 **Tip — the recommended dev loop is kind, not a single container.** Swap step 2 for `kars dev --release --target local-k8s` to run the *same* images on a local [kind](https://kind.sigs.k8s.io/) cluster in the real production pod shape (separate router container, `NetworkPolicy`, seccomp). It behaves almost identically to AKS, and kind drives **Docker, Podman, or nerdctl** — your choice.

## What just happened?

`kars dev --release` pulled the published, cosign-signed sandbox image plus the AGT mesh relay and ran them locally. The agent runs with **no credentials of its own** — the router holds them and enforces identity, content safety, token budgets, tool policy, and a tamper-evident audit chain on every call. See [Architecture → Two modes](architecture.md#two-modes) for exactly what is and isn't isolated in dev mode.

## Next steps

- 🔰 **Go deeper on local dev** → [Getting started](getting-started.md) — provider options (Foundry, GitHub Models), building from source, and the full local walkthrough.
- ☁️ **Run it on AKS** → [Getting started → Deploy to AKS](getting-started.md#step-2--deploy-to-aks) — `kars up` provisions the cluster, controller, and your first sandbox.
- 🧭 **Understand the design** → [Architecture](architecture.md) and the [architecture diagrams](architecture-diagrams.md).
- 📊 **Check feature status** → [Feature maturity](maturity.md) — what's GA, preview, and planned.
