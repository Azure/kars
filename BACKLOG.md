# AzureClaw — Backlog

> Last updated: July 2026 · Status: **Alpha**

---

## Implemented

| Area | What works |
|------|-----------|
| **CLI** | 15+ commands: `up`, `dev`, `add`, `destroy`, `connect`, `status`, `logs`, `credentials`, `credentials update`, `model set`, `policy`, `egress`, `trace`, `eval`, `push`, `list`. `add` supports `--governance`, `--trust-threshold`, `--policy-profile`, `--agent-tools`, `--channels`, channel token flags, and plugin API key flags. |
| **Channels** | Telegram, Slack, Discord, WhatsApp — enabled via CLI flags (e.g., `--channels telegram --telegram-token <token>`). Entrypoint auto-configures `plugins.allow` + `plugins.entries` + `channels.*` from env vars. |
| **Third-party Plugins** | Brave, Tavily, Exa, Firecrawl, Perplexity, OpenAI — enabled via API key flags (e.g., `--brave-api-key <key>`). Auto-registered in `plugins.allow` + `plugins.entries` when env var is present. |
| **Foundry Web Search** | Bing Grounding auto-discovered via `/connections` API. Uses full resource ID for the Bing connection. No manual config needed when a Bing Grounding resource is connected to the Foundry project. |
| **Controller** | Rust/kube-rs operator. Reconciles ClawSandbox → NS, SA, NetworkPolicy, Deployment, Service, ConfigMap. 3 isolation levels. AGT: Service (mesh DNS), ConfigMap (policy), volume mount, mesh ingress. `envFrom` credentials secret mounting (`<name>-credentials`, optional). Azure Services RBAC annotations. 9 unit tests. |
| **Inference Router** | Rust/axum per-pod proxy. 40+ routes, 18 Foundry API groups. SSE streaming. Content Safety + Prompt Shields. Token budgets (429). IMDS/WI auth. Auto-refreshing domain blocklist (OISD + URLhaus, 6h refresh). Native AGT governance: PolicyEngine, TrustStore, AuditLog, MeshInbox, Prometheus metrics, policy hot-reload. 9 `/agt/*` endpoints. |
| **Foundry Integration** | All services via Responses API — no hosted agents. 18 API groups E2E tested: memory_stores, agents, evaluators, evaluationrules, indexes, connections, deployments, datasets, insights, openai/*, knowledgebases, redTeams, schedules, evaluationtaxonomies. |
| **Foundry Skills** | 9 SKILL.md files: foundry-memory, foundry-code, foundry-knowledge, foundry-web-search, foundry-agents, foundry-conversations, foundry-evaluations, foundry-deployments, agt-governance. |
| **AGT Governance** | Opt-in via CRD or `--governance` flag. Router-integrated PolicyEngine, TrustStore (0-1000), AuditLog (hash-chain), MeshInbox. Inter-agent mesh via K8s DNS. Controller creates Service + ConfigMap + mesh ingress. E2E: 2 agents, 21/21 tests. |
| **Credentials Update** | `azureclaw credentials update <name>` — updates K8s secret (`<name>-credentials`) in-place and restarts pod. Supports all channel/plugin credential flags. |
| **Push** | `azureclaw push --only sandbox --apply` — builds and pushes images to ACR, then restarts deployments. Supports `--only` filter (controller, router, sandbox, relay, registry). |
| **Proxy Bootstrap** | Node.js 22 explicit proxy for `HTTPS_PROXY` — `proxy-bootstrap.js` ensures `fetch()` respects proxy settings (Node.js 22's built-in fetch ignores `HTTPS_PROXY`). |
| **Seccomp** | `azureclaw-strict` profile with inotify syscalls added (`inotify_init`, `inotify_init1`, `inotify_add_watch`, `inotify_rm_watch`) for file-watching tools. |
| **Security** | 8 layers validated on live AKS cluster: iptables egress guard, NetworkPolicy, seccomp, read-only rootfs, non-root, Content Safety, Prompt Shields, token budgets. Local dev iptables guard. Auto-refreshing domain blocklist (OISD + URLhaus feeds). |
| **E2E Encryption** | Signal Protocol (X3DH + Double Ratchet) proven with hex dumps. Inter-agent messages encrypted end-to-end via AgentMesh relay. Vendored SDK with 8 bug fixes. |
| **Foundry RBAC** | `azureclaw up` auto-configures: project MI → Azure AI User on RG, sandbox WI → Azure AI User on AI Services (via Bicep). |
| **Infrastructure** | Bicep: AKS, ACR, KV, AOAI, Monitor. Helm: CRD, controller, RBAC, seccomp DaemonSet. |
| **CI/CD** | ci.yml + image-sign-sbom.yml (Notation signing with Azure KV). blocklist-refresh.yml (daily seed auto-update from OISD + URLhaus). |
| **Plugin** | OpenClaw provider (7 models), 6 slash commands, 9 skills. |
| **Metrics** | Prometheus: `inference_requests`, `inference_latency`, `tokens_used`. |
| **E2E Tests** | infra-e2e.sh (22 pass), AGT multi-agent mesh (21 pass). |

---

## Roadmap

| Item | Priority | Notes |
|------|----------|-------|
| Image signing enforcement (Ratify) | Medium | Notation in CI. Ratify admission controller not auto-deployed yet. |
| Azure Monitor alerting | Medium | Token spikes, egress anomalies, syscall alerts. |
| Envoy L7 sidecar | Medium | HTTP method/path filtering for non-inference egress. |
| SBOM generation in CI | Medium | SPDX SBOM attached to images. |
| Node compliance (OS Config) | Low | azure-osconfig for CIS AKS Optimized benchmarks. |
| Behavioral anomaly detection | Low | Kill switch + SLO circuit breakers (AGT v2). |
| Multi-region AKS | Low | Cross-region state sync. |
| CLI unit tests (vitest) | Low | Framework configured, coverage TBD. |
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
