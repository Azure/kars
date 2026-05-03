# AzureClaw Security

AzureClaw implements defense-in-depth: layered controls covering infrastructure (Layers 0–6), behavioural governance + protocol-layer controls (Layer 7), E2E encrypted communications (Layer 8), and engineering controls — CI gates, security-audit framework, conformance corpus, fedcred reaper — that gate every PR (Layer 9). PR #44 ships **26 vendored AgentMesh patches**, **75 security-audit docs**, **6 blocking CI gates**, **8 conformance specs**, and **5 cargo-fuzz targets**. See [`docs/threat-model.md`](threat-model.md) for the per-route auth-tier walkthrough and [`docs/security-mcp-top10.md`](security-mcp-top10.md) for the OWASP MCP Top 10 controls matrix.

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
| enhanced (default) | Localhost `azureclaw-strict` | Custom strict allowlist (219 allowed syscalls, 28 explicitly blocked). Blocks: mount, ptrace, bpf, unshare, setns, init_module, kexec_load, pivot_root, chroot, reboot, perf_event_open. |
| confidential | RuntimeDefault | Kata VM provides the isolation boundary |

The seccomp profile is installed on every node via a DaemonSet that writes `azureclaw-strict.json` to `/var/lib/kubelet/seccomp/profiles/`.

**inotify syscalls:** The profile allows `inotify_init`, `inotify_init1`, `inotify_add_watch`, and `inotify_rm_watch`. These are required by Node.js file watchers (used by OpenClaw for config reloading and plugin hot-reload). They are safe to allow — inotify operates only on files the process can already access (governed by filesystem permissions and read-only rootfs). It cannot be used for privilege escalation or sandbox escape.

**fsync/fdatasync/sync syscalls:** The profile allows `fsync`, `fdatasync`, and `sync`. These are required for reliable file I/O (SQLite WAL, Python package writes, etc.) and were previously missing — causing `EPERM` errors on AKS nodes with the strict profile. The canonical seccomp profile (~219 syscalls) already included them; the deploy profiles were updated to match.

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
| Content filtering | Foundry Guardrails (`Microsoft.DefaultV2`) | Always on (server-side) |
| Jailbreak detection | Prompt Shields (Foundry-side) | Always on (server-side) |
| Token budgets | In-process enforcement | Per-sandbox daily + per-request limits, HTTP 429 |
| Audit | Prometheus metrics | Always on (requests, latency, tokens per sandbox) |

"Foundry-side" means: Content Safety and Prompt Shields are applied by the Azure AI Foundry model deployment (`Microsoft.DefaultV2` guardrails). The router does not make separate Content Safety API calls — it parses `prompt_filter_results` annotations from model responses and reports detected flags to AGT governance for trust scoring and audit logging.

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

## Layer 7: Behavioral Governance — AGT (with Phase 1 protocol-layer controls)

When `spec.governance.enabled: true`, AGT governance runs **natively inside the Rust inference router** — no sidecar, no external process. The router implements PolicyEngine, TrustManager, AuditLogger, RateLimiter, and BehaviorMonitor as compiled-in Rust modules with <1µs evaluation latency. The OpenClaw plugin connects to the AGT relay (via `@agentmesh/sdk`) for E2E encrypted inter-agent messaging only — governance evaluation always goes through the router.

### Phase 1 protocol-layer controls

In addition to the legacy `/agt/evaluate` tool gate, the router enforces protocol-aware controls on the new MCP 2026 + A2A 1.0.0 ingress paths:

