# AzureClaw — Backlog

> Last updated: March 20, 2026 · Status: **Alpha**

---

## Implemented

| Area | What works |
|------|-----------|
| **CLI** | 12 commands: `up`, `add`, `dev`, `connect`, `status`, `logs`, `model`, `trace`, `policy`, `approve`, `onboard`, `destroy`. All fully implemented. |
| **Controller** | Rust/kube-rs operator. Reconciles ClawSandbox CRDs → namespaces, pods, NetworkPolicies, iptables init containers. 3 isolation levels (standard/enhanced/confidential). 9 unit tests. |
| **Inference Router** | Rust/axum sidecar. Foundry (prod) + AOAI (dev) dual-mode. SSE streaming. Content Safety + Prompt Shields (fail-open). Token budgets (daily + per-request, 429). IMDS/WI auth with per-scope caching. Foundry Agent API proxy (`/agents/*`). Concurrency limit (64). 5 unit tests. |
| **Infrastructure** | Bicep: AKS (Azure Linux, Cilium, WI), ACR (Premium), KV, AOAI, Monitor. Helm: CRD, controller, RBAC, seccomp DaemonSet. Deployment hardened against soft-delete, Helm stale releases, IP drift, PodSecurity conflicts. |
| **Security** | 7-layer defense-in-depth. seccomp (custom strict profile). iptables UID-based egress. Default-deny NetworkPolicy. Kata VM (confidential). Read-only rootfs, non-root, drop ALL. |
| **CI/CD** | `ci.yml`: Rust (fmt, clippy, build, test), CLI (typecheck, lint, build), Bicep validate, Helm lint, Trivy scan. `image-sign-sbom.yml`: Notation signing + Syft SBOM on tag. |
| **Build** | Makefile with 11 targets. Container images versioned (`VERSION-GIT_SHA`). Azure Linux 3 base. |
| **Plugin** | OpenClaw provider registration (7 models), slash commands (/azureclaw status, agents, memory). |
| **Metrics** | Prometheus: `inference_requests`, `inference_latency`, `tokens_used` (per sandbox, model, direction). |

---

## Roadmap

| Item | Priority | Notes |
|------|----------|-------|
| E2E test suite | High | Kind-based framework exists (`tests/e2e/`). Needs mock Azure services. |
| CLI unit tests | Medium | vitest not yet configured. |
| `azureServices` CRD wiring | Medium | Schema exists. Controller does not yet create RBAC bindings for declared services (Storage, AI Search, etc). |
| Image signing enforcement | Medium | Notation signing in CI. Ratify admission guide exists. Not auto-deployed by `azureclaw up`. |
| Azure Monitor alerting | Medium | Token spikes, egress anomalies. KQL queries exist in `deploy/monitoring/dashboards.md`. |
| Node compliance | Low | azure-osconfig for CIS AKS benchmarks. |
| Multi-region AKS | Low | Cross-region state sync. |
| Documentation site | Low | GitHub Pages or Docusaurus. |

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
