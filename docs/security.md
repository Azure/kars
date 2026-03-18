# AzureClaw Security

## Defense in Depth

AzureClaw implements 8 security layers, each independently enforceable:

### Layer 0: Azure Infrastructure
- Azure DDoS Protection
- Network Security Groups (NSG)
- Azure Firewall (optional)
- Private Link for Azure services

### Layer 1: Azure Container Linux (Host OS)
- Immutable, read-only root filesystem
- SELinux in enforcing mode
- Verified boot chain (dm-verity)
- CIS Level 1 benchmark compliance
- No package manager — minimal attack surface
- Automatic security patch updates

### Layer 2: Confidential Containers (Optional Add-on)
- AMD SEV-SNP or Intel TDX hardware encryption
- Workload runs in a Trusted Execution Environment (TEE)
- Memory encrypted — not readable by host, hypervisor, or cloud operator
- Hardware attestation for workload integrity
- **Optional add-on** (`--isolation confidential`) — not the default isolation mode
- Recommended for regulated industries, government workloads, or highly sensitive data
- Maintains competitive parity with NemoClaw's TEE narrative

### Layer 3: Container Hardening
- Read-only root filesystem (`readOnlyRootFilesystem: true`)
- Non-root user (`runAsNonRoot: true`, UID 1000)
- No privilege escalation (`allowPrivilegeEscalation: false`)
- All capabilities dropped (`drop: [ALL]`)
- Writable only: `/sandbox` and `/tmp`

### Layer 4: Kernel Confinement
- **seccomp:** Deny-by-default syscall filter. Only ~200 safe syscalls allowed. Blocks mount, ptrace, bpf, module loading, namespace manipulation.
- **SELinux:** Mandatory access control via Azure Container Linux's enforcing SELinux policy. Sandbox pods run under the `azureclaw_sandbox_t` SELinux type, which restricts file paths, network socket types, and capabilities. Unlike AppArmor (used by NemoClaw on Ubuntu), SELinux is ACL's native MAC — no extra profiles to load.

### Layer 5: Network Segmentation
- Kubernetes NetworkPolicy with Cilium or Azure NPM
- Default-deny egress per sandbox namespace
- Only DNS and inference router accessible by default
- Additional endpoints require explicit policy or operator approval

### Layer 6: Application Firewall (Envoy L7)
- HTTP method + path filtering (e.g., allow `GET /repos/**` but block `POST`)
- TLS origination and inspection
- Request/response logging for audit
- Rate limiting per endpoint
- Request body size limits

### Layer 7: Inference Safety
- **Azure AI Content Safety:** Filter harmful content in inputs and outputs
- **Prompt Shields:** Detect jailbreak and prompt injection attempts
- **Groundedness detection:** Prevent hallucination
- **Token budgets:** Per-sandbox daily and per-request limits
- **Audit logging:** Every inference call logged to Azure Monitor

## Identity & Access

- **Zero standing credentials:** No API keys baked into images
- **Workload Identity:** Pods authenticate via federated OIDC tokens
- **Key Vault CSI:** Secrets mounted as tmpfs volumes, auto-rotated
- **Entra ID RBAC:** Operator access through Azure Entra ID groups
- **Audit trail:** All actions logged to Azure Monitor

## Supply Chain Security

- **Image signing:** Notation (CNCF standard) with Ratify admission controller
- **Vulnerability scanning:** ACR vulnerability scanning + SBOM generation
- **SBOM:** Automatic SPDX generation attached to images
- **Registry:** Azure Container Registry with geo-replication and quarantine
- **Admission control:** Only verified, signed images from allowed registries

## Compliance

Built-in Azure Policy packs for:
- CIS Kubernetes Benchmark
- NIST 800-53
- PCI-DSS
- SOC 2
- ISO 27001

Continuous node compliance via azure-osconfig (TODO) with CIS AKS Optimized benchmarks.

## Compliance (TODO)

Compliance enforcement is planned via **azure-osconfig** + the **Compliance Augmentation Engine** rather than Defender for Cloud:

- **azure-osconfig daemon** on ACL nodes — open-source, declarative security configuration
- **CIS AKS Optimized Azure Linux benchmark** — purpose-built for AKS nodes
- **DISA STIG support** — 400+ rules per distro, validated against DISA SCC scanner
- **Multi-authority management** — Azure Policy + GitOps + local (works air-gapped)
- **Automated remediation** — desired-state model continuously reconciles drift

> Status: TODO — requires integration with azure-osconfig team.

## Runtime Observability (Inspektor Gadget)

[Inspektor Gadget](https://www.inspektor-gadget.io/) provides eBPF-powered observability on ACL nodes:

- **Syscall tracing** — validate seccomp profiles, detect anomalies
- **Network flow monitoring** — real-time TCP/UDP/DNS visibility per pod
- **File access tracing** — verify filesystem policies, detect unexpected writes
- **Process lifecycle** — track process creation, detect unexpected binaries
- **Container escape detection** — monitor for namespace breakouts

## Comparison with NemoClaw

| Feature | NemoClaw | AzureClaw |
|---------|----------|-----------|
| Container isolation | Docker + K3s | AKS (managed K8s) |
| Kernel hardening | Landlock + seccomp | seccomp + SELinux (ACL-native) |
| Network filtering | Custom proxy | Cilium + Envoy L7 |
| Hardware isolation | None | Confidential Containers — optional add-on |
| Identity | API keys | Managed Identity + Workload Identity |
| Secrets | Env vars | Key Vault CSI |
| Supply chain | Digest verification | Notation + Ratify + SBOM |
| Compliance | Manual | azure-osconfig + CIS AKS Optimized (TODO) |
| Runtime observability | TUI + logs | Inspektor Gadget (eBPF) + Azure Monitor |
| Scale | Single node | Multi-node, multi-region |
