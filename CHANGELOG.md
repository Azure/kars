# Changelog

All notable changes to AzureClaw will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased] — PR #44 `dev → main` uplift

This entry covers **186 commits** on `dev` since `main`, structured as Phase 0
(seams + safety net) and Phase 1 (protocol freshness + minimal schema). Every
capability cites code; every capability-introducing PR shipped a security-audit
doc under `docs/security-audits/` (75 docs total). See
[`docs/phase-0-1-capabilities.md`](docs/phase-0-1-capabilities.md) for the full
evidence index.

### Phase 0 — provider seams + compat suite + CI gates

#### Added
- **Four-seam provider architecture** — `PolicyDecisionProvider`, `AuditSink`,
  `SigningProvider` traits with in-tree `impl … for Governance` (router crate);
  each contract reachable via `Arc<dyn Trait>` view of the same
  `Arc<Governance>`. The fourth contract `MeshProvider` is **plugin-side by
  design** — the router's `providers/mesh.rs` is a documentation-only trait
  file (see [`docs/agt-boundary.md`](docs/agt-boundary.md)).
- **Outage-mode dispatch** (`providers/outage.rs`) — `Strict` (prod default,
  fail-closed), `CachedRead` (allow if cached decision < TTL), `DegradedDev`
  (fail-open with warning label, dev only). Configurable per-`ClawSandbox`
  via `spec.agt.outageMode`.
- **Six blocking CI gates** under `ci/`: `check-loc.sh` (LOC budget),
  `no-stubs.sh` (no `TODO/FIXME/unimplemented!`), `no-custom-crypto.sh`
  (forbids hand-rolled crypto outside provider seams + vendored SDK),
  `no-null-provider-prod.sh` (Null* providers blocked unless
  `azureclaw.azure.com/dev-only` label set), `security-audit-required.sh`
  (per-PR audit-doc enforcement, 2 sign-offs), `vendored-patch-audit.sh`
  (forces re-audit on AGT SDK bump), plus `a2a-module-isolation.sh`. Budget
  in `ci/loc-budget.yaml`.
- **75 security-audit docs** under `docs/security-audits/` from the
  `_template.md` shape: threat-model delta, OWASP MCP/LLM mapping, AuthN/Z
  path, secret + key custody, egress-surface delta, audit events emitted,
  failure mode (fail-closed default), negative-test coverage, two sign-offs.
- **Behavioral conformance corpus** (`tests/conformance/`) — 8 specs:
  `signal-x3dh`, `signal-knock`, `signal-negative`, `oauth21-bcp`,
  `mcp-streamable-http`, `a2a-agent-card`, `ap2-commerce`,
  `sandbox-isolation`. Negative cases (tampered ciphertext, replayed message,
  wrong-issuer card, expired mandate) are mandatory per new endpoint.
- **Compat suite** (`tests/compat/`) — operator TUI flow with virtual-screen
  + outgoing-CR-payload assertions via a `blessed` mock harness.
- **5 cargo-fuzz targets** (`inference-router/fuzz/fuzz_targets/`) —
  `a2a-jws`, `a2a-base64url`, `deserialize-state`, `sanitize-chat`,
  `parse-streaming-pf`.
- **`docs/agt-boundary.md`** — operational AGT boundary (the four contracts,
  what AGT owns, what AzureClaw owns).
- **`docs/agt-vendored-patch-audit.md`** — full index of **26 vendor patches**
  (SDK 21 + relay 4 + registry 1) with re-audit cadence on AGT SDK bump.
- **`docs/internal-boundaries.md`** — per-MSFT-product consume / be-consumed /
  orthogonal posture; required for every new CRD or router capability.
- **`docs/sigs-agent-sandbox-compat.md`** — `TranslateMode` / `OverlayMode`
  design for optional compat with `kubernetes-sigs/agent-sandbox`. Opt-in,
  no upstream dependency, no CI pin.
