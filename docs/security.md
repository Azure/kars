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
- RuntimeClass: `kata-vm-isolation` (auto-created by AKS when provisioning a Kata nodepool)
- **Isolation inheritance**: sub-agents spawned by a confidential parent automatically inherit confidential isolation — downgrading is blocked
- Auto-provisioning: `azureclaw add --isolation confidential` will offer to create a Kata nodepool if none exists

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
| enhanced (default) | Localhost `azureclaw-strict` | Custom strict allowlist (175 syscalls). Blocks: mount, ptrace, bpf, unshare, setns, init_module, kexec_load, pivot_root, chroot, reboot, perf_event_open. |
| confidential | RuntimeDefault | Kata VM provides the isolation boundary |

The seccomp profile is installed on every node via a DaemonSet that writes `azureclaw-strict.json` to `/var/lib/kubelet/seccomp/profiles/`.

**inotify syscalls:** The profile allows `inotify_init`, `inotify_init1`, `inotify_add_watch`, and `inotify_rm_watch`. These are required by Node.js file watchers (used by OpenClaw for config reloading and plugin hot-reload). They are safe to allow — inotify operates only on files the process can already access (governed by filesystem permissions and read-only rootfs). It cannot be used for privilege escalation or sandbox escape.

**fsync/fdatasync/sync syscalls:** The profile allows `fsync`, `fdatasync`, and `sync`. These are required for reliable file I/O (SQLite WAL, Python package writes, etc.) and were previously missing — causing `EPERM` errors on AKS nodes with the strict profile. The policy-engine profile (~219 syscalls) already included them; the deploy profiles were updated to match.

**Explicit proxy bootstrap:** The sandbox uses `proxy-bootstrap.js`, preloaded via `NODE_OPTIONS="--require ..."` before any OpenClaw code executes. It sets undici's `EnvHttpProxyAgent` as the global fetch dispatcher, ensuring all outbound HTTP/HTTPS requests (Telegram polling, model pricing, plugin calls) honor `HTTPS_PROXY` and `NO_PROXY` environment variables. This provides a forward (explicit) proxy path complementing the iptables-based transparent proxy on port 8444.

### Layer 5: Network Segmentation

**Three enforcement layers for network control:**

1. **iptables UID-based egress guard** (init container):
   - Agent (UID 1000): can only reach `localhost` + DNS + reply packets (`ESTABLISHED,RELATED`). All other outbound traffic dropped.
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