| Control | Implementation | Source |
|---|---|---|
| **OAuth 2.1 bearer verifier (RFC 8725 BCP)** | `tower::Layer` mounted on `/mcp`; PKCE / audience / expiry / `resource` indicator / scope checks. Gated by `McpServer.spec.productionMode: true`. | `inference-router/src/mcp/{oauth,oauth_layer}.rs` |
| **MCP Streamable HTTP framing** | JSON-RPC 2.0 strict mode; `Mcp-Session-Id` semantics; oversized-frame reject; batch validation. | `inference-router/src/mcp/{streamable_http,jsonrpc}.rs` |
| **A2A AgentCard signing** | Per-sandbox `/.well-known/agent.json` signed Ed25519 detached JWS via `SigningProvider`; inbound calls have signature, issuer, expiry verified. | `inference-router/src/a2a/{card_signing,card_verifier,trust_store}.rs` |
| **AP2 IntentMandate verify** | Detached-JWS mandate signature; `commerce.dailyCap` / `monthlyCap` / `counterpartyAllowlist` enforcement at `message/send`. | `inference-router/src/a2a/{ap2,mandate_signing,mandate_trust_store,message_send_ap2}.rs` |
| **Per-route fuzz coverage** | A2A JWS, A2A base64url, handoff state deserialize, chat sanitiser, streaming-PF parser. | `inference-router/fuzz/fuzz_targets/` |
| **OWASP MCP Top 10 (2025) controls matrix** | Per-control mapping to AzureClaw enforcement point. | [`docs/security-mcp-top10.md`](security-mcp-top10.md) |

### Four-seam provider architecture

Three of the four AGT contracts have in-tree implementations on the router-side `Governance` struct, each reachable via `Arc<dyn Trait>` views of the same `Arc<Governance>`:

| Contract | Trait file | In-tree impl | Migrated call sites |
|---|---|---|---|
| `PolicyDecisionProvider` | `providers/policy.rs` | `policy_impl.rs` (`impl … for Governance`) | `routes/inference.rs` (3 sites) |
| `AuditSink` | `providers/audit.rs` | `audit_impl.rs` | `handoff/mod.rs` (13 sites) |
| `SigningProvider` | `providers/signing.rs` | `signing_impl.rs` | A2A AgentCard + AP2 mandate signing |
| `MeshProvider` | `providers/mesh.rs` (doc-only) | **none — plugin-side by design** | Router has no in-tree mesh impl |

`providers/outage.rs` selects between `Strict` (prod default), `CachedRead`, `DegradedDev` per-`ClawSandbox` via `spec.agt.outageMode`.

### Layer 7 (legacy) — Behavioral Governance — AGT

When `spec.governance.enabled: true`, AGT governance runs **natively inside the Rust inference router** — no sidecar, no external process. The router implements PolicyEngine, TrustManager, AuditLogger, RateLimiter, and BehaviorMonitor as compiled-in Rust modules with <1µs evaluation latency. The OpenClaw plugin connects to the AGT relay (via `@agentmesh/sdk`) for E2E encrypted inter-agent messaging only — governance evaluation always goes through the router.

**Router (native Rust)** — all governance evaluation happens here:

| Control | Implementation | Integration |
|---------|----------------|-------------|
| **Tool-level policy** | `PolicyEngine` (YAML rules, hot-reloaded) gates `exec_command` and `http_fetch` pre-execution via `POST /agt/evaluate`; denies sensitive files, recon tools, cloud metadata, destructive commands | Plugin forwards to router before tool runs |
| **Trust scoring** | `TrustManager` — Ed25519 identity, 0-1000 scale, 5 tiers, clamped ±200/update | Per-agent trust tracked in router |
| **Audit logging** | `AuditLogger` — SHA-256 Merkle hash-chain, tamper detection, integrity verification | Append-only log in router |
| **Rate limiting** | `RateLimiter` — 500 req/sec global, 50/sec per-agent, token bucket with burst | In-process enforcement |
| **Behavior monitoring** | `BehaviorMonitor` — burst detection (100/60s), failure tracking (20), denial tracking (10/60s) | Alerts on anomalous patterns |

**Admin-plane hardening (cross-cutting, applies to every admin-gated route):**

