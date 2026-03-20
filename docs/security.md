# AzureClaw Security

AzureClaw implements defense-in-depth: eight infrastructure layers (always on) plus two behavioral governance layers (opt-in via AGT). Together: 10/10 OWASP Agentic Security Index coverage.

---

## Security Layers

### Layer 0: Azure Infrastructure
- AKS API server restricted to authorized IP ranges
- Network Security Groups on AKS subnet
- Azure DDoS Protection (platform-level)
- ACR Premium with content trust and network rules

### Layer 1: Node OS (Azure Linux)
- AKS nodes run Azure Linux (default AKS node OS)
- SELinux in enforcing mode
- Automatic security patch updates via node image upgrades
- No SSH access to nodes by default

### Layer 2: Kata VM Isolation (confidential level only)
- Each pod runs in a dedicated lightweight VM (Cloud Hypervisor) with its own kernel
- Container escape attacks are trapped inside the VM boundary
- Requires `--isolation confidential` and a dedicated Kata node pool (`katapool`)
- AKS preview feature: `KataVMIsolationPreview`

### Layer 3: Container Hardening
Applied to every sandbox pod:

| Control | Setting |
|---------|---------|
| Root filesystem | Read-only (`readOnlyRootFilesystem: true`) |
| User | Non-root (`runAsNonRoot: true`, UID 1000 for agent) |
| Privilege escalation | Blocked (`allowPrivilegeEscalation: false`) |
| Capabilities | All dropped (`drop: [ALL]`) |
| Writable paths | `/sandbox` and `/tmp` only (emptyDir volumes) |

### Layer 4: Kernel Confinement (seccomp)

| Isolation Level | seccomp Profile | Effect |
|-----------------|----------------|--------|
| standard | RuntimeDefault | Kernel's default syscall filter |
| enhanced (default) | Localhost `azureclaw-strict` | Custom strict allowlist (~150 syscalls). Blocks: mount, ptrace, bpf, unshare, setns, init_module, kexec_load, pivot_root, chroot, reboot, perf_event_open. |
| confidential | RuntimeDefault | Kata VM provides the isolation boundary |

The seccomp profile is installed on every node via a DaemonSet that writes `azureclaw-strict.json` to `/var/lib/kubelet/seccomp/profiles/`.

### Layer 5: Network Segmentation

**Three enforcement layers for network control:**

1. **iptables UID-based egress guard** (init container):
   - Agent (UID 1000): can only reach `localhost` + UDP port 53 (DNS). All other outbound traffic dropped.
   - Inference router (UID 1001): unrestricted within the pod's NetworkPolicy.
   - Effect: even if an agent exploits a vulnerability, it cannot make arbitrary network connections.

2. **Kubernetes NetworkPolicy** (per namespace):
   - Default-deny egress per sandbox namespace
   - Allowlist managed via `azureclaw policy allow/deny` (CRD merge patch → controller reconcile)
   - DNS (kube-dns) always allowed
   - IMDS (169.254.169.254) allowed for inference router only

3. **Inference-as-network-policy**:
   - The inference router is the sole egress path for AI model calls
   - Agent cannot bypass the router (iptables + NetworkPolicy + no credentials)

### Layer 6: Inference Safety

| Control | Service | Default |
|---------|---------|---------|
| Content filtering | Azure AI Content Safety (`text:analyze`) | On (fail-open) |
| Jailbreak detection | Prompt Shields | On (fail-open) |
| Token budgets | In-process enforcement | Per-sandbox daily + per-request limits, HTTP 429 |
| Audit | Prometheus metrics | Always on (requests, latency, tokens per sandbox) |

"Fail-open" means: if Content Safety is unreachable, inference proceeds. This prevents the safety service from becoming a denial-of-service vector.

---

## Identity & Access

| Principle | Implementation |
|-----------|----------------|
| Zero standing credentials | No API keys in images, env vars, or mounted secrets (AKS mode) |
| IMDS authentication | Inference router acquires tokens via Instance Metadata Service (kubelet Managed Identity) |
| Workload Identity fallback | Federated OIDC token exchange (projected SA token → Azure AD bearer) |
| Per-scope token caching | HashMap keyed by resource scope, auto-refresh on expiry |
| Credential isolation | Only UID 1001 (router) can reach IMDS — UID 1000 (agent) is blocked by iptables |

**Required Azure RBAC roles on the kubelet identity:**

| Role | Role Definition ID | Why |
|------|-------------------|-----|
| Cognitive Services User | `a97b65f3-24c7-4388-baec-2e87135dc908` | Content Safety API access |
| Cognitive Services OpenAI User | `5e0bd9bd-7b93-4f28-af87-19fc36ad61bd` | OpenAI inference API access |
| AcrPull | `7f951dda-4ed3-4680-a7ca-43fe172d538d` | Pull sandbox images from ACR |
| Key Vault Secrets User | `4633458b-17de-408a-b874-0445c86b69e6` | Read secrets from Key Vault |

