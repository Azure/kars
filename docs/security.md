# AzureClaw Security

## Defense in Depth

AzureClaw implements multiple security layers, each independently enforceable:

### Layer 0: Azure Infrastructure
- Azure DDoS Protection
- Network Security Groups (NSG)
- AKS API server authorized IP ranges

### Layer 1: Azure Linux (Host OS)
- AKS nodes run Azure Linux
- SELinux in enforcing mode
- Automatic security patch updates via node image upgrades

### Layer 2: Kata VM Isolation (Confidential Level)
- Each pod runs in its own lightweight VM (Cloud Hypervisor) with a dedicated kernel
- Container escape attacks are trapped inside the VM, not the host kernel
- Requires `--isolation confidential` and a dedicated Kata node pool
- Uses AKS `KataVMIsolationPreview` feature

### Layer 3: Container Hardening
- Read-only root filesystem (`readOnlyRootFilesystem: true`)
- Non-root user (`runAsNonRoot: true`, UID 1000)
- No privilege escalation (`allowPrivilegeEscalation: false`)
- All capabilities dropped (`drop: [ALL]`)
- Writable only: `/sandbox` and `/tmp`

### Layer 4: Kernel Confinement (seccomp)
- **Standard isolation**: RuntimeDefault seccomp profile
- **Enhanced isolation** (default): Custom `azureclaw-strict` Localhost seccomp profile. Blocks mount, ptrace, bpf, module loading, namespace manipulation.
- Installed via DaemonSet on all nodes

### Layer 5: Network Segmentation
- Kubernetes NetworkPolicy with default-deny egress per sandbox namespace
- `egress-guard` iptables init container enforces per-container UID-based rules:
  - Agent (UID 1000): restricted to localhost + DNS only
  - Inference router (UID 1001): controlled by pod-level NetworkPolicy
- Additional endpoints require `azureclaw policy allow` or operator approval

### Layer 6: Inference Safety
- **Azure AI Content Safety:** Filter harmful content in inputs and outputs
- **Prompt Shields:** Detect jailbreak and prompt injection attempts
- **Token budgets:** Per-sandbox daily and per-request limits
- **Audit logging:** Prometheus metrics per sandbox (requests, latency, tokens)

## Identity & Access

- **Zero standing credentials:** No API keys in images or env vars
- **Workload Identity:** Pods authenticate via federated OIDC tokens
- **IMDS fallback:** Kubelet Managed Identity via Instance Metadata Service
- **Credential isolation:** Only inference-router (UID 1001) can reach IMDS. Agent container (UID 1000) is blocked by iptables.

## Runtime Observability (Inspektor Gadget)

[Inspektor Gadget](https://www.inspektor-gadget.io/) provides eBPF-powered observability:

- **Syscall tracing** via `azureclaw trace --exec`
- **Network flow monitoring** via `azureclaw trace --network`
- **File access tracing** via `azureclaw trace --files`
- **DNS monitoring** via `azureclaw trace --dns`

## Comparison with NemoClaw

| Feature | NemoClaw | AzureClaw |
|---------|----------|-----------|
| Container isolation | Docker + K3s | AKS (managed K8s) |
| Kernel hardening | Landlock + seccomp | seccomp (custom Localhost profile) |
| Network filtering | Custom proxy | NetworkPolicy + iptables UID-based egress |
| Hardware isolation | None | Kata VM per pod (`--isolation confidential`) |
| Identity | API keys | Managed Identity + Workload Identity |
| Inference safety | None | Content Safety + Prompt Shields |
| Runtime observability | TUI + logs | Inspektor Gadget (eBPF) + Prometheus |
| Scale | Single node | Multi-node AKS cluster |

## Roadmap

The following security features are planned but **not yet implemented**:

- **Envoy L7 sidecar:** HTTP method/path filtering for non-inference egress
- **Image signing:** Notation with Ratify admission controller
- **SBOM generation:** Automatic SPDX generation attached to images
- **Node compliance:** azure-osconfig for CIS AKS Optimized benchmarks
- **Alerting:** Token spike and egress anomaly alerts via Azure Monitor
