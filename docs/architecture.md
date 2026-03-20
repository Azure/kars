# AzureClaw Architecture

## Overview

AzureClaw is a Kubernetes-native stack for running OpenClaw AI assistants safely on Azure. It replaces NVIDIA's single-node OpenShell/NemoClaw approach with a production-grade architecture built on AKS and Azure AI services.

## Components

### 1. AzureClaw CLI

**Language:** TypeScript

User interface for cluster initialization, onboarding, sandbox management, and policy operations.

**Commands:** `up`, `dev`, `connect`, `status`, `logs`, `model`, `trace`, `policy`, `approve`, `costs`, `destroy`, `init`, `onboard`, `launch`

### 2. Blueprint Controller

**Language:** Rust (kube-rs)

Kubernetes operator that watches `ClawSandbox` custom resources and reconciles:

- Isolated namespaces per sandbox
- OpenClaw pods with security constraints
- NetworkPolicies (default-deny egress)
- iptables egress-guard init containers for per-container network isolation
- Content Safety endpoint, token budgets, and IMDS credentials injected into inference router
- Workload Identity ServiceAccounts

### 3. Inference Router

**Language:** Rust (axum)

Sidecar proxy in every sandbox pod. Every inference call flows through this router:

- **Authentication:** Workload Identity federation + IMDS fallback (zero keys)
- **Token counting:** Prometheus metrics (`azureclaw_tokens_total`, `azureclaw_inference_latency_seconds`, `azureclaw_inference_requests_total`)
- **Content Safety:** Azure AI Content Safety text analysis (on by default)
- **Prompt Shields:** Jailbreak and prompt injection detection (on by default)
- **Token budgets:** Per-sandbox daily and per-request limits with HTTP 429 enforcement
- **Provider routing:** Azure OpenAI and Azure AI Foundry backends
- **Model listing:** `/v1/models` endpoint queries Foundry catalog live

### 4. Policy Engine

| Layer | Technology | Scope |
|-------|-----------|-------|
| Kernel | seccomp (Localhost profile) | Strict syscall allowlist |
| Container | Pod Security Standards | Read-only rootfs, non-root, drop ALL |
| Per-container egress | iptables init container (UID-based) | Agent restricted to localhost + DNS |
| L3/L4 Network | Kubernetes NetworkPolicy | Namespace-level egress control |
| Inference safety | Content Safety + Prompt Shields | Input/output filtering |
| Governance | Azure Policy for Kubernetes | Subscription-level constraints |
| VM isolation | Kata Containers (confidential) | Per-pod VM isolation |
| Runtime observability | Inspektor Gadget (eBPF) | Syscall/network/file tracing |

### 5. Sandbox Images

OCI images based on Azure Linux 3 with OpenClaw + Node.js 22. The inference router runs as a separate sidecar container.

## Security Model

See [security.md](security.md) for the defense-in-depth breakdown.

## Runtime Observability

Inspektor Gadget provides eBPF tracing. Deployed via `kubectl gadget deploy` during `azureclaw up`. Queried via `azureclaw trace <name>`.

## Roadmap

Planned but **not yet implemented**:

- Envoy L7 sidecar for HTTP method/path filtering
- Multi-region AKS deployment
- SBOM generation in CI
- Node compliance via azure-osconfig