| Control | Implementation | Source |
|---------|----------------|--------|
| **Constant-time admin-token compare** | `handoff::constant_time_eq` replaces all `==` compares for the admin token in `admin_auth_middleware`, `POST /agt/trust`, `DELETE /agt/trust/{id}`, `PUT /agt/rate-limit`, and the cross-pod bearer middleware in `main.rs`. Eliminates timing side channel on token validation. | `inference-router/src/handoff.rs`, `routes.rs:1788/1947/2008`, `main.rs:288` |
| **Strict deserialization** | `#[serde(deny_unknown_fields)]` on `SpawnRequest` and `HandoffMeta` — typo'd or unknown fields are rejected with HTTP 422 instead of silently defaulting. Other handlers take `Json<serde_json::Value>` and forward opaquely; the encryption / admin-token gate is the authoritative trust boundary for them. | `inference-router/src/spawn.rs:37/62` |
| **Optional source-IP allowlist** | `ROUTER_ADMIN_ALLOW_IPS` (CIDR list). When set, a leaked admin token is useless without a pod at an allowlisted address. Localhost is always allowed (same-pod agent path is governed by per-route `no_localhost_bypass` flags). | `main.rs:146,392` |
| **Browser-origin gate** | `ADMIN_ALLOWED_ORIGINS` rejects admin-route requests carrying a browser `Origin` header unless on the allowlist (default: empty). CLI/curl traffic is unaffected (no `Origin`). Closes cross-site abuse on a leaked token. | `inference-router/src/main.rs` |
| **Canonical bearer header** | `Authorization: Bearer <token>` is the canonical admin auth. The legacy `x-azureclaw-admin` is still accepted but emits a one-shot `warn!` per process and will be removed. | `main.rs:376` |
| **Handoff middlewares — no localhost bypass** | `handoff_init_auth_middleware` and `handoff_auth_middleware` require the admin token unconditionally; the same-pod agent (UID 1000) cannot forge a handoff even via 127.0.0.1. Read-only `/agt/handoff/status` and `/agt/handoff/sub-agents` keep localhost allowed for self-poll. | `inference-router/src/handoff.rs` |
| **Per-request size caps** | Default axum 2 MB body limit on inference; explicit `DefaultBodyLimit::max(MAX_BLOB_SIZE_BYTES)` (200 MB) on handoff snapshot/restore with post-decompress recheck. | `routes.rs:3012/3249/3498/3518` |
| **Trace-id sanitization** | Inbound `X-Azureclaw-Trace-Id` rejected if it contains CRLF / ANSI / path traversal; otherwise propagated to upstream Azure calls and stamped onto every audit-chain entry. | `main.rs:~440` (`is_safe_trace_id`) |
| **Test-only endpoint overrides — controller-set only** | `AZURE_IMDS_ENDPOINT` and `AZURE_AD_ENDPOINT` can redirect IMDS / AAD calls to test fakes. They are read once at startup from the controller-injected env; the threat model assumes anyone who can mutate the pod env has already escaped, so this is not a new attack surface. | `inference-router/src/auth.rs` |

For a per-route auth-tier and blast-radius walkthrough, see [`docs/threat-model.md`](threat-model.md).

**Mesh & Communication Layer** (E2E encryption via `@agentmesh/sdk`):

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

| Risk | ASI | AzureClaw (infra) | AGT (behavioral) | Status |
|------|-----|-------------------|-------------------|--------|
| Agent Goal Hijacking | ASI-01 | Foundry Guardrails (jailbreak + indirect attack detection) | Policy engine blocks unauthorized actions | ⚠️ Partial — action-level blocking, not semantic goal-drift detection |
| Excessive Capabilities | ASI-02 | iptables UID isolation + NetworkPolicy + seccomp (219 syscalls) | Capability allow/deny lists (least-privilege) | ✅ Strong |
| Identity & Privilege Abuse | ASI-03 | Workload Identity (OIDC token exchange) | Ed25519 keypairs + AMID derivation | ⚠️ Partial — identities generated but not verified on message receipt |
| Uncontrolled Code Execution | ASI-04 | seccomp + Kata VM + read-only rootfs + drop ALL caps | UID isolation (1000/1001) + non-root | ✅ Strong |
| Insecure Output Handling | ASI-05 | Foundry Guardrails (prompt-side only) | Output policy evaluation (log-only) | ⚠️ Partial — input filtered, output only logged not blocked |
| Memory Poisoning | ASI-06 | — | — | ❌ Not implemented — memory APIs proxy to Foundry without safety checks |
| Unsafe Inter-Agent Comms | ASI-07 | NetworkPolicy default-deny | Signal Protocol Double Ratchet E2E encryption | ✅ Strong |
| Cascading Failures | ASI-08 | Token budgets + concurrency semaphore (256) + rate limiter (500/s) | Behavior monitor thresholds | ⚠️ Partial — no circuit breaker, no SLO enforcement |
| Human-Agent Trust | ASI-09 | RequiresApproval policy decision type | Audit logging (every policy decision) | ⚠️ Partial — audit works, but no approval UI/workflow |
| Rogue Agents | ASI-10 | iptables network kill + `azureclaw destroy` | BehaviorMonitor (burst/failure/denial detection) | ⚠️ Partial — anomaly detected but no automated response |

