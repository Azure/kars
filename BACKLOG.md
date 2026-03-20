# AzureClaw — Backlog

> Honest assessment of what's implemented, what's broken, and what's missing.
> Last updated: March 20, 2026

## Status: Alpha

AzureClaw is functional for demo and development purposes. The core architecture (Rust controller + inference router, AKS deployment, Foundry integration, iptables egress-guard) is solid. The gaps below must be addressed before any public release.

---

## What Works (verified E2E)

| Capability | Status | Notes |
|-----------|--------|-------|
| `azureclaw up` — full AKS deployment | Working | 13-step pipeline: Bicep, Helm, ACR, Kata, WI, federated creds |
| `azureclaw dev` — local Docker sandbox | Working | Azure Linux 3, seccomp, read-only rootfs, inference router |
| `azureclaw connect` — shell into sandbox | Working | Docker + AKS, Kata fallback |
| `azureclaw destroy` — teardown | Working | Single sandbox + full resource group + soft-delete purge |
| `azureclaw model set/get/list` — model switching | Working | CRD patch + deployment env update |
| `azureclaw policy allow/get` — hot-reload egress | Working | CRD merge patch, controller reconciles NetworkPolicy |
| `azureclaw trace` — eBPF tracing | Working | kubectl gadget integration (exec, network, files, dns) |
| `azureclaw approve --list/--approve/--deny` | Working | ConfigMap-based workflow |
| `azureclaw status` — health dashboard | Working | Docker + AKS, Prometheus metrics parsing |
| `azureclaw logs` — log streaming | Working | kubectl logs wrapper |
| `azureclaw onboard` — credential wizard | Working | Verifies against Azure OpenAI API |
| Rust inference router — Foundry backend | Working | `/openai/v1/` path, IMDS auth, Content Safety, Prompt Shields |
| Rust inference router — AOAI dev backend | Working | `/openai/deployments/` path, API key auth |
| Rust controller — CRD reconciliation | Working | Namespace, SA, NetworkPolicy, Deployment, egress-guard init |
| Token budgets (daily + per-request) | Working | 429 enforcement, Prometheus metrics |
| Content Safety + Prompt Shields | Working | Calls Azure AI Content Safety API, blocks on detection |
| iptables egress-guard (UID-based) | Working | Agent UID 1000 restricted to localhost + DNS |
| Three isolation levels | Working | standard (runc), enhanced (seccomp), confidential (Kata VM) |
| seccomp profile DaemonSet | Working | Installs azureclaw-strict to all nodes |
| Namespace isolation | Working | Per-sandbox namespace with default-deny NetworkPolicy |
| Graceful shutdown (SIGTERM) | Working | Inference router handles SIGTERM/SIGINT |
| Prometheus metrics | Working | 3 metric families: requests, latency, tokens |
| OpenClaw plugin | Working | Slash commands, model provider registration |

---

## Broken / Incomplete (must fix before public)

### P0 — Blocks public release

| Item | Problem | Fix |
|------|---------|-----|
| **SSE streaming** | Inference router buffers entire response body. No streaming support. For chat UX, TTFT (time to first token) matters. Every serious inference proxy supports SSE streaming. | Implement `reqwest` streaming → axum `Body::from_stream()`. Medium effort (~2 days). |
| **`init` command is a stub** | 5 TODO comments, does nothing. Registered in CLI and visible to users. | Either implement it or remove it. `up` supersedes it. |
| **`launch` command is a stub** | Takes args, shows spinner, does nothing. | Either implement (kubectl apply ClawSandbox) or remove. |
| **`costs` command is fake** | Shows hardcoded mock data. No actual Azure Cost Management or Prometheus queries. | Either implement or remove the command entirely. |
| **`policy deny` is a stub** | `allow` works, `deny` has a TODO comment. | Implement: read allowedEndpoints, filter out the host, patch CRD. |
| **README install commands are aspirational** | `npm install -g @azure/azureclaw`, `brew install`, `winget install` — none of these packages exist. | Remove or replace with `git clone && cd cli && npm link`. |
| **README "Azure services just work" claim** | Claims agents can use Storage, Cosmos, AI Search via Managed Identity. No code implements this. The `azureServices` CRD field exists but the controller doesn't create role assignments for it. | Either implement or remove the claim. |
| **Approval flow not wired to controller** | CLI creates/reads ConfigMaps, but the controller doesn't watch ConfigMaps or auto-patch NetworkPolicy on approval. Approval is manual. | Add controller watch on ConfigMap annotations → NetworkPolicy reconciliation. |

