# AzureClaw Inference Router — Threat Model

**Scope:** this document walks every HTTP route group exposed by
`inference-router` and states, per group: the auth tier, input
validation, blast radius if bypassed, and what attacker capabilities
are required to reach it. Complements [`docs/security.md`](./security.md)
(infrastructure-level defence in depth) with a route-level view.

**Audience:** reviewers of changes to `inference-router/src/routes.rs`
and anyone writing new handlers. Use this doc to reason about what
auth tier and validation a new endpoint needs.

**Source of truth:** all "file:line" citations point at code as of the
branch this doc was introduced on. If you move a handler, update the
citation.

---

## Trust boundaries

Four principals interact with the router:

| Principal | Reaches router via | Can forge its source? |
|---|---|---|
| **Same-pod agent** (UID 1000 in the sandbox container) | `127.0.0.1:8443` | No — kernel-enforced; the router sees `IpAddr::is_loopback()` |
| **Controller / CLI** (admin plane) | Cluster network → pod IP | Partially — attacker with admin token leak needs a pod at an allowlisted IP (see s3) |
| **Peer sandbox** (mesh sender) | WebSocket relay (E2E encrypted) | No — AgentMesh identity is Ed25519-signed per frame |
| **Azure upstream** (responses) | Outbound only; router initiates | N/A — not a caller |

Two auth gates exist at the HTTP layer:
- **`admin_auth_middleware`** (`main.rs:376`) — localhost bypass, else `Authorization: Bearer <ADMIN_TOKEN>` + optional `ROUTER_ADMIN_ALLOW_IPS` IP allowlist.
- **Handoff-specific middlewares** (`handoff.rs`) — admin token required, **no localhost bypass** (prompt-injection-resistant), and mutations additionally require a handoff-session token.

All other routes rely on network-level isolation: the NetworkPolicy
on the sandbox namespace denies all ingress except from the controller
and the paired handoff peer.

---

## Route groups

### 1. `inference_routes` — `/v1/chat/completions`, `/v1/embeddings`, etc.

**Auth:** none at HTTP layer. Reachable from any network path the
NetworkPolicy allows — in practice, the same-pod agent only.

