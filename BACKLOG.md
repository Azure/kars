# AzureClaw — Backlog

> Last updated: March 20, 2026 · Status: **Alpha**

---

## Implemented

| Area | What works |
|------|-----------|
| **CLI** | 12 commands: `up`, `add`, `dev`, `connect`, `status`, `logs`, `model`, `trace`, `policy`, `approve`, `onboard`, `destroy`. `add` supports `--agent-instructions`, `--agent-tools`. |
| **Controller** | Rust/kube-rs operator. Reconciles ClawSandbox CRDs → namespaces, pods, NetworkPolicies, iptables init containers. 3 isolation levels. Injects Foundry Agent ID + tools + AGT governance env vars from CRD. 9 unit tests. |
| **Inference Router** | Rust/axum sidecar. Foundry (prod) + AOAI (dev) dual-mode. SSE streaming. Content Safety + Prompt Shields. Token budgets (429). IMDS/WI auth. Foundry Agent API proxy (`/agents/*`). Concurrency limit (64). 5 unit tests. |
| **Foundry Skills** | 4 OpenClaw SKILL.md files: foundry-memory (threads), foundry-knowledge (file_search), foundry-web-search (web grounding), foundry-code (code_interpreter). Shipped via plugin. |
| **AGT Governance** | Opt-in via CRD `spec.governance`. Tool-level policy (shell-safety, destructive-approval, rate limits). No overlap with AzureClaw infra controls. AGT skill teaches agent about trust + policy. |
| **CRD** | `spec.agent` (instructions, tools, fileIds) + `spec.governance` (enabled, toolPolicy, trustThreshold) + `status.foundryAgentId` |
| **Infrastructure** | Bicep: AKS, ACR, KV, AOAI, Monitor. Helm: CRD, controller, RBAC, seccomp DaemonSet. |
| **Security** | 8-layer defense-in-depth (infra) + 2-layer AGT governance (behavioral) = 10/10 OWASP Agentic. |
| **CI/CD** | ci.yml + image-sign-sbom.yml. |
| **Plugin** | OpenClaw provider (7 models), 6 slash commands, 5 skills (4 Foundry + 1 AGT). |
| **Metrics** | Prometheus: `inference_requests`, `inference_latency`, `tokens_used`. |

---

## Roadmap

| Item | Priority | Notes |
|------|----------|-------|
| Controller-side Foundry agent creation | High | POST /agents on reconcile. Currently agent ID must be set externally. |
| `/mesh/*` inter-agent routes | Medium | IATP messaging between sandbox namespaces via router. |
| E2E test suite | High | Kind-based framework exists. Needs mock Azure services. |
| Foundry evaluation + prompt optimization | Medium | CLI-side `azureclaw eval` command. |
| CLI unit tests | Medium | vitest not yet configured. |
| `azureServices` CRD wiring | Medium | Schema exists. Controller does not yet create RBAC bindings. |
| Image signing enforcement | Medium | Notation in CI. Ratify not auto-deployed. |
| Azure Monitor alerting | Medium | Token spikes, egress anomalies. |
| AGT sidecar container | Low | Upgrade from in-process SDK for stronger isolation. |
| Multi-region AKS | Low | Cross-region state sync. |

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