- **Hotspot decomposition (Pass 1 + 2)** with byte-equivalence proofs:
  - `inference-router/src/routes.rs` 4890 → 6 files (`routes/{inference,handoff,governance,mesh,egress,mod}.rs`); 1 allowlisted namespace fix.
  - `controller/src/reconciler.rs` 2326 → 1464 LOC.
  - `controller/src/mesh_peer.rs` 1970 → 1170 LOC; split into `mesh_peer/{mod,offload,pair}.rs`.
  - `inference-router/src/governance.rs` 1252 → 837 LOC.
  - `inference-router/src/handoff/mod.rs` 2075 → 1770 LOC.
  - `inference-router/src/spawn/docker.rs` 1199 → 762 LOC.
  - `cli/src/plugin.ts` 7455 LOC: `foundry-discovery.ts` and
    `router-client.ts` extracted.
- Repo tooling for behavioral-equivalence proofs (`tools/item-manifest/` +
  `tools/drift/drift.py`); baselines + allowlists under `tools/drift/`.
- **Federated-credential reaper** (`controller/src/fedcred_reaper.rs`, 232
  LOC, 4th `tokio::select!` arm in the controller event loop) — periodically
  GCs orphan federated credentials against the 20-fedcred-per-MI Azure cap;
  default 600 s, env override `FEDCRED_REAPER_INTERVAL_SECS`. 5 unit tests.
- **KEP-1623 status subresource on `ClawSandbox`** — `Conditions[]` +
  `observedGeneration`; controller stamps `Degraded=True` / `Ready=False`
  on the three validation-failure exits.
- **VAP / MAP set** in the controller Helm chart — `pods/exec|attach|portforward`
  ban on sandbox namespaces; deny posture-downgrades (isolation step-down,
  seccomp removal, `readOnlyRootFilesystem: false`); deny removal of
  `azureclaw.azure.com/dev-only` label once applied; mutating policy auto-
  injects router sidecar + sets seccomp to `azureclaw-strict`.

### Phase 1 — protocol freshness + minimal schema

#### Added
- **MCP 2026 Streamable HTTP** (`inference-router/src/mcp/`, 8 modules:
  `error`, `initialize`, `jsonrpc`, `oauth`, `oauth_layer`, `pipeline`,
  `streamable_http`, `tools`) — `POST /mcp` with full JSON-RPC 2.0 framing,
  `Mcp-Session-Id` semantics, batch support, oversized-frame reject;
  `tools/list` + `tools/call` dispatch; OWASP MCP Top 10 controls matrix at
  `docs/security-mcp-top10.md`.
- **OAuth 2.1 (RFC 8725 BCP)** — bearer-token verifier as a `tower::Layer`;
  PKCE, audience, expiry, resource-indicator, scope checks; gated by
  `McpServer.spec.productionMode: true`.
- **A2A 1.0.0** (`inference-router/src/a2a/`, 14 modules including
  `agent_card`, `agent_projection`, `card_server`, `card_signing`,
  `card_verifier`, `jsonrpc_dispatch`, `signature`, `snapshot_rebuild`,
  `trust_store`) — `/.well-known/agent.json` per-sandbox (Ed25519 detached
  JWS via `SigningProvider`); inbound `POST /a2a` JSON-RPC dispatch
  (`message/send`, `tasks/get`, `tasks/cancel`); hot-reloading
  trust-store snapshot for `kid → VerifyingKey`. Schema source:
  <https://a2a-protocol.org/v1.0.0/specification>. Ingress posture is
  gateway-only, surgical opt-in via `ClawSandbox.spec.a2a.expose: true` —
  see [ADR-0001](docs/adr/0001-a2a-ingress-front-edge.md).
- **AP2 commerce mandates** (`a2a/{ap2,mandate_signing,mandate_trust_store,
  message_send_ap2}.rs`) — IntentMandate detached-JWS sign/verify; per-tool
  `commerce.dailyCap` / `monthlyCap` / `counterpartyAllowlist` enforcement;
  type-safe `MandateTrustStore`.
