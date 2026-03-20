# AzureClaw — Backlog

> Last updated: March 20, 2026

## Status: Alpha

Core architecture is solid and verified. All P0/P1/P2 code issues resolved. Ready for internal review before public release.

---

## What Works (verified)

- `azureclaw up` — full AKS deployment (Bicep, Helm, ACR, Kata, WI, federated creds)
- `azureclaw dev` — local Docker sandbox with security + inference router
- `azureclaw connect/destroy/status/logs/onboard` — all functional
- `azureclaw model set/get/list` — CRD patch + deployment env update
- `azureclaw policy allow/get/deny` — hot-reload via CRD merge patch
- `azureclaw trace` — kubectl gadget eBPF tracing
- `azureclaw approve --list/--approve/--deny` — ConfigMap-based workflow
- Rust inference router — Foundry (prod) + Azure OpenAI (dev) dual-mode
- SSE streaming for chat/completions when `stream: true`
- Content Safety + Prompt Shields (on by default, fail-open)
- Token budgets (daily + per-request, 429 enforcement)
- iptables egress-guard (UID-based per-container network isolation)
- Three isolation levels (standard/enhanced/confidential with Kata VM)
- seccomp profile DaemonSet, namespace isolation, graceful shutdown
- Concurrency rate limiting (64 concurrent requests max)
- Per-scope token cache (HashMap keyed by resource)
- CRD input validation (isolation level, model, endpoint)
- Content Safety endpoint readiness check
- Prometheus metrics (requests, latency, tokens)
- OpenClaw plugin with slash commands
- 5 unit tests (budget tracker)

---

## Remaining Work

### Before Public Release

| Item | Effort | Notes |
|------|--------|-------|
| Scrub internal Microsoft links from PLAN.md | Small | PLAN.md is gitignored but still on disk — contains eng.ms links, subscription IDs |
| Container image versioning | Small | Helm defaults to `0.1.0`, CI should tag images properly |
| Public package distribution | Medium | npm publish, Homebrew tap, MCR for images |

### Future Roadmap

| Item | Priority | Effort | Notes |
|------|----------|--------|-------|
| Image signing (Notation + Ratify) | Medium | Medium | Guide exists, needs CI integration |
| SBOM generation | Medium | Small | `cosign attach sbom` in CI |
| Node compliance (azure-osconfig) | Low | Large | Requires external team |
| Multi-region AKS deployment | Low | Large | Cross-region state sync |
| Documentation site | Medium | Small | GitHub Pages or Docusaurus |
| Alerting (Azure Monitor) | Medium | Medium | Token spikes, egress anomalies |
| E2E test suite | High | Large | Kind cluster + mock Azure |
| Controller unit tests | Medium | Medium | Mock K8s client via kube-rs |

---

## Architecture Decisions

| Decision | Rationale | Date |
|----------|-----------|------|
| Foundry as primary inference backend | Unified `/openai/v1/` API, 200+ models, IMDS auth bypasses CA policy | March 2026 |
| iptables egress-guard over Envoy | Agent (UID 1000) restricted to localhost + DNS. Strictly stronger than L7 proxy. | March 2026 |
| IMDS over WI for prod auth | Bypasses Conditional Access Token Protection policies | March 2026 |
| Azure Linux 3 for container base | AL3 is GA on MCR. AL4 is alpha with limited access. | March 2026 |
| Per-pod sidecar router | Each sandbox gets its own process. Simple NetworkPolicy, no cross-tenant blast radius. | March 2026 |
| Baseline PodSecurity (not restricted) | egress-guard init container needs NET_ADMIN. Audit+warn remain restricted. | March 2026 |
| No custom SELinux types | Incompatible with restricted PodSecurity. seccomp + iptables + Kata sufficient. | March 2026 |
| SSE streaming for chat | Buffer non-streaming for budget tracking. Stream SSE for low TTFT. | March 2026 |
| Concurrency limit over rate limit | `ConcurrencyLimitLayer(64)` is Clone-compatible with axum. Per-second rate limiting needs tower-governor (future). | March 2026 |
