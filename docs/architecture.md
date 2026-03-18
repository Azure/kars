# AzureClaw Architecture

## Overview

AzureClaw is a Kubernetes-native stack for running OpenClaw AI assistants safely on Azure. It replaces NVIDIA's single-node OpenShell/NemoClaw approach with a production-grade architecture built on AKS, Azure Container Linux, and Azure AI services.

## Components

### 1. AzureClaw CLI (`azureclaw`)

**Language:** TypeScript

The primary user interface. Handles cluster initialization, onboarding, sandbox management, and policy operations. Designed to be familiar to NemoClaw users while adding Azure-specific capabilities.

### 2. Blueprint Controller

**Language:** Rust (kube-rs, CNCF Sandbox)

A Kubernetes operator built with kube-rs that watches `ClawSandbox` custom resources and reconciles the desired state:

- Creates isolated namespaces per sandbox
- Deploys OpenClaw pods with security constraints
- Configures NetworkPolicies and Envoy sidecars
- Manages inference routing
- Handles hot-reload of dynamic policies

### 3. Inference Router

**Language:** Rust (axum)

High-performance reverse proxy that sits between OpenClaw and Azure AI backends. Every inference call flows through this router:

- **Authentication:** Workload Identity (AKS) or API key from secret mount (dev mode)
- **Token counting:** Prometheus metrics per sandbox (`azureclaw_tokens_total`, `azureclaw_inference_latency_seconds`)
- **Content safety:** Azure AI Content Safety + Prompt Shields (on by default)
- **Audit logging:** Every request logged with sandbox ID, model, status, latency
- **Binary size:** ~5MB release build

Routes all LLM API calls from sandboxes to Azure OpenAI / Azure AI Foundry. Authenticates using Workload Identity (no API keys in sandboxes). Integrates with Azure AI Content Safety and Prompt Shields.

### 4. Policy Engine

Multi-layer enforcement:

| Layer | Technology | Scope |
|-------|-----------|-------|
| L3/L4 Network | Kubernetes NetworkPolicy (Cilium) | Namespace-level egress control |
| L7 Application | Envoy sidecar proxy | HTTP method/path/header filtering |
| Kernel | seccomp + SELinux | Syscall filtering + MAC (ACL-native SELinux) |
| Container | Pod Security Standards | Read-only rootfs, non-root, no privesc |
| Hardware | Confidential Containers (SEV-SNP) | Memory encryption (optional add-on) |
| Inference | Azure AI Content Safety | Input/output content filtering |
| Runtime observability | Inspektor Gadget (eBPF) | Syscall/network/file/process tracing |
| Node compliance | azure-osconfig (TODO) | CIS AKS Optimized + STIG baselines |

### 5. Sandbox Images

Minimal OCI images based on Azure Linux 4 with OpenClaw + Rust inference router pre-installed. Auto-configured by entrypoint script — OpenClaw gateway, inference router, and agent identity are set up on first start.

## Data Flow

**Local dev mode (`azureclaw dev`):**
```
azureclaw CLI ──▶ Docker ──▶ Azure Linux 4 container
                                    │
                              ┌─────┴─────┐
                              ▼           ▼
                         OpenClaw    Inference Router (Rust)
                         Gateway         │
                              │          ▼
                              │    Azure OpenAI / AI Foundry
                              ▼
                         Web UI (port 18789)
```

**AKS production mode (`azureclaw up`):**
```
azureclaw CLI ──▶ kubectl ──▶ AKS API Server
                                    │
                                    ▼
                             Blueprint Controller (Rust)
                                    │
                     ┌──────────────┼──────────────┐
                     ▼              ▼              ▼
               Namespace      NetworkPolicy    Deployment
                      (isolated)     (strict)         (sandbox pod)
                                                          │
                                                          ▼
                                                    OpenClaw Agent
                                                          │
                                                          ▼
                                                    Envoy Sidecar
                                                          │
                                                          ▼
                                                   Inference Router
                                                          │
                                                          ▼
                                                    Azure OpenAI
```

## Security Model

See [security.md](security.md) for the full defense-in-depth breakdown.

## Azure Linux OS Strategy

AzureClaw uses two related Microsoft Linux distributions:

| Layer | Distro | Where |
|-------|--------|-------|
| **Container image** | Azure Linux 4 | Base OS inside sandbox containers |
| **Node OS** | Azure Container Linux (ACL) | AKS node pool host OS |

Both are SELinux-enforcing, CIS-hardened, and share the same rpm/tdnf package ecosystem.

### Azure Linux 4 (container base)

- Base image for all sandbox Dockerfiles: `azlpubstagingacroxz2o4gw.azurecr.io/azurelinux/base/core:4.0`
- Same image runs locally (`azureclaw dev`) and on AKS (`azureclaw up`)
- Lightweight tdnf package manager, no apt/Debian dependencies

### Azure Container Linux (AKS node OS)

AKS node pools run Azure Container Linux:

- **Minimal attack surface:** No package manager on the host
- **Immutable root FS:** A/B partition updates
- **Verified boot:** dm-verity chain
- **SELinux enforcing:** Mandatory access control
- **CIS L1 hardened:** Out of the box
- **Fast boot (~2s):** Critical for scaling sandboxes quickly
- **azure-osconfig (TODO):** Continuous CIS AKS Optimized + DISA STIG compliance enforcement via declarative desired-state model

## Runtime Observability (Inspektor Gadget)

[Inspektor Gadget](https://www.inspektor-gadget.io/) runs as a DaemonSet on ACL nodes, providing eBPF-powered tracing:

- `trace exec` — monitor process execution inside sandboxes
- `trace open` — track file opens, validate filesystem policy
- `trace tcp` / `trace dns` — network observability per pod
- `trace mount` — detect mount attempts (should be blocked)
- `snapshot process` — point-in-time process inventory per sandbox

Data feeds into Azure Monitor / Log Analytics, Prometheus metrics, and the AzureClaw TUI.