---

## PodSecurity Standards

| Label | Value | Reason |
|-------|-------|--------|
| `pod-security.kubernetes.io/enforce` | `privileged` | egress-guard init container requires `NET_ADMIN` capability |
| `pod-security.kubernetes.io/audit` | `restricted` | Audit violations for post-init containers |
| `pod-security.kubernetes.io/warn` | `restricted` | Warn on violations |

The init container (egress-guard) runs as root with NET_ADMIN to install iptables rules, then exits. All runtime containers are non-root with all capabilities dropped.

---

## Comparison with NemoClaw

| Feature | NemoClaw | AzureClaw |
|---------|----------|-----------|
| Orchestration | K3s (single node) | AKS (multi-node, managed) |
| Container isolation | Docker | runc + Kata VM option |
| Kernel hardening | Landlock + seccomp | seccomp (custom Localhost profile) |
| Network control | Custom proxy | NetworkPolicy + iptables UID-based + inference-as-network-policy |
| Hardware isolation | None | Kata VM per pod (confidential level) |
| Identity | API keys | Managed Identity + Workload Identity (zero credentials) |
| Inference safety | None | Content Safety + Prompt Shields + token budgets |
| Observability | TUI + logs | Prometheus metrics + optional eBPF (Inspektor Gadget) |
| Scale | Single node | Multi-node AKS cluster, multi-tenant namespace isolation |
| AI models | NVIDIA (Nemotron) | Azure AI Foundry (200+ models) |

---

## Layer 7: Behavioral Governance — AGT (opt-in)

When `spec.governance.enabled: true`, the [Agent Governance Toolkit](https://github.com/microsoft/agent-governance-toolkit) adds application-layer governance that AzureClaw's infrastructure can't provide:

| Control | What it does | OWASP ASI |
|---------|-------------|-----------|
| **Tool-level policy** | Allow/deny per tool call before execution (<0.1ms). Shell safety, destructive approval. | ASI-02, ASI-04 |
| **Inter-agent trust** | Ed25519 DID identity per agent. Trust scoring 0-1000 with decay. | ASI-03, ASI-07 |
| **Tamper-evident audit** | Hash-chain append-only log of every action. | ASI-09 |
| **Kill switch** | Behavioral anomaly detection → agent termination. | ASI-10 |
| **Circuit breakers** | SLO enforcement, error budgets, automatic fallback. | ASI-08 |

### Overlap resolution

AGT does NOT duplicate AzureClaw infrastructure controls:

| Control | Owner | AGT defers to |
|---------|-------|---------------|
| Token budgets | AzureClaw Router | AGT reads, doesn't enforce |
| Content safety | AzureClaw Router (Azure AI) | AGT has NO content rules |
| Network restrictions | AzureClaw (iptables) | AGT has NO network rules |
| Filesystem scope | AzureClaw (read-only rootfs) | AGT has NO filesystem rules |
| Tool allow/deny | **AGT only** | Router can't see tool calls |

---

## OWASP Agentic Top 10 Coverage

| Risk | ASI | AzureClaw (infra) | AGT (behavioral) |
|------|-----|-------------------|-------------------|
| Agent Goal Hijacking | ASI-01 | Content Safety + Prompt Shields | Policy engine blocks unauthorized goals |
| Excessive Capabilities | ASI-02 | iptables + NetworkPolicy | Capability model (least-privilege) |
| Identity & Privilege Abuse | ASI-03 | Workload Identity (OIDC) | DID/Ed25519 agent identities |
| Uncontrolled Code Execution | ASI-04 | seccomp + Kata VM + read-only rootfs | Execution rings + sandboxing |
| Insecure Output Handling | ASI-05 | Content Safety + Prompt Shields | Content output policies |
| Memory Poisoning | ASI-06 | Content Safety pre-model | CMVK majority voting (AGT) |
| Unsafe Inter-Agent Comms | ASI-07 | NetworkPolicy | IATP encrypted channels + trust gates |
| Cascading Failures | ASI-08 | Token budgets + concurrency limit | Circuit breakers + SLOs |
| Human-Agent Trust | ASI-09 | Approve/deny workflow | Audit trails + flight recorder |
| Rogue Agents | ASI-10 | eBPF tracing + iptables kill | Behavioral anomaly + kill switch |

---

## Roadmap

Not yet implemented:

| Feature | Notes |
|---------|-------|
| Image signing enforcement | Notation signing exists in CI. Ratify admission controller guide exists but not auto-deployed. |
| Node compliance | azure-osconfig for CIS AKS benchmarks |
| Azure Monitor alerting | Token spike and egress anomaly alerts |
| `/mesh/*` inter-agent routes | IATP messaging between sandbox namespaces via router |