**Input validation:**
- `x-azureclaw-sandbox` header is validated against K8s-name rules (lowercase alnum + hyphens, ≤63 chars, first byte alnum) at `routes.rs:443-449`. Unsafe values fall back to `"unknown"` and the request proceeds — this is a label-only field, not an authorization carrier.
- Body is parsed as raw `Bytes` (subject to axum's 2MB default body limit; large contexts can hit this).
- Model name is extracted from body JSON for AGT policy evaluation (`routes.rs:459-462`).

**Upstream:** Azure AI Foundry deployments. IMDS-authenticated; no API keys ever pass through the agent.

**Blast radius if bypassed:**
- Token-budget consumption on the Foundry deployment.
- Prompt/response goes through Content Safety (Foundry-side DefaultV2 guardrails).
- Responses parsed for `prompt_filter_results` and reported to AGT governance (`safety.rs`).

**Residual risk:** a compromised agent can send arbitrary prompts — that is by design; the governance layer (AGT policy evaluation at `routes.rs:464`) is what enforces policy. Do **not** add auth on top of this group expecting it to gate the agent — agent sandboxing is the primary defence.

---

### 2. `foundry_agent_routes` + `foundry_standalone_routes` — Foundry passthrough

**Auth:** none at HTTP layer. Same posture as `inference_routes`.

**Input validation:** proxied verbatim to Foundry. Router strips the
inbound `Authorization` header (`proxy.rs`) and injects IMDS-obtained
`Authorization: Bearer <token>` for `ai.azure.com` audience.

**Blast radius if bypassed:** agent can reach any Foundry API group
(memory stores, knowledge bases, agents, threads, runs, evaluations,
vector stores, files). This is deliberate — the agent's entire Foundry
access flows through the router.

**Residual risk:** agent can exfiltrate its own Foundry memory store
contents. Mitigation lives at the AGT policy layer and at the
per-agent Foundry project isolation — not at the router.

---

### 3. `mesh_routes` — `/agt/mesh/inbox`, `/agt/relay`, `/agt/registry/*`, `/blocklist/*`

**Auth:** none at HTTP layer.

- **`GET /agt/mesh/inbox`** — read-only. Returns decrypted inbound messages for the same-pod agent.
- **`GET /agt/relay`** — WebSocket upgrade; proxies to AgentMesh relay. Frames inside are Ed25519-signed + double-ratchet-encrypted end-to-end; the router is a transport and cannot read them.
- **`GET /agt/registry/{*path}` / `POST`** — proxies registry lookups. Registry is a public-key directory; no sensitive data.
- **`POST /blocklist/check`** — informational egress check against the 51k-entry blocklist.

**Blast radius if bypassed:**
- The WebSocket frames carry encrypted payloads; an attacker on the router→relay path cannot decrypt.
- The inbox endpoint only returns messages addressed to the local agent; the router never stores peer messages in plaintext beyond the decryption fence.

**Residual risk:** offload file transfers flow through relay WS frames (base64-encoded payloads). The relay itself enforces rate limits and signature verification; the router applies **no body-size limit to WS frames** because they aren't buffered. A malicious peer sending giant frames is a relay concern, not a router concern. See `vendor/agentmesh-relay` for upstream mitigations.

---

### 4. `health_routes` + `metrics_routes` — `/healthz`, `/readyz`, `/metrics`

**Auth:** none. Health endpoints return OK/NOT READY. Metrics return Prometheus text.

**Blast radius:** metrics include counts (requests, upstream retries, safety flags, trust updates, handoff rate-limit hits) but no identifiers or payloads. Safe to expose within the cluster.

**Residual risk:** metric cardinality growth (e.g., labels keyed on free-form strings). Current metrics use fixed label sets — watch this when adding new counters.

---

### 5. `admin_routes` — `/admin/model`

**Auth:** `admin_auth_middleware` — localhost bypass **or** admin token + optional IP allowlist.

**Scope:** dev-mode live model switching (read/PUT the active Foundry deployment name). Persists in memory only; lost on pod restart.

**Blast radius if bypassed:** attacker can redirect inference to a different deployment in the same Foundry project. Cannot reach outside the project (IMDS audience is pinned). Cannot exfiltrate keys (there are none).

**Residual risk:** if admin token leaks, attacker with pod network access can redirect traffic. `ROUTER_ADMIN_ALLOW_IPS` (s3) now mitigates this by requiring the attacker to also control a pod at an allowlisted address.

---

### 6. `egress_routes` — `/egress/{approve,deny,learn,enforce,allowlist,pending,fetch}`

**Auth:** `admin_auth_middleware`.

**Scope:** manages the per-sandbox egress allowlist (which hostnames UID 1000 can reach via the forward proxy). `approve`/`deny` add/remove entries; `learn` toggles learn-mode; `enforce` locks the current list.

**Blast radius if bypassed:** attacker with admin credentials can:
- Broaden the egress list → agent can reach arbitrary external hosts (up to what the NetworkPolicy allows — typically `*.openai.com`, `*.azure.com`, plus the relay).
- Flip to `learn` mode → new destinations auto-allowed.

**Residual risk:** `ROUTER_ADMIN_ALLOW_IPS` (s3) raises the bar. The agent itself cannot flip these because they require the admin token, and the UID 1000 sandbox has no token access (`/etc/azureclaw/secrets/admin-token` is root-owned, read-only, not mounted in the sandbox container — only in the router sidecar).

---

### 7. `spawn_routes` — `/sandbox/spawn`, `/sandbox/list`, `/sandbox/{name}/status`, `/sandbox/{name}` (DELETE)

**Auth:** `admin_auth_middleware`.

**Scope:** creates, lists, inspects, and deletes sub-agent sandboxes via the controller's CRD API.

**Input validation:**
- `SpawnRequest` is a typed struct (serde) with `#[serde(deny_unknown_fields)]` (`spawn.rs:37`). A typo like `{"nam": "x"}` is rejected with a 422 instead of silently falling back to a default. The canonical field is `agent_id` (a DNS-safe k8s metadata.name, 1–63 chars, `[a-z0-9-]`); `name` is accepted as a deserialise-only serde alias during the plugin-side migration window.
- AGT policy evaluation runs before CRD creation (`routes.rs:2893`) — a denied policy returns 403 without reaching kubeapi.

**Blast radius if bypassed:** attacker with admin token can spin up arbitrary sub-agent sandboxes with arbitrary images **iff** the controller accepts the CR. Controller-side validation (CRD schema + admission) is the second line of defence.

**Residual risk:**
- A malicious spawn could mount arbitrary images if admin token leaks. Mitigation: ACR pull is pinned to a single registry by the controller's image template; image tags are rewritten to `:latest`. See controller image-policy notes.
- Sibling sandbox enumeration via `/sandbox/list` — leaks names but not credentials.

---

### 8. `sensitive_agt_routes` — AGT governance plane

Routes: `/agt/evaluate`, `/agt/trust` (GET/POST/DELETE), `/agt/audit`, `/agt/audit/verify`, `/agt/status`, `/agt/signing-counter`, `/agt/rate-limit` (GET/PUT), `/agt/reputation`.

**Auth:** `admin_auth_middleware` at the router layer **plus** in-handler admin-token re-verification for mutations (`POST /agt/trust`, `DELETE /agt/trust/{id}`, `PUT /agt/rate-limit`). The in-handler check bypasses the localhost exemption — same-pod agent (UID 1000) **cannot** forge trust mutations even via localhost. See `routes.rs:1779-1780` comment.

**Input validation:**
- Trust body fields pulled by `.get("agent_id").and_then(...).unwrap_or("unknown")` — loose; see Q2/Q4 in plan.md.
- Rate-limit body fields are `u32`/`u64` with defaults.
- Handlers here take `Json<serde_json::Value>` rather than typed structs, so `deny_unknown_fields` does not apply. Input validation is per-field inline. Moving to typed DTOs is a separate refactor (Q2).

**Blast radius if bypassed:**
- `update_trust` rewrites AGT's signed trust store → attacker can downgrade a peer's trust tier and trigger AGT-driven policy changes (rate limits, permissions).
- `rate_limit_update` broadens or narrows router-wide throughput caps.
- `audit` returns the full hash-chained audit log (informational; chain verification via `/agt/audit/verify` detects tampering).

**Residual risk:** double-layer admin check means admin-token leak is the only path; s3 allowlist mitigates. Trust field validation is the next weakest link — S1 will close this.

---

### 9. `handoff_init_routes` — `POST /agt/handoff/init`

**Auth:** `handoff_init_auth_middleware` — admin token, **no localhost bypass**.

**Scope:** starts a handoff session, returning a one-time `handoff_token`.

**Blast radius if bypassed:** attacker can open a handoff session, receive a session token, and then try to snapshot/restore. They still need the admin token for subsequent mutation routes.

---

### 10. `handoff_protected_routes` — `/agt/handoff/{snapshot,restore,verify,drain,decommission,abort,succession}`

**Auth:** `handoff_auth_middleware` — admin token **and** handoff-session token, **no localhost bypass**.

**Body limit:** 200 MB (`DefaultBodyLimit::max(MAX_BLOB_SIZE_BYTES)` at `routes.rs:3012`). Sized for real workspace-tar + sub-agent-workspace payloads. Large but capped.

**Input validation:**
- Blob-size recheck after compression (`routes.rs:3249, 3498, 3518`) — defence-in-depth vs. a body-limit bypass.
- `HandoffMeta` DTO has `#[serde(deny_unknown_fields)]` (`spawn.rs:62`). Larger handoff state structs use untyped `Value` decode; ratchet-key decryption is the authoritative gate — a state blob that decrypts is authentic by definition.
- Decryption must succeed before the state is trusted — ratchet keys are the real gate.

**Blast radius if bypassed:** handoff snapshots contain encrypted workspace data, trust state, audit log, and per-agent secrets (not private keys — identity succession replaces key transfer, `handoff.rs:69`). Attacker with admin + handoff tokens can harvest this.

**Residual risk:** the 200MB cap is a DoS shoulder — an adversarial handoff allocates ~200MB in serde before rejection. Tighter per-field streaming would bound this but is a bigger refactor. Mitigated in practice by handoff being admin-token-gated + no-localhost-bypass, so only an admin-credential-holder can exercise the allocation.

---

### 11. `handoff_status_routes` — `GET /agt/handoff/status`, `GET /agt/handoff/sub-agents`

**Auth:** `handoff_status_auth_middleware` — admin token, **localhost allowed** (read-only, needed by the agent to poll its own handoff state).

**Blast radius:** read-only metadata.

---

## Cross-cutting controls

These apply to every group above, not just one:

| Control | Where | What it catches |
|---|---|---|
| **Trace-id middleware** (r6) | `main.rs:~440` | Every request gets a stable `X-Azureclaw-Trace-Id`. Sanitizer rejects log-injection payloads (CRLF/ANSI/path) at `is_safe_trace_id`. |
| **Concurrency limit** | `main.rs:233` | `ROUTER_CONCURRENCY_LIMIT` (default 256). Bounds simultaneous in-flight requests → bounds memory. |
| **Graceful-shutdown timeout** | `main.rs:258-275` | `SHUTDOWN_TIMEOUT_SECS` bounds how long SSE streams can hold up pod termination. Derived from `TERMINATION_GRACE_PERIOD_SECS` − 5s. |
| **Retry middleware** (r3) | `proxy.rs` | Idempotent upstream calls (GET + `/embeddings`) retry with exponential backoff on connect-reset + 502/503/504. Non-idempotent verbs never retry. |
| **Azure correlation ids** (r6) | `proxy.rs:~165, ~350` | Every upstream response logs `x-ms-request-id` + `apim-request-id` paired with our `trace_id` — closes the Azure-support loop. |
| **Constant-time admin-token compare** | `handoff::constant_time_eq` | Used in all admin token checks. |
| **Content Safety** | Foundry DefaultV2 + `safety::parse_prompt_filter_results` | Prompt-injection / harmful-content categories surfaced to AGT. |

---

## Known gaps (tracked on plan.md)

| Gap | Tracking |
|---|---|
| `#[serde(deny_unknown_fields)]` coverage only applies to the two typed-struct bodies (`SpawnRequest`, `HandoffMeta`). Every other handler takes `Json<serde_json::Value>` and forwards opaquely — no struct to decorate. Migrating those to typed DTOs is part of Q2/Q4. | Q2, Q4 |
| `routes.rs` is 5000 LOC | Q1 — deferred; high merge-conflict risk |
| Inconsistent error shapes | Q2 — deferred |
| Inconsistent identity field naming (`agent_id` vs `sandbox_name` vs `name`) | Q4 — deferred |
| Per-trust-tier body caps | S6 — **not pursued**. Analysis showed body-size is the wrong knob: inference large-context traffic legitimately approaches the axum 2MB default, file transfers flow through the AgentMesh WS relay (bypasses body-limit middleware), and admin endpoints are token-gated rather than tier-gated. The per-tier abuse defences that actually bite already exist: AGT `McpSlidingRateLimiter` (50 req/sec/agent) and `TokenBudgetTracker`. |

---

## Reviewer checklist — new route group

When adding a new route group to `routes.rs`, answer these in the PR:

1. **Auth tier?** Pick one: public (agent/cluster), admin-gated, handoff-tiered, or custom. If public, justify why the agent being able to call this is safe.
2. **Same-pod bypass?** If admin-gated, does localhost bypass make sense, or do you need `no_localhost_bypass` (like handoff)?
3. **Input DTO has `deny_unknown_fields`?** (S1) If you introduce a new typed struct body, yes. Handlers that forward opaque JSON are exempt by design.
4. **Body size?** Explicit `DefaultBodyLimit` or relying on axum's 2MB default? Justify.
5. **AGT policy hook?** If the action is agent-initiated and may be policy-relevant, call `state.governance.evaluate(...)` before the side-effecting code.
6. **Audit logging?** Mutations should record to the audit chain.
7. **Trace-id?** You get this for free via the outermost middleware — just don't strip headers.