**Legend:** ✅ Strong = implemented and tested | ⚠️ Partial = core infra works, behavioral gaps remain | ❌ = not implemented

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

## Layer 9: Global Registry & Handoff Security

Cross-environment agent handoff (local ↔ cloud) introduces new attack surface. Mitigations:

### Relay Authentication (4 layers)

| Layer | Control | Purpose |
|-------|---------|---------|
| WAF (AppGW) | Rate limiting, DDoS protection | Network-level flood prevention |
| Ed25519 signature | `verify_connection_signature()` | Proves AMID ownership (replay-protected via timestamp) |
| Registry lookup | `RegistryVerifier.verify_registered()` | Confirms agent is registered + not revoked |
| OAuth | GitHub / Entra ID / Google | Controls who can register in the first place |

Relay fails open on registry 5xx (avoids cascading failures) but rejects on 404 (unregistered).

### Handoff Protocol Security

| Threat | Mitigation |
|--------|------------|
| LLM-initiated handoff | Two-stage confirmation gate: router token + 3s human delay + rate limit (1/5min) |
| Prompt injection in state blob | `sanitize_chat_snapshot()` strips 17 injection patterns |
| Trust score inflation | Scores capped at 750 on restore (cannot import max trust) |
| State blob DoS | 50MB blob cap, 100 file limit, 10MB/file |
| Succession flooding | DB-backed rate limits: succession 1/5min, reclamation 1/hour per AMID |
| Identity impersonation | Ed25519 succession signatures with canonical messages |
| Downgrade attack | One-shot succession (unique index), co-signature reclamation |
| AGT policy bypass | `handoff-tool-approval` rule at priority 75 (above tool-allow at 70) |

### NetworkPolicy (public mode)

```
PostgreSQL ← only registry pods (port 5432)
Registry   ← only AppGW subnet + internal sandbox pods (port 8080)
Relay      ← only AppGW subnet + internal sandbox pods (port 8765)
```

### Identity at Rest

Mesh identity stored in `~/.azureclaw/mesh-identity.json`:
- Private key encrypted with AES-256-GCM
- Encryption key derived from machine-specific seed (hostname + homedir)
- File permissions restricted to owner (0600), directory (0700)

---

## Upstream Alignment

AzureClaw governs the agent runtime **without forking OpenClaw.** The native `sessions_spawn` / `sessions_send` tools are denied via upstream's own `tools.deny` config, and governance-aware replacements (`cloud_offload`, `azureclaw_spawn`, `mesh_send`, …) are registered through the upstream `api.registerTool()` plugin API. The `vendor/` directory contains only AgentMesh forks — there is **no OpenClaw fork**.

See [Upstream Alignment](upstream-alignment.md) for the full rationale and file-level references.

The operator-facing TypeScript CLI and the OpenClaw plugin run *outside* the sandbox boundary (on the operator's workstation or in the sandbox's Node process), so they have their own hardening surface independent of the AKS controls above. CodeQL findings on this surface are tracked and closed as code-scanning alerts.

