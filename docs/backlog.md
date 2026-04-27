# AzureClaw — Backlog

> Last updated: July 2026 · Status: **Alpha**

---

## Implemented

| Area | What works |
|------|-----------|
| **CLI** | 18 commands: `up`, `dev`, `add`, `destroy`, `connect`, `status`, `list`, `logs`, `credentials`, `model`, `policy`, `egress`, `trace`, `eval`, `push`, `operator`, `handoff`, `mesh`. Supports `--governance`, `--trust-threshold`, `--policy-profile`, `--agent-tools`, `--channels`, channel token flags, plugin API key flags, `--isolation`, `--learn-egress`, `--global-registry`. |
| **Operator** | `azureclaw operator` — live TUI dashboard with agent list, egress management, security panel, cluster health. Keyboard-driven: spawn, connect, approve/deny egress, switch model, enforce egress, delete agents. |
| **Handoff** | `azureclaw handoff <name> --to cloud\|local` — live-migrate agents between local Docker and AKS with sub-agent snapshotting, E2E encrypted workspace transfer, identity succession, and task resumption. Both CLI-driven and LLM-driven (webchat) orchestration paths. |
| **Channels** | Telegram, Slack, Discord, WhatsApp — enabled via CLI flags (e.g., `--channels telegram --telegram-token <token>`). Entrypoint auto-configures `plugins.allow` + `plugins.entries` + `channels.*` from env vars. |
| **Third-party Plugins** | Brave, Tavily, Exa, Firecrawl, Perplexity, OpenAI — enabled via API key flags (e.g., `--brave-api-key <key>`). Auto-registered in `plugins.allow` + `plugins.entries` when env var is present. |
| **Foundry Web Search** | Bing Grounding auto-discovered via `/connections` API. Uses full resource ID for the Bing connection. No manual config needed when a Bing Grounding resource is connected to the Foundry project. |
| **Controller** | Rust/kube-rs operator. Reconciles ClawSandbox → NS, SA, ClusterRoleBinding, gateway-token Secret, NetworkPolicy, Deployment, Service, ConfigMap. 3 isolation levels (standard/enhanced/confidential). Kata auto-provisioning. AGT: Service (mesh DNS), ConfigMap (policy), volume mount, mesh ingress. `envFrom` credentials secret mounting (`<name>-credentials`, optional). 74 unit tests. |
| **Inference Router** | Rust/axum per-pod proxy. 80+ routes, 18 Foundry API groups. SSE streaming. Content Safety + Prompt Shields (Foundry-side guardrails, annotations parsed from responses). Token budgets (429). IMDS/WI auth. Auto-refreshing domain blocklist (OISD + URLhaus, 6h refresh). Native AGT governance: PolicyEngine, TrustManager, AuditLogger, RateLimiter, BehaviorMonitor. Prometheus metrics, policy hot-reload. Egress proxy (blocklist + allowlist + learn mode). Sub-agent spawn. Handoff protocol (21 endpoints). 105 unit + 26 integration tests. |
| **Sub-Agent Spawning** | Agents create child agents via CRD (`/sandbox/spawn`). Full tool access via native delegation. Isolation inheritance (confidential cannot be downgraded). Workspace transfer via E2E mesh. |
| **Foundry Integration** | All services via Responses API — no hosted agents. 18 API groups: memory_stores, agents, evaluators, evaluationrules, indexes, connections, deployments, datasets, insights, openai/*, knowledgebases, redTeams, schedules, evaluationtaxonomies. |
| **Foundry Skills** | 10 SKILL.md files: foundry-memory, foundry-code, foundry-knowledge, foundry-web-search, foundry-agents, foundry-conversations, foundry-evaluations, foundry-deployments, agt-governance, azureclaw-spawn. |
| **AGT Governance** | Native Rust implementation in the router (not a sidecar). PolicyEngine (10 YAML rules, hot-reloaded), TrustManager (0-1000, ±200 clamp, Ed25519 signed), AuditLogger (SHA-256 Merkle chain), RateLimiter (500/sec global, 50/sec per-agent), BehaviorMonitor (burst/failure/denial detection). E2E encrypted mesh via Signal Protocol. |
| **E2E Encryption** | Signal Protocol (X3DH + Double Ratchet) via AgentMesh relay. KNOCK trust handshake. No plaintext fallback. Proven with hex dumps in `docs/e2e-encryption-proof.md`. Vendored SDK with 8 bug fixes. |
| **Credentials Update** | `azureclaw credentials update <name>` — updates K8s secret (`<name>-credentials`) in-place and restarts pod. Supports all channel/plugin credential flags. |
| **Push** | `azureclaw push --only sandbox --apply` — builds and pushes images to ACR, then restarts deployments. Supports `--only` filter (controller, router, sandbox, relay, registry). |
| **Proxy Bootstrap** | Node.js 22 explicit proxy for `HTTPS_PROXY` — `proxy-bootstrap.js` ensures `fetch()` respects proxy settings (Node.js 22's built-in fetch ignores `HTTPS_PROXY`). |
| **Seccomp** | `azureclaw-strict` profile: 219 allowed syscalls, 28 explicitly blocked. Includes inotify for file watching, fsync for reliable I/O. |
| **Security** | 9 defense-in-depth layers validated on live AKS: Azure infrastructure, Azure Linux, Kata VM, container hardening, seccomp, network segmentation, inference safety, AGT governance, E2E encrypted mesh. |
| **Foundry RBAC** | `azureclaw up` auto-configures: project MI → Azure AI User on RG, sandbox WI → Azure AI User on AI Services (via Bicep). |
| **Infrastructure** | Bicep: AKS, ACR, KV, AOAI, Monitor. Helm: CRD, controller, RBAC, seccomp DaemonSet. |
| **CI/CD** | ci.yml + image-sign-sbom.yml (Notation signing with Azure KV). blocklist-refresh.yml (daily seed auto-update from OISD + URLhaus). |
| **Plugin** | OpenClaw provider, 11 tools (spawn, spawn_status, spawn_list, spawn_destroy, mesh_send, mesh_inbox, mesh_transfer_file, discover, handoff_request, handoff_confirm, handoff_status), 10 skills. |
| **Metrics** | Prometheus: `inference_requests`, `inference_latency`, `tokens_used`, `agt_policy_evaluations_total`, `agt_eval_latency_seconds`, `agt_behavior_alerts_total`. |
| **Testing** | 205 Rust tests (74 controller + 105 router + 26 integration) + 207 CLI tests (vitest). |

---

## Roadmap

| Item | Priority | Notes |
|------|----------|-------|
| Image signing enforcement (Ratify) | Medium | Notation in CI. Ratify admission controller not auto-deployed yet. |
| Azure Monitor alerting | Medium | Token spikes, egress anomalies, syscall alerts. |
| SBOM generation in CI | Medium | SPDX SBOM attached to images. |
| Node compliance (OS Config) | Low | azure-osconfig for CIS AKS Optimized benchmarks. |
| Behavioral anomaly detection | Low | Kill switch + SLO circuit breakers. |
| Multi-region AKS | Low | Cross-region state sync. |
| `azureclaw migrate` | Low | Import existing OpenClaw installations. |
| Public npm / Helm distribution | Low | npm registry + Helm repo. |

---

## Architecture Decisions

| Decision | Rationale |
|----------|-----------|
| Foundry as primary inference backend | Unified `/openai/v1/` API, 200+ models, IMDS auth bypasses CA Token Protection |
| Foundry Agent API proxy (`/agents/*`) | Memory, threads, files, runs via Foundry — no custom Cosmos/Search needed |
| iptables egress-guard (UID-based) | Agent (UID 1000) restricted to localhost + DNS. Strictly stronger than proxy-only. |
| IMDS over Workload Identity for prod | Bypasses Conditional Access Token Protection policies |
| Per-pod router (not shared gateway) | No cross-tenant blast radius. Simple NetworkPolicy. |
| `enforce: privileged` PodSecurity | egress-guard init container needs NET_ADMIN. Audit+warn remain restricted. |
| seccomp + iptables + Kata (no custom SELinux) | Custom SELinux types incompatible with restricted PodSecurity |
| SSE streaming for chat completions | Direct pipe for low TTFT. Non-streaming buffered for budget tracking. |
| Concurrency limit (64) over rate limit | `ConcurrencyLimitLayer` is Clone-compatible with axum. Per-second limiting deferred. |
| Azure Linux 3 base images | AL3 is GA on MCR. |
| Rust edition 2024 / MSRV 1.88 | Latest stable language features |