### P1 — Should fix before public

| Item | Problem | Fix |
|------|---------|-----|
| **No tests** | Zero unit tests, zero integration tests, zero E2E tests for Rust or TypeScript. | Add cargo test for router (mock HTTP), controller (mock K8s). Add jest tests for CLI. |
| **`model set` causes pod restart** | `kubectl set env` triggers rollout. Not "instant" as documented. | Accept model from request body (already works with Foundry) or use ConfigMap watch. |
| **No rate limiting** | Inference router has no request-rate limiting. Token budgets exist, but a compromised agent could DoS the router with rapid requests. | Add tower rate-limit middleware to axum. |
| **Token cache is single-entry** | `WorkloadIdentityAuth` caches one token regardless of resource scope. If multiple Azure resources need different scopes, they'd overwrite each other. | Use `HashMap<scope, CachedToken>` instead of `Option<CachedToken>`. |
| **No input validation on CRD fields** | Controller trusts CRD spec fields (endpoint URLs, model names) without validation. Malformed values would cause confusing errors. | Add validation in reconciler before creating K8s resources. |
| **Helm values still reference AOAI fields** | `inferenceRouter.azure.openai.endpoint`, `inferenceRouter.azure.openai.deploymentName` — should be Foundry-centric. | Rename to `foundry.endpoint` / `foundry.model` (keep AOAI as `devMode.`). |

### P2 — Nice to have

| Item | Problem | Fix |
|------|---------|-----|
| **No health check for Content Safety** | If Content Safety endpoint is unreachable, requests fail open silently. No readiness signal. | Add Content Safety endpoint check to `/readyz`. |
| **`policy-engine/network/baseline.yaml` not used** | Exists on disk but no code reads it. The CRD's `networkPolicy.allowedEndpoints` is the actual policy source. | Delete or wire it as a default template. |
| **`CONTRIBUTING.md` references missing tooling** | Mentions `make test`, `make lint` but no Makefile exists. | Add Makefile or update CONTRIBUTING.md. |
| **No container image tags** | Helm defaults to `0.1.0` but actual builds use `latest`. | Implement proper versioning in CI. |

---

## Not Started (future roadmap)

| Item | Priority | Effort | Notes |
|------|----------|--------|-------|
| **SSE streaming in inference router** | High | Medium | Critical for chat UX. Requires axum streaming response. |
| **Image signing (Notation + Ratify)** | Medium | Medium | Guide exists in docs but no CI integration. |
| **SBOM generation** | Medium | Small | `cosign attach sbom` in CI pipeline. |
| **Node compliance (azure-osconfig)** | Low | Large | Requires integration with azure-osconfig team. |
| **Multi-region AKS** | Low | Large | Requires cross-region state sync. |
| **Public package distribution** | Medium | Medium | npm, Homebrew tap, MCR for container images. |
| **Documentation site** | Medium | Small | GitHub Pages or Docusaurus. |
| **Alerting** | Medium | Medium | Azure Monitor alerts for token spikes, egress anomalies. |
| **E2E test suite** | High | Large | Kind cluster + mock Azure endpoints. |

---

## Architecture Decisions (ADRs)

| Decision | Rationale | Date |
|----------|-----------|------|
| Foundry as primary inference backend | Unified `/openai/v1/` API, 200+ models, IMDS auth bypasses CA policy | March 2026 |
| iptables egress-guard over Envoy sidecar | Agent container restricted to localhost + DNS. Strictly stronger than L7 proxy since agent can't reach ANY external host. | March 2026 |
| IMDS over Workload Identity for prod auth | Bypasses Conditional Access Token Protection policies. Kubelet MI available without federation. | March 2026 |
| Azure Linux 3 (not 4) for container base | AL4 is alpha with limited access. AL3 is GA with full MCR availability. | March 2026 |
| Per-pod sidecar router (not shared service) | Each sandbox gets its own router instance. Simpler NetworkPolicy, token budgets per-pod, no cross-tenant blast radius. | March 2026 |
| Baseline PodSecurity (not restricted) | egress-guard init container needs NET_ADMIN. Audit+warn remain restricted. | March 2026 |
| No custom SELinux types | Incompatible with restricted PodSecurity enforcement. seccomp + iptables + Kata provide equivalent containment. | March 2026 |
| No Envoy L7 sidecar | Superseded by iptables UID-based egress. Agent can only reach localhost:8443. Nothing for Envoy to filter. | March 2026 |