| Control | Where | What it catches |
|---------|-------|-----------------|
| **`redactSecrets()` log filter** | `cli/src/plugin.ts:165` (wraps `_log.info` / `_log.warn`) | Bearer / Basic auth headers, JWTs (`eyJ…`), generic `<keyword>: <value>` secrets (api_key, access_token, refresh_token, handoff_token, admin_token, pairing_token, invite_code, authorization, …), AzureClaw `azcp_<n>_…` one-time pairing tokens, and full PEM key blocks are redacted before reaching `console.log`. ReDoS-bounded character classes (`{1,40}`, `{0,8192}?`) on the PEM regex. Covered by 9 unit tests in `cli/src/redact.test.ts`. |
| **`sanitizeForLog()`** | `cli/src/stepper.ts:158`, `cli/src/commands/mesh.ts:179` | Strips CR/LF/tab from untrusted strings before they reach `console.log`/`console.error`, blocking log-injection payloads (CWE-117). Uses the classic split `.replace` pattern that CodeQL's JS query models as a sanitizer. |
| **`escapeHtml()` on OAuth callback** | `cli/src/commands/mesh.ts:168/209` | The local OAuth-callback HTML response HTML-encodes the upstream error string before injecting into `<p>…</p>`, blocking reflected-XSS in `azureclaw mesh login` browser flows. |
| **Per-request tmpdir** | `cli/src/plugin.ts:1513–1520` | `fs.mkdtempSync(os.tmpdir() + "/offload-")` replaces the predictable `/tmp/.offload-start-<requestId>` marker (CWE-377 insecure temporary file). The session banner-dedup marker moved from `/tmp/.azureclaw-banner-printed` to user-private `~/.cache/azureclaw/banner-printed` (mode 0700/0600). |
| **TOCTOU-safe file reads** | `cli/src/plugin.ts:1650, 5440`, `mesh-plugin/src/connection.ts:634` | `fs.statSync(...)` → `fs.readFileSync(...)` was a CWE-367 race window. Replaced by `openSync` + `fstatSync` + `readSync` so size check and read happen on the **same fd**. |
| **No-shell command exec** | `cli/src/plugin.ts:1573–1594` | The "find newly-created files since offload start" call replaces `execSync("find … | head -n 50")` with `execFileSync("find", [...args])`. The shell is removed entirely, the `head` cap is enforced via `.slice(0, 50)` in JS. Closes CWE-78 indirect command-line injection. |

### Sandbox entrypoint hardening

| Control | Where | Why |
|---------|-------|-----|
| **EPERM-tolerant `fchmod`/`chmod`** | `sandbox-images/openclaw/entrypoint.sh` | Some Docker Desktop + virtiofs combinations return EPERM on `fchmod(2)` against the sandbox volume even when content writes succeed. All `cp`/`chmod`/`chown` calls now use `|| true` so transient mode-bit failures don't kill the script under `set -e`. The hardening block re-applies modes later. |
| **Policy profile leak fix** | `sandbox-images/openclaw/entrypoint.sh` | The router unions rules from every `*.yaml` in `AGT_POLICY_DIR`, so copying *all* profiles let the offload `no-spawn` deny leak into `default`. Entrypoint now copies only `azureclaw-${AGT_POLICY_PROFILE}.yaml` (with `default` fallback) and clears stale YAMLs first. |
| **Plugin / SDK / node_modules read-only** | `sandbox-images/openclaw/entrypoint.sh:598–630`, `Dockerfile:65–67` | After the blanket `chown -R sandbox:sandbox /sandbox`, plugin code, vendored SDK, `node_modules`, and skills are re-`chown`'d back to root and made read-only. UID 1000 can read but not modify its own runtime. Asserted by a controller-side reconciler regression test (every hardening invariant — UID, RO rootfs, drop ALL caps, seccomp profile, NET_ADMIN drop after init, iptables egress-guard, plugin+SDK ownership). |

### Supply-chain & dependency hygiene