- **`McpServer` CRD (schema-only)** + **`ToolPolicy` CRD (schema-only)** —
  `controller/src/{mcp_server,tool_policy}.rs`; CEL `x-kubernetes-validations`
  post-processed via `controller/src/crd_validations.rs` because kube-rs
  `CustomResource` derive does not emit the field (kube-rs#1557). Reconciliation
  ships in Phase 2.
- **`ClawPairing` CRD** + reconciler — operator-assisted pairing as a
  K8s-native operation (`controller/src/{pairing,pairing_reconciler}.rs`);
  `azureclaw pair <a> <b>`.
- **Identity provider seam — Microsoft Graph agent identity** — production
  Graph client at `controller/src/providers/identity_*.rs` calling
  `POST /beta/servicePrincipals/microsoft.graph.agentIdentity`,
  `POST /beta/servicePrincipals/{id}/federatedIdentityCredentials`,
  `DELETE /beta/servicePrincipals/{id}`. Endpoints verified against
  learn.microsoft.com (commit `2114bf2`). +5 controller tests (147 total).
- **Policy hot-reload** — router subscribes via K8s informers + AGT SSE;
  applies new `ToolPolicy` / `InferencePolicy` in-process without pod
  rollout; provider-flag flip (`vendored ↔ agt`) also hot-reloads.
- **OTel GenAI SemConv 1.x** emission on every router span.
- **Gateway token via `secretKeyRef`** — `OPENCLAW_GATEWAY_TOKEN` is mounted
  from a K8s `Secret` instead of plain env, with a one-shot warning when
  legacy plain-env paths are exercised.
- **Three new CLI commands** — `azureclaw a2a` (Phase 1 scaffold:
  `list-exposed`, `schema`), `azureclaw convert` (Phase 0 skeleton),
  `azureclaw pair`.
- **`docs/use-cases.md`** + **`docs/phase-0-1-capabilities.md`** + ADR-0001
  + OWASP MCP Top-10 controls matrix.

### Phase 0/1 — Recent fixes
- **Sub-agent re-spawn after handoff** — sub-agent trust + resume signals
  must use `restoreResp.sub_agent_results` (spawned), not
  `sub_agent_workspaces` (may be empty). `cli/src/plugin.ts:2164-2270`.
- **Vendor patch #21 (SDK)** — `SessionManager.initiateSession` returns
  `{reused: true}` when an incoming KNOCK already established a crypto-layer
  session (was throwing "Active session already exists").
- **`azureclaw connect` port-forward error surfacing** — kubectl stderr is
  now displayed in the human-readable "address already in use" form.
- **Deduplicated chat replay** — long-standing duplicate-message UI bug
  triaged across plugin + sandbox image; investigation captured in session
  checkpoints.

### Engineering metrics (PR #44)
- **186 commits** on `dev` since `main`.
- **75 security-audit docs** under `docs/security-audits/`.
- **26 vendor patches** (SDK 21 + relay 4 + registry 1).
- **6 blocking CI gates** + a2a-module-isolation + LOC budget.
- **8 conformance specs**, **1 compat spec**, **5 fuzz targets**.
- **205 Rust tests** (74 controller + 105 router + 26 integration); **207 CLI
  tests**.
- **4 CRDs total** — `ClawSandbox` + `ClawPairing` reconciled; `McpServer` +
  `ToolPolicy` schema-only.
- **21 CLI commands**, **10 skills** (8 Foundry + 2 internal), **5 Docker
  images**.

## [pre-PR-44 baseline]

### Added
- **Preflight RBAC checks for `azureclaw up`** — new `cli/src/preflight.ts` queries effective permissions at subscription scope (`Microsoft.Authorization/permissions`), resource-provider registration, and preview-feature flags BEFORE Bicep runs, so operators fail in ≤30s instead of 20 minutes in. Prints copy-pasteable `az role assignment create` remediation commands with the exact missing actions. Escape hatch: `--skip-preflight`. See `docs/permissions.md` for the full role matrix + custom-role JSON.
- **`docs/permissions.md`** — canonical required-roles reference for `azureclaw up`: Contributor + User Access Administrator (or Owner), per-action justification, least-privilege custom role, preview feature registration, and Entra `api://agentmesh` tenant-admin caveat.
- **Bidirectional Agent Handoff** — live-migrate agents between local Docker and AKS cloud with `azureclaw handoff <name> --to cloud|local`. Supports both CLI-driven (operator) and LLM-driven (webchat) orchestration paths
- **Sub-Agent Handoff** — sub-agents are snapshotted (workspace + task state), destroyed on source, re-spawned on target, and injected with workspace + resume signal via E2E encrypted mesh
- **Stale AMID Cache Poisoning Fix** — three-layer defense: identity-based AMID rejection, prekey readiness gate, workspace inject retry with ack verification
- **Workspace Injection Pipeline** — tarball extraction with path traversal validation, `incoming/` file promotion to workspace root, `HANDOFF_FILES.md` manifest for agent discoverability
- **Handoff Decommission Cleanup** — reverse handoff deletes all cloud CRDs (parent + sub-agents); forward handoff destroys local sub-agent containers
- **Mesh Inbox Improvements** — protocol message filtering (hides handoff/ack messages), auto-decode of `file_transfer` base64 content
- **Native AGT Governance** — Rust-native governance module (replaces former Python sidecar) with PolicyEngine, TrustManager (0–1000, ±200 clamp, Ed25519 signed), SHA-256 Merkle audit chain, RateLimiter, and BehaviorMonitor
- **E2E Encrypted Inter-Agent Messaging** — Signal Protocol (X3DH + Double Ratchet) via AgentMesh relay/registry with KNOCK trust handshake
- **Content Safety via Foundry Guardrails** — Content Safety + Prompt Shields enforced server-side by Foundry (`Microsoft.DefaultV2`); the router parses `prompt_filter_results` annotations from model responses and reports flags to AGT governance for trust scoring and audit
- **Foundry Agent Service Integration** — web search, code execute, file search, image generation, memory via Foundry project endpoint
- **5-Image Architecture** — controller, inference-router, sandbox, agentmesh-relay, agentmesh-registry (governance runs natively in the router)
- **CLI `push --only <image> --apply`** — selective image builds with automatic pod restart
- **10 AGT Policy Rules** — shell-safety, inference rate-limiting, content safety, mesh trust gates, spawn governance, sensitive file deny, recon tool deny, cloud metadata deny
- **AGT Tool Execution Gate** — exec_command and http_fetch are evaluated by the native governance module before execution; fail-open with 2s timeout
- **Operator Dashboard** — real-time trust scores, audit chain, policy status, mesh connectivity
- **GitHub CI/CD** — Rust + TypeScript + Python lint/test, Bicep validation, Helm lint, Trivy security scan, Dockerfile lint, tag-triggered releases
- **Unit Tests** — Rust (controller + router) and TypeScript (CLI + plugin) covering controller, router, CLI, and governance
- **GitHub Templates** — issue templates (bug, feature, security), PR template, CODEOWNERS
- **Trace-ID correlation** — every inbound router request is assigned an opaque `x-trace-id` (or honors a client-supplied one), propagated to upstream Azure calls, tagged on all tracing spans, and stamped onto every AGT audit-chain entry. Unblocks multi-hop "why did this prompt fail" debugging without a rebuild loop.
- **Bounded-retry middleware for idempotent upstream calls** — `proxy::forward` now retries up to 3× with exponential backoff on transient Azure upstream failures (connection-reset, 502, 503, 504) for GET and `/embeddings` only. `/chat/completions`, `/completions`, `/responses` are never retried (non-idempotent). Configurable via `UPSTREAM_RETRY_MAX_ATTEMPTS` / `UPSTREAM_RETRY_INITIAL_MS`.
- **Handoff lifecycle metrics** — new Prometheus counters `azureclaw_handoff_pending_events_total{result}` and `azureclaw_handoff_phase_transitions_total{from,to,result}` so operators can see rate-limit cooldowns, token expirations, and phase-machine progress without tail-searching logs.
- **Route-level threat model** (`docs/threat-model/routes.md`) — walks every router group (inference, foundry, agt, mesh, handoff, egress, admin, health) with auth posture, input validation, and blast-radius analysis.
- **Repo tooling for behavioral-equivalence proofs** — `tools/item-manifest/` (syn-based fn-body hasher) + `tools/drift/drift.py` (comparator with allowlist) gates large mechanical refactors. Baselines and allowlists under `tools/drift/baselines/` and `tools/drift/allowlist-*.txt`.
- **Local dev stack** — `docker-compose.dev.yml` + YAML scenario runner (`cli/src/testing/scenario.ts`) so plugin/router behavior can be exercised against a zero-dep fake router without any Docker image builds. Drives the `rebuild → push → wait → debug` loop from >15min down to sub-second for protocol changes.
- **Test fixtures** — 8 sanitized Azure Foundry JSON fixtures + 3 axum-based fake servers (IMDS, AAD, Azure upstream) with a request recorder, all shared between Rust integration tests and the CLI fake-router runner.

### Fixed
- **`azureclaw up` stepper numbering** — declared `totalSteps: 7` never matched the 9 runtime phases (10 with `--expose-registry`), and step 4 (`kubectl` configure) was missing its `stepper.done()` call so it appeared to silently disappear from the progress log. Total now tracks the actual branch count, and every step has an explicit completion.
- Router bind address fix for K8s probe accessibility
- K8s probe host field removal (kubelet defaults to pod IP)
- Missing transitive Python dependencies (typing_inspection, cryptography) via PyPI fallback
- 8 vendor patches for AgentMesh relay, registry, and SDK bugs (this baseline; the active count is **26 patches** as of PR #44 — see `docs/agt-vendored-patch-audit.md`)
- Foundry Memory Store format — ensureMemoryStore creates full store with chat + embedding models; item format matches Foundry REST API spec

### Changed
- AGT inference rate limit bumped from 120 → 500 calls/60s (policy) and router token bucket from 100 → 500 global req/s (needed for multi-agent handoff traffic)
- Controller reconcile error requeue is now split by error kind: transient `kube::Error` keeps the 30s requeue, but `serde_json::Error` (malformed CR fields) now requeues at 300s instead of 30s. Malformed CRs won't heal on retry, so the longer back-off avoids log-spamming every 30s while a human edits the resource. Operators debugging a failed reconcile should expect a ~5-minute gap, not 30s. An `error!` log line is always emitted so the delay is never silent. See `controller/src/reconciler.rs::error_requeue_duration`.
- **`POST /sandbox/spawn` canonical field is now `agent_id` (was `name`).** The Rust `SpawnRequest` / `SpawnResponse` / `SubAgentEntry` / `SubAgentSnapshot` structs use `agent_id` as the field name, and responses serialise `agent_id` on the wire. For backward compatibility with in-flight plugins, `name` is accepted as a deserialise-only serde alias on `SpawnRequest` and `SubAgentSnapshot`; a payload that sets both `agent_id` and `name` is rejected with a 422 (duplicate field) to catch inconsistent clients. The bundled plugin has been migrated to send and read `agent_id`. Operators who call `/sandbox/spawn` directly (e.g. via curl or a custom client) should switch to `agent_id` — the `name` alias will be removed in a future release.
- **Canonical admin auth is now `Authorization: Bearer <token>`.** The legacy `x-azureclaw-admin` header is still accepted but emits a one-shot `warn!` log on first use per process. It will be removed in a future release. No action required for operators using the bundled CLI; custom scripts should switch to `Authorization: Bearer`.
- **Router bounded graceful-shutdown.** `axum::serve().with_graceful_shutdown(...)` is now wrapped in `tokio::time::timeout`. Default timeout is `max(TERMINATION_GRACE_PERIOD_SECS − 5s, 10s)` (typically 25s). Override with `SHUTDOWN_TIMEOUT_SECS`. Long-running SSE streams past the budget are log-and-dropped instead of blocking pod termination indefinitely.
- **Router error-response format unified.** All router handlers now emit one of two documented shapes: a flat `{code, message, trace_id}` for internal endpoints, or the OpenAI-compatible `{error: {type, code, message}}` for inference/foundry endpoints. The constructors (`errors::flat`, `errors::openai`) are pinned by byte-exact unit tests. See `inference-router/src/errors.rs` and `docs/threat-model/routes.md`.
- **Internal: `inference-router/src/routes.rs` (4890 LOC) split into 6 files** under `routes/` (`inference`, `handoff`, `governance`, `mesh`, `egress`, `mod`). Byte-level equivalence proven by `tools/drift/drift.py` against the pre-split baseline; exactly 1 allowlisted namespace-resolution fix. No behavior change.
- **File-size policy** (`CONTRIBUTING.md`): any PR that pushes a source file past 1500 LOC must either split the file in the same PR or add a follow-up issue link. Enforced via manual review.

### Security
- Foundry-side Content Safety guardrails (`DefaultV2`) — content filter annotations parsed from model responses and reported to AGT governance
- iptables UID-based egress — agent process restricted to localhost
- Zero Azure credentials in agent container — router authenticates via Workload Identity
- Kata Confidential VM support — per-pod dedicated kernel
- Custom seccomp profile (219 allowed syscalls, 28 explicitly blocked)
- Domain blocklist (51k+ malicious domains)
- **`#[serde(deny_unknown_fields)]` on typed inbound DTOs** (`SpawnRequest`, `HandoffMeta`) — unknown fields are now rejected at deserialization. All other router handlers take `Json<serde_json::Value>` and forward opaquely.
- **Constant-time admin-token comparisons** via `handoff::constant_time_eq` — replaces 4 `==` compares across `routes.rs` (AGT trust/rate-limit endpoints) and `main.rs` (cross-pod bearer-auth middleware). Eliminates timing-side-channel risk.
- **Admin-endpoint Origin allowlist** — requests to admin routes with a browser `Origin` header are rejected unless the origin is on `ADMIN_ALLOWED_ORIGINS` (default: none). CLI/curl traffic (no `Origin`) is unaffected. Closes cross-site-request abuse vector on leaked admin token.
- **`cargo audit` CI job** — runs on every PR; currently `continue-on-error: true` pending triage-cadence decision. Caught RUSTSEC-2026-0098/-0099/-0104 during rollout (closed by bumping `rustls-webpki` 0.103.10 → 0.103.13).
- **Sandbox-hardening regression tests** — every hardening invariant (UID 1000, read-only rootfs, all caps dropped, seccomp strict profile, NET_ADMIN drop after init, iptables egress-guard, plugin+SDK root-owned read-only) is asserted by a controller-side reconciler unit test that fails on regression.
- **Fuzz + proptest coverage** — `cargo +nightly fuzz` targets for the handoff blob parser, blocklist domain parser, AGT policy evaluator, and safety-response parser. `proptest` coverage for handoff-chunking, Double-Ratchet state transitions, and K8s name validation.
- **Vendor dependency advisory closure** — pulled `rustls-webpki` 0.103.13 to close 3 live RUSTSEC advisories. Only remaining audit warning is a transitive `rand 0.8.5` soundness note via upstream `agentmesh 3.1.0` (requires upstream bump).
