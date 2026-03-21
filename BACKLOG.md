# AzureClaw — Backlog

> Last updated: March 21, 2026 · Status: **Alpha**

---

## Implemented

| Area | What works |
|------|-----------|
| **CLI** | 13 commands: `up`, `add`, `dev`, `connect`, `status`, `logs`, `model`, `trace`, `policy`, `approve`, `eval`, `onboard`, `destroy`. `add` supports `--governance`, `--trust-threshold`, `--policy-profile`, `--agent-tools`. |
| **Controller** | Rust/kube-rs operator. Reconciles ClawSandbox → NS, SA, NetworkPolicy, Deployment, Service, ConfigMap. 3 isolation levels. AGT: Service (mesh DNS), ConfigMap (policy), volume mount, mesh ingress. Azure Services RBAC annotations. 9 unit tests. |
| **Inference Router** | Rust/axum sidecar. 40+ routes, 18 Foundry API groups. SSE streaming. Content Safety + Prompt Shields. Token budgets (429). IMDS/WI auth. Auto-refreshing domain blocklist (OISD + URLhaus, 6h refresh). AGT module: PolicyEngine, TrustStore, AuditLog, MeshInbox. 9 `/agt/*` endpoints. |
| **Foundry Integration** | All services via Responses API — no hosted agents. 18 API groups E2E tested: memory_stores, agents, evaluators, evaluationrules, indexes, connections, deployments, datasets, insights, openai/*, knowledgebases, redTeams, schedules, evaluationtaxonomies. |
| **Foundry Skills** | 9 SKILL.md files: foundry-memory, foundry-code, foundry-knowledge, foundry-web-search, foundry-agents, foundry-conversations, foundry-evaluations, foundry-deployments, agt-governance. |
| **AGT Governance** | Opt-in via CRD or `--governance` flag. Router-integrated PolicyEngine, TrustStore (0-1000), AuditLog (hash-chain), MeshInbox. Inter-agent mesh via K8s DNS. Controller creates Service + ConfigMap + mesh ingress. E2E: 2 agents, 21/21 tests. |
| **Foundry RBAC** | `azureclaw up` auto-configures: project MI → Azure AI User on RG, sandbox WI → Azure AI User on AI Services (via Bicep). |
| **Infrastructure** | Bicep: AKS, ACR, KV, AOAI, Monitor. Helm: CRD, controller, RBAC, seccomp DaemonSet. |
| **Security** | 8-layer defense-in-depth + AGT governance (opt-in). Local dev iptables guard. Auto-refreshing domain blocklist (OISD + URLhaus feeds, seed ConfigMap + CronJob + router background task). |
| **CI/CD** | ci.yml + image-sign-sbom.yml (Notation signing with Azure KV). blocklist-refresh.yml (daily seed auto-update from OISD + URLhaus). |
| **Plugin** | OpenClaw provider (7 models), 6 slash commands, 9 skills. |
| **Metrics** | Prometheus: `inference_requests`, `inference_latency`, `tokens_used`. |
| **E2E Tests** | infra-e2e.sh (22 pass), AGT multi-agent mesh (21 pass). |

---

## Roadmap

| Item | Priority | Notes |
|------|----------|-------|
| Image signing enforcement (Ratify) | Medium | Notation in CI. Ratify admission controller not auto-deployed. |
| Azure Monitor alerting | Medium | Token spikes, egress anomalies. |
| Node compliance (OS Config) | Low | azure-osconfig for CIS AKS benchmarks. |
| Behavioral anomaly detection | Low | Kill switch + SLO circuit breakers (AGT v2). |
| Multi-region AKS | Low | Cross-region state sync. |
| CLI unit tests (vitest) | Low | Framework not yet configured. |

---

## Architecture Decisions

| Decision | Rationale |
|----------|-----------|
| Foundry as primary inference backend | Unified `/openai/v1/` API, 200+ models, IMDS auth bypasses CA Token Protection |
| Foundry Agent API proxy (`/agents/*`) | Memory, threads, files, runs via Foundry — no custom Cosmos/Search needed |
| iptables egress-guard (UID-based) | Agent (UID 1000) restricted to localhost + DNS. Strictly stronger than proxy-only. |
| IMDS over Workload Identity for prod | Bypasses Conditional Access Token Protection policies |
| Per-pod sidecar router (not shared gateway) | No cross-tenant blast radius. Simple NetworkPolicy. |
| `enforce: privileged` PodSecurity | egress-guard init container needs NET_ADMIN. Audit+warn remain restricted. |
| seccomp + iptables + Kata (no custom SELinux) | Custom SELinux types incompatible with restricted PodSecurity |
| SSE streaming for chat completions | Direct pipe for low TTFT. Non-streaming buffered for budget tracking. |
| Concurrency limit (64) over rate limit | `ConcurrencyLimitLayer` is Clone-compatible with axum. Per-second limiting deferred. |
| Azure Linux 3 base images | AL3 is GA on MCR. |
| Rust edition 2024 / MSRV 1.88 | Latest stable language features |