| Control | Where | What it catches |
|---------|-------|-----------------|
| **`cargo audit` CI job** | `.github/workflows/ci.yml` (`continue-on-error: true` pending triage cadence) | Caught RUSTSEC-2026-0098 / -0099 / -0104 (cert-name-constraint flaws in the `rustls-webpki` chain used by `reqwest`, `hyper-rustls`, `kube-client`, `tokio-tungstenite`); closed by bumping `rustls-webpki` 0.103.10 → 0.103.13. Remaining audit warning is a transitive `rand 0.8.5` soundness note via upstream `agentmesh 3.1.0`, requires upstream bump. |
| **npm overrides for vulnerable transitives** | `cli/package.json` | Pins `uuid` ≥14.0.0 (GHSA-w5hq-g745-h8pq, via `@azure/msal-node`), `xml2js` ≥0.6.2 (prototype pollution via `blessed-contrib/map-canvas`), `lodash` ≥4.17.24 (GHSA-r5fr-rjxr-66jc `_.template` CVE). `npm audit` clean. |
| **Vendored Python wheel bumps** | `sandbox-images/openclaw/Dockerfile.base` | `lxml` 6.0.2 → 6.1.0, `pillow` 12.1.1 → 12.2.0, `pypdf` 6.9.2 → 6.10.2, `cryptography` 46.0.6 → 46.0.7. |
| **Sandbox Go toolchain** | `sandbox-images/openclaw/Dockerfile.base` | `golang:1.23-alpine` → `1.24-alpine` to pick up Go stdlib patches for the bundled CLIs. |
| **Vendored AgentMesh `Cargo.lock` bumps** | `vendor/agentmesh-{relay,registry}/Cargo.lock` | `openssl` 0.10.76 → 0.10.78 (GHSA-hppc-g8h3-xhp3), `tokio` 1.50 → 1.52.1, `mio` 1.1.1 → 1.2.0. |
| **Fuzz + proptest coverage** | `cargo +nightly fuzz` | Targets: handoff blob parser, blocklist domain parser, AGT policy evaluator, safety-response parser. `proptest`: handoff-chunking, Double-Ratchet state transitions, K8s name validation. |
| **Vendored AgentMesh patch index** | `docs/agt-vendored-patch-audit.md` + `ci/vendored-patch-audit.sh` | **26 patches** (SDK 21 + relay 4 + registry 1) tracked with reasons; CI gate forces re-audit on every AGT SDK pin bump (catches "patch quietly absorbed upstream — drop ours"). |

---

## Layer 9: Engineering controls (PR #44)

The following controls live above any individual layer — they apply to **every PR** and gate the merge.

### Six blocking CI gates

| Gate | Path | Enforces |
|---|---|---|
| LOC budget | `ci/check-loc.sh` + `ci/loc-budget.yaml` | 800-line hard cap on new files; monotonic-decrease budget on hotspots |
| Anti-stub | `ci/no-stubs.sh` | No `TODO/FIXME/unimplemented!/todo!/panic!("not impl")` on production paths |
| No custom crypto | `ci/no-custom-crypto.sh` | Forbids hand-rolled crypto (`sha2::`, `hmac::`, `curve25519_dalek::`, …) outside `providers/signing.rs` + `providers/mesh.rs` (vendored path only) + `vendor/agentmesh-sdk/` |
| No `Null*` provider in prod | `ci/no-null-provider-prod.sh` | Static + admission mirror — `provider: null/noop/disabled` requires `azureclaw.azure.com/dev-only: "true"` label |
| Security audit required | `ci/security-audit-required.sh` | If PR touches CRDs / reconcilers / admission / router providers / MCP / A2A / CLI commands / sandbox image — requires a matching `docs/security-audits/<date>-<slug>.md` with two distinct sign-offs |
| Vendored patch audit | `ci/vendored-patch-audit.sh` | If AGT SDK pin bumped — requires `docs/agt-vendored-patch-audit.md` re-confirmation row per patch |
| A2A module isolation | `ci/a2a-module-isolation.sh` | Keeps the A2A 1.0.0 module surface from leaking back into `routes/` |

### Security-audit framework

Every capability-introducing PR carries `docs/security-audits/YYYY-MM-DD-<slug>.md` from the `_template.md` shape: threat-model delta, OWASP MCP/LLM mapping, AuthN/Z path, secret + key custody, egress-surface delta, audit events emitted, failure mode (fail-closed default), negative-test coverage, vendored / 3rd-party dependency delta, two sign-offs from named reviewers. See `docs/security-audits/` for the full archive of per-capability sign-offs.

### Conformance corpus

`tests/conformance/` ships **8 specs** with mandatory negative cases:
`signal-x3dh`, `signal-knock`, `signal-negative` (tampered ciphertext, replayed message, missing prekey signature), `oauth21-bcp`, `mcp-streamable-http`, `a2a-agent-card` (wrong-issuer, expired-exp, missing-fields), `ap2-commerce` (cap exceeded, counterparty not in allowlist), `sandbox-isolation`.

### Federated-credential reaper

The controller's 4th `tokio::select!` arm (`controller/src/fedcred_reaper.rs`, 232 LOC, 5 unit tests) periodically GCs orphan federated credentials so a sandbox WI MI never hits the **20-fedcred-per-MI Azure cap**. Default 600 s; `FEDCRED_REAPER_INTERVAL_SECS` env override.