4. **Domain blocklist + Egress proxy** (auto-refreshing):
   - Blocks known-malicious domains: malware C2, phishing, cryptojacking pools, reverse shell services
   - **Egress proxy** with allowlist, approval flow, and learn mode — see [egress-proxy.md](egress-proxy.md)
   - Seed file embedded in controller binary, mounted as ConfigMap (`/etc/azureclaw/blocklist/domains.txt`)
   - Router background task refreshes from [OISD](https://oisd.nl/) + [URLhaus](https://urlhaus.abuse.ch/) every 6h
   - K8s CronJob also refreshes the ConfigMap every 6h (defense-in-depth)
   - GitHub Actions daily cron keeps the seed file in the repo fresh (≤ 24h old)
   - High-risk TLDs blocked: `.tk`, `.ml`, `.ga`, `.cf`, `.gq` (>80% of phishing per APWG)
   - Bare IP addresses blocked (no DNS = suspicious)
   - Subdomain matching: if `evil.com` is blocked, `sub.evil.com` is too
   - Safe refresh: if all upstream feeds fail, previous entries are preserved (no wipe-on-failure)
   - Endpoints: `GET /blocklist/status`, `POST /blocklist/check`

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

## Layer 7: Behavioral Governance — AGT

When `spec.governance.enabled: true`, the [Agent Governance Toolkit](https://github.com/microsoft/agent-governance-toolkit) (`@agentmesh/sdk`) adds application-layer governance at two levels:

**Application Layer** (OpenClaw plugin, `@agentmesh/sdk` TypeScript SDK):

| Control | Implementation | Integration |
|---------|----------------|-------------|
| **Tool-level policy** | AGT `Policy` engine with allow/deny rules, OPA/Rego/Cedar support | Plugin evaluates before tool execution |
| **Trust scoring** | AGT `createTrustStore()` — Ed25519 identity, 0-1000 scale, 5 tiers | Plugin tracks agent trust |
| **Audit logging** | AGT `createAuditLogger()` — hash-chain, OWASP ASI compliance | Plugin logs all governance decisions |

**Infrastructure Layer** (Rust inference router, native implementation):

| Control | Implementation | API Endpoint |
|---------|----------------|--------------|
| **Trust-gated mesh** | KNOCK protocol with trust scoring, E2E encrypted via Signal Protocol | `GET /agt/relay` (WebSocket) |
| **Mesh inbox** | Cross-namespace message delivery + auto-response | Plugin `onMessage` handler |
| **Router-level policy** | YAML-based policy evaluation as defense-in-depth | `POST /agt/evaluate` |
| **Tamper-evident audit** | Hash-chain append-only log (SHA-256), integrity verification | `GET /agt/audit/verify` |

### What the controller creates for AGT

When governance is enabled, the controller (Step 4c) creates:
- **K8s Service** — `{name}:8443` for mesh DNS routing
- **ConfigMap** — `agt-policy-{profile}` with policy YAML, mounted at `/etc/agt/policies`
- **NetworkPolicy ingress** — allows ports 8443, 18789, 18791 from other sandbox namespaces
- **Env vars** — `AGT_GOVERNANCE_ENABLED`, `AGT_TRUST_THRESHOLD`, `AGT_MESH_NAMESPACE`, `AGT_POLICY_DIR`

### Overlap resolution

AGT does NOT duplicate AzureClaw infrastructure controls:

| Control | Owner | AGT defers to |
|---------|-------|---------------|
| Token budgets | AzureClaw Router | AGT reads, doesn't enforce |
| Content safety | AzureClaw Router (Azure AI) | AGT has NO content rules |
| Network restrictions | AzureClaw (iptables) | AGT has NO network rules |
| Filesystem scope | AzureClaw (read-only rootfs) | AGT has NO filesystem rules |
| Tool allow/deny | **AGT only** | Router can't see tool calls |

### Layer 8: E2E Encrypted Inter-Agent Communications

All inter-agent messages are encrypted end-to-end using the **Signal Protocol** via the AgentMesh SDK (`@agentmesh/sdk`). The relay server acts as a dumb routing pipe — it can see who is talking to whom (AMIDs) but **cannot read message content**.

**There is no plaintext fallback.** If E2E encryption fails (key exchange error, decrypt failure, session mismatch), the message is **rejected** — never delivered in cleartext. Decrypt failures are surfaced as `security_event` in the operator inbox with a trust penalty on the peer.

**E2E channel verification:** After the first successful X3DH + Double Ratchet decrypt per peer, the gateway emits:
```
✅ E2E encrypted channel UP — first verified peer: '<name>' (X3DH + Double Ratchet)
```
This log is the definitive proof that encryption is working end-to-end.

**Protocol stack:**

| Layer | Component | What it sees |
|-------|-----------|-------------|
| Application | OpenClaw agent | Plaintext messages |
| Encryption | AGT SDK (Signal Protocol) | Encrypts before send, decrypts on receive |
| Transport | WebSocket relay bridge | Opaque `encrypted_payload` + routing AMIDs |
| Relay | AgentMesh relay server | Same as transport — cannot decrypt |

**Signal Protocol features used:**
- **X3DH** (Extended Triple Diffie-Hellman) — Initial key agreement between agents
- **Double Ratchet** — Forward secrecy with per-message key rotation
- **Identity keys** — Each agent has a unique AMID derived from its Signal Protocol identity key
- **Signed prekeys** — 100 one-time prekeys uploaded at registration for async key exchange

**Message flow:**
1. Agent registers with registry (identity + 100 signed prekeys)
2. Agent connects to relay via WebSocket (AMID + session UUID)
3. Sender fetches recipient's prekeys from registry → X3DH key exchange
4. Sender sends KNOCK via relay (policy-gated session establishment)
5. Recipient evaluates KNOCK: trust score + spawner affinity + policy → accept or reject
6. If accepted: sender encrypts message with Double Ratchet → `encrypted_payload`
7. Relay routes by AMID, forwards opaque payload
8. Recipient decrypts with Double Ratchet → plaintext (or rejects on failure)
9. If KNOCK rejected: all messages from that peer are **blocked** (enforcement mode)

**KNOCK protocol enforcement:** When `AGT_TRUST_THRESHOLD > 0`, KNOCK enforcement is active — messages from peers without an accepted KNOCK session are **blocked** (not delivered). The KNOCK handler evaluates trust scores using a normalized scale:

- Registry reputation (0.0–1.0) is normalized to 0–1000 for threshold comparison
- **Spawner affinity bonus** (+200) is added for sub-agents that this parent spawned, providing headroom for known children while still allowing the score to be overridden if the registry flags the agent
- If the effective score (normalized + bonus) falls below the threshold, the KNOCK is rejected and all subsequent messages from that peer are blocked
- Blocked messages are surfaced as `⛔ MESSAGE BLOCKED` security events in the operator inbox

**Trust tiers** (registry-assigned at registration):

| Tier | Registry Score | Normalized (×1000) | With Spawner Bonus |
|------|---------------|--------------------|--------------------|
| Anonymous | 0.5 | 500 | 700 |
| Verified (Tier 1) | 0.6 | 600 | 800 |
| Organization (Tier 2) | 0.7 | 700 | 900 |

With the default `AGT_TRUST_THRESHOLD=500`, all tiers pass. Raising it to `600` would require either OAuth verification or spawner affinity for anonymous agents.

**Traffic capture proof:** A full hex-dump analysis of a live inter-agent exchange
is documented in [`docs/e2e-encryption-proof.md`](e2e-encryption-proof.md), showing that
the relay sees only encrypted payloads while endpoints see plaintext.

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
| Unsafe Inter-Agent Comms | ASI-07 | NetworkPolicy + E2E encryption | Signal Protocol encrypted relay + trust gates |
| Cascading Failures | ASI-08 | Token budgets + concurrency limit | Circuit breakers + SLOs |
| Human-Agent Trust | ASI-09 | Approve/deny workflow | Audit trails + flight recorder |
| Rogue Agents | ASI-10 | eBPF tracing + iptables kill | Behavioral anomaly + kill switch |

---

## Remaining Roadmap

> **Validation report:** All security layers have been validated on a live AKS cluster
> with captured evidence. See [`docs/security-validation.md`](security-validation.md).

| Feature | Status |
|---------|--------|
| Image signing enforcement | Notation signing in CI. Ratify admission controller not auto-deployed. |
| Node compliance | azure-osconfig for CIS AKS benchmarks (deferred) |
| Azure Monitor alerting | Token spike and egress anomaly alerts (planned) |
| Behavioral anomaly detection | Kill switch + SLO circuit breakers (planned for AGT v2) |
