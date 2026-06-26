# Quickstart

Get a governed, sandboxed agent running on your laptop in **three commands** — no Azure account, no Rust, no clone.

<p align="center">
  <img src="assets/kars-dev-firstrun.gif" alt="kars first run on a local kind cluster" width="100%" />
  <br />
  <em>First run on a local kind cluster (<a href="assets/kars-dev-firstrun.cast">replayable asciinema cast</a>).</em>
</p>

> 📋 **You need:** [`kind`](https://kind.sigs.k8s.io/) · [`kubectl`](https://kubernetes.io/docs/tasks/tools/) · any container runtime (**Docker, Podman, or nerdctl** — kind drives all three) · [Node.js 22+](https://nodejs.org/) · a **GitHub Copilot** seat (any tier). Nothing else — no Azure account, no Rust.

```bash
# 1. Install the CLI (public, signed, SLSA-attested)
npm i -g @kars-runtime/cli

# 2. Bring up a governed agent on a local kind cluster (the real production pod shape)
kars dev --release --target local-k8s

# 3. Chat with it
kars connect dev-agent
```

On first run, `kars dev` asks you to pick an inference provider — choose **GitHub Copilot** (one device-code login, no Azure account). That's it: you now have an agent on a real Kubernetes cluster whose every model call, tool call, and network request is brokered by the in-pod Rust router — exactly as it runs on AKS.

> ⚡ **Even faster, less faithful.** Drop `--target local-k8s` to run a single container instead of a kind cluster (`kars dev --release`). It's the quickest path to a chat, but it co-locates the agent and router — not the production pod shape. This path uses the `docker` CLI specifically.

## What just happened?

`kars dev --release --target local-k8s` pulled the published, cosign-signed images and ran them on a local kind cluster in the real sandbox shape — agent (UID 1000) and inference router (UID 1001) in separate containers, with the `egress-guard` init container and a `NetworkPolicy`. The agent runs with **no credentials of its own** — the router holds them and enforces identity, content safety, token budgets, tool policy, and a tamper-evident audit chain on every call. See [Architecture → Two modes](architecture.md#two-modes) for exactly what is and isn't isolated in each mode.

## Next steps

- 🔰 **Go deeper on local dev** → [Getting started](getting-started.md) — provider options (Foundry, GitHub Models), building from source, and the full local walkthrough.
- ☁️ **Run it on AKS** → [Getting started → Deploy to AKS](getting-started.md#step-2--deploy-to-aks) — `kars up` provisions the cluster, controller, and your first sandbox.
- 🧭 **Understand the design** → [Architecture](architecture.md) and the [architecture diagrams](architecture-diagrams.md).
- 📊 **Check feature status** → [Feature maturity](maturity.md) — what's GA, preview, and planned.
