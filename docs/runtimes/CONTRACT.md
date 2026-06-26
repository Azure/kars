# Kars Runtime Contract — v1

**Status**: stable contract; runtimes adopting this spec are first-class peers of OpenClaw.
**Audience**: implementers of new kars runtime adapters (Hermes, future agent frameworks).

> **What "stable" means here.** The *contract surface* (the HTTP / env / file
> interface below) is stable — you can build against it. Two injection details are
> still being generalised from the OpenClaw reference branch to every runtime kind
> (tracked as **A1.2** and called out inline below); the **"Status (today)"**
> columns mark exactly what is wired now versus the target v1 state. Nothing in the
> stable surface will change incompatibly — A1.2 only widens where the existing
> envs/mounts are injected.

---

## What a "runtime" is in kars

A **runtime** is the agent framework that executes inside a kars sandbox pod / container. Kars provides isolation (K8s pod, seccomp, NetworkPolicy, egress-guard), an inference proxy sidecar (`inference-router`), AGT mesh + governance, and CRD-driven orchestration. The runtime is the **agent loop** that consumes those services — OpenClaw is the reference implementation; Hermes is the next first-class runtime; Pydantic-AI / LangGraph / Anthropic-SDK / OpenAI-Agents / MAF-Python are partial-support runtimes.

A runtime adapter consists of three artifacts:
1. **A sandbox image** (`sandbox-images/<runtime>/Dockerfile` + `entrypoint.sh`)
2. **An in-pod adapter layer** (`runtimes/<runtime>/`) — implements the kars-side hooks. This is "plugin code" in OpenClaw and Hermes terminology, but kars treats it generically: it can be a plugin, a wrapper library, an entrypoint shim, or a sibling process — whatever shape lets the runtime honor the HTTP / env / file contract below.
3. **A controller branch** — `runtime.kind` enum variant + image selection in `controller/src/reconciler/runtime.rs`

Everything else (mesh, policy enforcement, audit logging, trust scoring, content safety, token budgets, Foundry proxy, MCP gateway) lives in the router/controller and is **runtime-agnostic**.

---

## Three deployment platforms

The contract has **two layers**:

1. **Portable runtime contract** — the env/HTTP/CRD surfaces the runtime adapter sees. Holds identically on all three platforms.
2. **Platform security implementation** — how kars *enforces* the boundaries around the runtime. Strongest on AKS, weaker on docker dev where you're trading isolation for speed.

| Layer | AKS (production) | local-k8s (kind) | docker dev |
|---|---|---|---|
| Runtime contract envs / HTTP / CRDs | ✅ identical | ✅ identical | ⚠️ CRDs absent — runtime sees env-shaped equivalents from `inference-router/src/spawn/docker.rs` |
| K8s pod isolation (NetworkPolicy, seccomp `kars-strict`, readOnlyRootFilesystem, runAsNonRoot UID 1000) | ✅ enforced | ✅ enforced (kindnet enforces NetworkPolicy since 1.35) | ❌ docker container — no NetworkPolicy / seccomp / readOnly rootfs |
| Egress restriction | iptables egress-guard init container (kernel-level) | same | iptables rules inside the container (entrypoint adds them; container has CAP_NET_ADMIN at start) |
| Workload Identity for upstream calls | ✅ AAD federated cred | ✅ same | API-key based (`KARS_AUTH_MODE=api-key`); user-supplied creds |
| Controller present | ✅ | ✅ | ❌ — router does spawn directly via Docker Engine |
| Mesh relay/registry endpoint | `ws://agentmesh-relay.agentmesh.svc.cluster.local:8765` | same (kind-deployed) | `ws://kars-agt-relay:8765` (compose network `kars-dev`) |
| Sub-agent spawn | Router → controller → CRD → new pod | Same | Router → `docker create` via mounted socket → sibling container |
| Secrets propagation | K8s Secret `<sandbox>-credentials` mounted via `envFrom` | Same | `docker run -e` env passthrough; updated via `docker exec` + restart |
| Process model | Multi-container pod (runtime + router + init egress-guard) | Same | Single container, runuser shim for UID separation |

**Rule of thumb:** A runtime adapter that obeys the portable contract works on all three platforms. Operators choosing docker dev accept the weakened isolation in exchange for speed. The runtime cannot detect which platform it's running on (and shouldn't try) — the contract is platform-neutral.

---

## Environment-variable contract

The controller (AKS/local-k8s) or `inference-router/src/spawn/docker.rs` (dev) injects this env into the runtime container.

### Always present (all 3 platforms)

> **Implementation gap (to be closed in A1.2):** Several "always present" envs are currently injected only by the OpenClaw branch in `controller/src/reconciler/mod.rs` (gated by `if is_openclaw`). The contract describes the target v1 state — A1.2 lifts these to be unconditionally injected for every runtime kind. The "Status (today)" column tracks current implementation.

| Env | Source | Purpose | Status (today) |
|---|---|---|---|
| `SANDBOX_NAME` | CRD `metadata.name` (k8s) or `--name` flag (docker) | DNS-safe sandbox identifier; **MUST** be reflected as the runtime's profile/session-scope name | ✅ all runtimes |
| `KARS_MODEL` (preferred) / `OPENCLAW_MODEL` (alias) | `InferencePolicy.modelPreference.primary.deployment` | Default model deployment name | ⚠️ OpenClaw-only as `OPENCLAW_MODEL`; A1.2 adds generic `KARS_MODEL` for all runtimes |
| `AZURE_OPENAI_ENDPOINT` | Helm `azureOpenai.endpoint` (k8s) or `--endpoint` (docker) | Upstream model endpoint — runtime **MUST NOT** call this directly; sees it for diagnostic display only | ✅ all runtimes |
| `OPENAI_BASE_URL=http://127.0.0.1:8443/v1` | **Runtime entrypoint MUST set this**, not the controller | What the runtime's OpenAI-compatible client uses. The router enforces governance on every call. | Runtime entrypoint responsibility |
| `AGT_RELAY_URL` | controller | E2E mesh relay WebSocket URL | ⚠️ OpenClaw-only branch today; A1.2 generalizes |
| `AGT_REGISTRY_URL` | controller | AGT registry HTTP base URL | ⚠️ same |
| `AGT_GOVERNANCE_ENABLED` | `KarsSandbox.spec.governance.enabled` | Hint to runtime that AGT policy gating is enforced server-side | ⚠️ same |
| `AGT_TRUST_THRESHOLD` | `KarsSandbox.spec.governance.trustThreshold` | Numeric trust floor (default 500) | ⚠️ same |
| `KARS_MESH_PROVIDER=agt` | controller hard-set | Mesh protocol family | ✅ all runtimes |
| `KARS_AUTH_MODE` | controller: `workload-identity` (k8s) or `api-key` (dev) | Tells entrypoint how creds are sourced | ✅ all runtimes |

### Conditionally present

| Env | Set when | Purpose |
|---|---|---|
| `KARS_RUNTIME_CONTRACT_VERSION` | always (controller / docker spawn) — **A1.2 makes this generic across runtimes; today only on OpenClaw** | Currently the controller emits `v1` only via per-runtime branches; A1.2 lifts to the always-on layer. Runtimes MUST error loudly if value is missing or `>v1` until they support a newer contract. |
| `KARS_DEV_PROFILE=true` | `kars dev` (any target) — relaxed defaults | Runtime may relax tool gating / governance noise suppressors. Production AKS leaves this unset. |
| `KARS_PROVIDER` | dev creds use `github-models` or `github-copilot` | Runtime **MUST** skip Foundry tool registration in slim modes (model context limit + no Foundry project) |
| `COPILOT_GITHUB_TOKEN` | `KARS_PROVIDER=github-copilot` or McpServer/github CR present | GitHub PAT or Copilot token; used as bearer for inference (when copilot mode) and for github-MCP outbound auth |
| `FOUNDRY_PROJECT_ENDPOINT` | Foundry project bound to sandbox | Project endpoint for foundry_* tools |
| `FOUNDRY_AGENT_ID` | `spec.agent.foundryAgentId` set | Pre-provisioned Foundry agent ID |
| `FOUNDRY_AGENT_TOOLS` | `spec.agent.tools` set | Comma-separated list of allowed Foundry tools |
| `AGT_TRUSTED_PEERS` | Sub-agent spawn | Parent-verified `name:AMID` pairs; runtime seeds these into mesh trust set BEFORE calling `connect()` |
| `PARENT_SANDBOX` | Sub-agent spawn | The parent sandbox's name — receiver of task replies |
| `PARENT_RUNTIME_KIND` (**to be added in A1.2**) | Sub-agent spawn | Parent's runtime kind so child can inherit by default. Today the child always gets OpenClaw image; A1.2 makes spawn runtime-aware. |
| `AGT_REGISTRY_MODE` | governance enabled | `local` (per-sandbox registry namespace) or `global` (handoff target) |
| `AGT_SKIP_ENTRA` | `KARS_DISABLE_ENTRA_AUTH=1` controller env (default `1` in dev; `0` in production with KarsAuthConfig wired) | Skip Entra Agent ID enrolment |
| `MESH_AUTH_BACKEND=EntraAgentIdentity` | `KarsAuthConfig.spec.meshAuthBackend == EntraAgentIdentity` (single-level field, not nested) | Runtime / WS factory should attach Entra bearer on relay connect |
| `MESH_AUTH_AUDIENCE` | `KarsAuthConfig.spec.meshAuthAudience` | Entra token audience for the relay |
| `AUTH_SIDECAR_URL` | Entra Agent ID mode | URL of the cluster-shared auth sidecar Service the router calls for tokens |
| `EXPECTED_TENANT_ID` | Entra Agent ID mode | Pinned Entra tenant; rejects tokens from other tenants |
| `PINNED_AGENT_IDENTITY_APP_ID` | Entra Agent ID provisioned for this sandbox | Per-sandbox Entra app ID; runtime / router MUST use this and reject any inbound query-param mutation. **Currently injected to OpenClaw + router only; A1.2 extends to Hermes.** |
| `CLUSTER_NAME` | helm `meshPeer.clusterName` | Cluster identifier; runtime should include in memory-scope key as `agent:<cluster>/<sandbox>` |
| `TELEGRAM_BOT_TOKEN`, `TELEGRAM_ALLOW_FROM`, `SLACK_BOT_TOKEN`, `DISCORD_BOT_TOKEN` (and `WHATSAPP_ENABLED`) | `kars credentials update` populated `<sandbox>-credentials` Secret | If the runtime declares channel support, entrypoint MUST configure the matching channel in the runtime's native config. Runtimes without channel support ignore. |
| `BRAVE_API_KEY`, `TAVILY_API_KEY`, `EXA_API_KEY`, `FIRECRAWL_API_KEY`, `PERPLEXITY_API_KEY`, `OPENAI_API_KEY` | same Secret | Third-party plugin keys; runtime entrypoint maps to the matching plugin config if supported |
| `KARS_SUPPRESS_EXFIL_URL=1`, `KARS_SUPPRESS_CONTENT_FLAGS=violence`, `KARS_CONTENT_FLAG_MIN_SEVERITY=medium` | `KARS_DEV_PROFILE=true` | Governance noise suppressors for dev sessions |
| `KARS_STRICT_TOOLS=1` | helm `controller.strictTools` | OpenAI strict-mode tool schemas where supported. **Currently OpenClaw-only; A1.2 makes generic.** |
| `KARS_AGT_EVALUATE_FAIL_OPEN_GRACE` | runtime opt-in | Number of consecutive `/agt/evaluate` failures the runtime tolerates before failing closed. Default 3 (OpenClaw compat). Max 10. Set to 0 to fail closed immediately. |

### Runtime-specific (per-platform)

These are **set by the runtime's own entrypoint** based on the kars contract above:

| Env | Runtime | Why |
|---|---|---|
| `OPENCLAW_GATEWAY_TOKEN` | OpenClaw | Secret-keyref to gateway auth; required by OpenClaw 2026.4.x |
| `HERMES_PROFILE` (new) | Hermes | Pin Hermes' multi-profile system to the kars sandbox name |
| `HERMES_DISABLED_BACKENDS=docker,modal,daytona,ssh,singularity` (new) | Hermes | Hermes ships 6 terminal backends; only `local` is valid inside a kars pod |
| `HERMES_HOME=/sandbox/.hermes` (new) | Hermes | Writable state path under read-only rootfs (emptyDir mount) |

---

## File system contract

The controller mounts these paths into the runtime container.

> **Implementation gap (to be closed in A1.2):** Several mounts currently apply only to the OpenClaw branch in `controller/src/reconciler/mod.rs`. The "Status" column tracks current implementation; A1.2 generalizes the runtime-relevant ones to every runtime kind.

| Path | Source | Read by | Status (today) |
|---|---|---|---|
| `/etc/kars/secrets/admin-token` (file: `admin-token`) | Secret `router-admin-token` | Runtime plugin → router HTTP for admin-scope endpoints (`/agt/trust` mutations, `/agt/handoff/*`) | ⚠️ OpenClaw + router only; A1.2 mounts on every v1 runtime |
| `/etc/kars/policies/<n>.yaml` | ConfigMap `toolpolicy-<n>-profile` | Router (not runtime) — runtime calls `/agt/evaluate`, router consumes profiles | ✅ router |
| `/etc/kars/inference/policy.json` | ConfigMap (InferencePolicy compiled) | Router | ✅ router |
| `/etc/kars/memory/binding.json` | ConfigMap (KarsMemory compiled) | Router (runtime's `foundry_memory` tool just uses the `memory-<sandbox>` convention) | ✅ router |
| `/etc/kars/mcp/<server>/meta.json` | ConfigMap (McpServer compiled) | Router (kars-governed MCP) + Runtime entrypoint (translates to native MCP config) | ✅ router; runtime entrypoint translation is A1.9 |
| `/etc/kars/mcp-signing/<server>/jwks.json` | ConfigMap (McpServer signing material) | Router | ✅ router |
| `/etc/kars/egress/allowlist.json` | ConfigMap (egress allowlist) | Router | ✅ router |
| `/etc/kars/egress-approvals/<host>.json` | ConfigMap (per-host EgressApproval) | Router | ✅ router |
| `/etc/kars/a2a-card/agent.json` (optional) | ConfigMap (A2AAgent compiled) | Router (mounts `/.well-known/agent.json` + `/a2a` routes when present) | ✅ router |
| `/etc/kars/trustgraph/projection.json` (optional) | ConfigMap (TrustGraph projection per sandbox) | Runtime + router | ⚠️ TrustGraph reconciler exists; runtime-side consumption is planned |
| `/sandbox/agent/` | OCI artifact or git via `spec.openclaw.config.agentCode` (and per-runtime equivalent) | Runtime entrypoint — user-supplied agent code lands here | ✅ all runtimes that support `agentCode` |
| `/sandbox/.openclaw/`, `/sandbox/.hermes/`, etc. | emptyDir | Runtime writable state (sessions, memory cache, prekeys) | ✅ all runtimes; per-runtime subdir |
| `/tmp` (4 GiB tmpfs by default) | pod spec | Runtime scratch space | ✅ all runtimes |

**Read-only root filesystem**: all of `/`, `/usr`, `/opt`, `/etc` (except mounted ConfigMaps/Secrets) is RO on AKS + local-k8s. Runtimes that need writable state under those paths must mirror to `/tmp` at entrypoint time (see `sandbox-images/openclaw/entrypoint.sh:42-52` for the OpenClaw mirror pattern).

---

## HTTP contract — runtime ↔ router

The runtime plugin talks to the inference-router via `http://127.0.0.1:8443`. **Localhost only** — the egress-guard iptables init container blocks every other destination from UID 1000.

### Public router endpoints (no auth required from same pod)

> Generated from `inference-router/src/main.rs` mount points + `inference-router/src/routes/*.rs` route registrations. This is the minimum stable surface a runtime may rely on.

| Endpoint | Method | Purpose |
|---|---|---|
| `/healthz` | GET | Router liveness |
| `/readyz` | GET | Router readiness (after policy load) |
| `/metrics` | GET | Prometheus scrape (router-side counters) |
| `/v1/chat/completions` | POST | Inference (governance applied: InferencePolicy + Content Safety + token budget + audit) |
| `/v1/completions` | POST | Legacy completions |
| `/v1/responses` | POST | OpenAI Responses API |
| `/v1/embeddings` | POST | Embeddings |
| `/v1/models` | GET | Model catalogue |
| `/v1/deployments` | GET | Foundry deployment list |
| `/v1/images/generations` | POST | Image generation (DALL-E etc.) |
| `/anthropic/v1/messages` (alias `/v1/messages`) | POST | Anthropic Messages API |
| `/v1/mesh-token` | POST | Acquire Entra mesh-auth token (used by runtime / WS factory when `MESH_AUTH_BACKEND=EntraAgentIdentity`) |
| Foundry Agent API: `/agents`, `/threads`, `/runs/*` | various | Foundry Agent API proxy |
| Foundry standalone: `/memory_stores/*`, `/knowledgebases/*`, `/evaluations/*`, `/evaluators/*`, `/evaluationrules/*`, `/evaluationtaxonomies/*`, `/datasets/*`, `/insights/*`, `/connections/*`, `/deployments/*`, `/indexes/*` | various | Foundry data-plane proxies |
| `/openai/vector_stores/*`, `/openai/files/*`, `/openai/containers/*` | various | Foundry OpenAI-compat namespace |
| `/agt/mesh/inbox` | GET | HTTP fallback mesh inbox (rarely used) |
| `/agt/relay` (and `/agt/relay/ws`) | WebSocket | E2E mesh relay proxy |
| `/agt/registry/*` | various | AGT registry HTTP proxy |
| `/mcp` | POST | MCP gateway (kars-governed; reads `/etc/kars/mcp/*/meta.json`) |
| `/platform/mcp` | POST | Platform MCP gateway (Foundry tool catalogue exposed as MCP server) |
| `/blocklist/status` | GET | Blocklist info (read-only) |
| `/blocklist/check` | POST | Check a specific URL/domain |

### Admin-protected router endpoints (require `Authorization: Bearer <admin-token>`; loopback is allowed by default)

| Endpoint | Method | Purpose |
|---|---|---|
| `/agt/evaluate` | POST | Policy decision per action verb |
| `/agt/trust` | GET (list), POST (update) | Trust manager |
| `/agt/trust/{agent_id}` | GET, DELETE | Per-peer trust |
| `/agt/audit`, `/agt/audit/verify` | GET | Audit log + integrity |
| `/agt/status` | GET | Governance summary |
| `/agt/signing-counter` | POST | Plugin pushes Ed25519 sign/verify/reject counts |
| `/agt/rate-limit` | GET, PUT | Dynamic per-tool rate-limit config |
| `/agt/reputation` | GET | Registry-side reputation record (proxied) |
| `/sandbox/spawn` | POST | Create sub-agent CRD (k8s) or container (docker) |
| `/sandbox/list` | GET | List sub-agents |
| `/sandbox/{name}/status` | GET | Sub-agent phase |
| `/sandbox/{name}` | DELETE | Destroy sub-agent |
| `/egress/fetch` | POST | HTTP egress via router (subject to allowlist + Learn-mode rules) |
| `/egress/learn` | POST | Toggle Learn mode |
| `/egress/learned` | GET | Domains seen in Learn mode |
| `/egress/learned/blocked` | GET | Domains blocked but seen |
| `/egress/learned/clear` | POST | Clear learned set |
| `/egress/allowlist` | GET | Current allowlist |
| `/internal/*` | various | Controller↔router (runtimes MUST NOT call) |

### Handoff endpoints (three auth tiers)

| Endpoint | Method | Auth | Purpose |
|---|---|---|---|
| `/agt/handoff/init` | POST | Admin token **only — no loopback bypass** | Mint a one-time handoff token (CLI use) |
| `/agt/handoff/snapshot`, `/restore`, `/verify`, `/drain`, `/decommission`, `/abort`, `/succession` | POST | Admin token + handoff token **no loopback bypass** | Handoff orchestration mutations |
| `/agt/handoff/status`, `/sub-agents`, `/pending`, `/confirm`, `/resume` | GET/POST | Admin token, loopback bypass allowed (read-only) | Handoff status queries |

**Admin token discovery**: read `/etc/kars/secrets/admin-token` synchronously at plugin init. Cache for the process lifetime. The router accepts `Authorization: Bearer <token>` for the protected set; **loopback connections (127.0.0.1 / ::1) bypass the bearer check for admin endpoints** (see `inference-router/src/main.rs:788-804`), but **handoff init + mutation endpoints reject loopback bypass deliberately**. Plugins should always send the token to remain platform-agnostic and to work with handoff endpoints.

---

## Action verb taxonomy (`/agt/evaluate`)

Runtime's `pre_tool_call` hook (or equivalent) **MUST** POST `{action, context: {tool: name}}` to `/agt/evaluate` before executing any tool. The action verb format:

| Pattern | When | Example |
|---|---|---|
| `shell:<command-prefix>` | tools `exec_command`, `foundry_code_execute` | `shell:ls /tmp` |
| `egress:<url>` | tool `http_fetch` | `egress:https://example.com` |
| `tool:<name>:<param-summary>` | any other plugin tool | `tool:foundry_web_search:climate` |
| `mcp:<server>:<tool>` | MCP tool invocation | `mcp:github:list_pull_requests` |
| `a2a:<peer>:<skill>` | A2A invocation | `a2a:writer-bot:summarize` |
| `channel:<platform>:send` | Channel adapter outbound | `channel:telegram:send` |
| `handoff:<phase>:<target>` | Handoff lifecycle | `handoff:request:azclaw2-cloud` |
| `mesh:send:<target>` | `kars_mesh_send` (Act 2) | `mesh:send:auditor` |
| `mesh:transfer_file:<target>` | `kars_mesh_transfer_file` (Act 2) | `mesh:transfer_file:auditor` |
| `memory:<op>` | `foundry_memory` operations | `memory:update`, `memory:search`, `memory:delete_scope` |
| `inference:chat_completions:<model>` | router-internal — runtime doesn't emit | `inference:chat_completions:gpt-4.1` |
| `inference:responses:<model>` | router-internal | — |
| `inference:anthropic_messages:<model>` | router-internal | — |
| `output:<first 200 chars>` | router-internal (post-inference output policy) | — |
| `foundry:<category>:<detail>` | router-internal (Foundry proxy hits) | `foundry:memory:list` |
| `spawn:create:<child>` | router-internal (`/sandbox/spawn`) | — |

### Canonical action construction (MUST follow to avoid policy drift)

For verbs the **runtime** emits (`shell:`, `egress:`, `tool:`, `mcp:`, `a2a:`, `channel:`, `handoff:`, `mesh:`, `memory:`):

1. **Length cap**: the full action string MUST be ≤ 256 bytes (router truncates beyond that for audit/policy purposes; runtime should pre-truncate to stay deterministic).
2. **Param serialization**: for verbs with `<param-summary>`, use the first significant argument (e.g. command for shell, URL for egress, query for web_search) — NOT a JSON dump of all args. Truncate at the cap with `...`.
3. **Secret redaction**: scrub bearer tokens, API keys, passwords, and any value that matches one of the patterns in `agentmesh_mcp::redactor::CredentialKind` (the router redacts again at audit time as defense-in-depth; runtime redaction is mandatory). Replace with `<REDACTED>` literally.
4. **Stable case**: lowercase the verb prefix; the tool name and param are case-preserving.
5. **No newlines**: replace `\n`, `\r`, `\0` with single space.
6. **UTF-8 only**: invalid bytes replaced with `?`.

These rules apply to OpenClaw and Hermes identically — divergence is a runtime bug.

Response shape:
```json
{
  "allowed": true|false,
  "action": "allow"|"deny"|"requires_approval",
  "decision": "allow"|"deny"|"requires_approval",
  "reason": "string" (when denied),
  "matched_rule": "string" (when matched),
  "rate_limited": true|false
}
```

**Fail-closed grace period**: if `/agt/evaluate` is unreachable, the runtime SHOULD allow the first N consecutive failures then fail-closed. Default N=3 (OpenClaw compat); configurable via env `KARS_AGT_EVALUATE_FAIL_OPEN_GRACE` (max 10). Set to 0 to fail closed immediately. After router readiness has been confirmed once (any successful response), subsequent failures may fail-closed immediately at the runtime's discretion.

---

## Identity modes & Entra Agent ID flow

Kars supports four identity modes for the runtime ↔ mesh / upstream auth:

| Mode | When active | Identity material | Mesh-connect auth | Upstream model auth |
|---|---|---|---|---|
| **Anonymous** | `KARS_AUTH_MODE` unset, no governance | runtime-generated Ed25519 keypair | no `Authorization` header on relay connect | Anonymous (router proxies whatever creds operator wired) |
| **Workload Identity** (AKS default) | `KARS_AUTH_MODE=workload-identity` | runtime-generated Ed25519 keypair for mesh; AAD federated cred for upstream | POP-signed connect frame (no Entra token) | AAD token from federated cred via IMDS |
| **API Key** (dev default) | `KARS_AUTH_MODE=api-key` | runtime-generated Ed25519 keypair | POP-signed connect frame | API key from `AZURE_OPENAI_API_KEY` env |
| **Entra Agent Identity** | `MESH_AUTH_BACKEND=EntraAgentIdentity` (per-cluster default in `KarsAuthConfig.spec.meshAuthBackend`, or per-sandbox override in `KarsSandbox.spec.meshAuth.backend`) | Per-sandbox Entra app reg (`PINNED_AGENT_IDENTITY_APP_ID`) for mesh + upstream; runtime still owns Ed25519 keypair for POP | POP-signed connect frame **+** `Authorization: Bearer <entra-token-from-auth-sidecar>` | AAD token from auth sidecar |

### Entra Agent ID flow

When `MESH_AUTH_BACKEND=EntraAgentIdentity`:

1. **Provisioning** (controller-side, one-time per sandbox): controller calls Graph API to create a per-sandbox Entra app registration. The app's `client_id` is written to env `PINNED_AGENT_IDENTITY_APP_ID` on both the router and runtime containers.
2. **Token acquisition** (per request): the runtime or router calls `POST /v1/mesh-token` on the local router. The router forwards to a cluster-shared **auth sidecar** Service (`AUTH_SIDECAR_URL` env) which holds the federated credential and returns a JWT with `aud=<MESH_AUTH_AUDIENCE>` and `appid=<PINNED_AGENT_IDENTITY_APP_ID>`.
3. **Mesh connect**: the runtime's WebSocket factory (`wsFactory` in TS MeshClient, equivalent in Python) intercepts the connect frame and adds `connect.token = <entra-token>` before sending. The relay validates the token (audience + tenant + app pinning matches the registry's record).
4. **Verification**: after first relay-accepted connect, the runtime calls `POST /v1/registry/verify` (proxied via `/agt/registry/v1/registry/verify`) to mark its registry record as Entra-verified. The registry then surfaces `verified_app_id` + `tier: "verified"` on the agent's record.

### Required envs for Entra mode

| Env | Origin | Purpose |
|---|---|---|
| `MESH_AUTH_BACKEND=EntraAgentIdentity` | `KarsAuthConfig.spec.meshAuthBackend` or per-sandbox override | Mode selector |
| `MESH_AUTH_AUDIENCE` | `KarsAuthConfig.spec.meshAuthAudience` | Token audience (e.g. `b712af17-b7f7-419f-a306-b86a607d5a21/.default`) |
| `EXPECTED_TENANT_ID` | controller cluster discovery | Pinned tenant; relay rejects tokens from other tenants |
| `PINNED_AGENT_IDENTITY_APP_ID` | controller per-sandbox provisioning | The sandbox's Entra app reg client ID. **Currently injected to OpenClaw + router only; A1.2 generalizes to all runtimes.** |
| `AUTH_SIDECAR_URL` | helm value | Cluster-shared auth sidecar Service URL the router calls for tokens |
| `AGT_SKIP_ENTRA` | inverse of above — `1` to disable Entra mode for this sandbox even if cluster default is Entra | Default `1` in dev, `0` in AKS production with Entra wired |

### Fail-closed under Entra mode

If `AUTH_SIDECAR_URL` is unreachable, the runtime MUST refuse to initialize mesh (no fallback to anonymous). The router similarly refuses to acquire upstream tokens. Sandboxes started without a reachable auth sidecar mark themselves `Degraded`.

---

## Cross-runtime mesh compatibility

Two runtimes can interoperate on the mesh **only if both implement the kars AGT profile**:

| Required for cross-runtime mesh |
|---|
| AGT v4.0+POP wire format (POP-signed register, Ed25519-Timestamp auth on registry POSTs) |
| Canonical DID derivation: `did:mesh:<sha256(public_key)[:32]>` |
| KNOCK auto-accept policy gated by trust-score threshold |
| Pre-connect trust seeding from `AGT_TRUSTED_PEERS` |
| Registry mode semantics (`local` vs `global`) |
| Relay WebSocket URL: append `/ws` if not present |
| Identity derivation: Ed25519 keypair, base64url-encoded public key in connect frame |
| Entra-aware ws_factory pattern if `MESH_AUTH_BACKEND=EntraAgentIdentity` |

A runtime that fails any of these will fail to handshake (KNOCK), fail to register (POP), or get rejected at the relay (Entra). Wire compatibility ≠ protocol compatibility.

Hermes implements the full kars AGT profile including the mesh tools (`kars_mesh_send`, `_inbox`, `_await`, `_transfer_file`) on the Python AGT MeshClient (`runtimes/agt-mesh-python/`), so encrypted mesh between OpenClaw and Hermes is supported and exercised end-to-end (`tests/e2e/interop/hermes_openclaw_bidi.sh`).

---

## CRD field consumption matrix

What each CRD tells the runtime / controller / router. **Runtime-visible** column shows whether the runtime adapter directly observes the field (via env / mount) vs. controller/router-only.

| CRD | Field | Consumer | Effect | Runtime-visible? |
|---|---|---|---|---|
| `KarsSandbox` | `spec.runtime.kind` | Controller | Image selection (OpenClaw / Hermes / Pydantic-AI / LangGraph / ...); per-runtime env injection | Indirectly (via image choice) |
| | `spec.runtime.<runtime>.*` (e.g. `openclaw`, `hermes`) | Controller | Per-runtime config (image override, version pin) | No |
| | `spec.sandbox.isolation` | Controller | seccomp profile selection (`standard` / `enhanced` / `confidential`) | No |
| | `spec.sandbox.readOnlyRootFilesystem`, `runAsNonRoot`, `allowPrivilegeEscalation`, `seccompProfile`, `writablePaths` | Controller | Pod security context | No |
| | `spec.networkPolicy.egressMode` (`Strict` / `Learn`) | Controller → router env `EGRESS_MODE` | Router blocks unknown domains in Strict, logs in Learn | No |
| | `spec.networkPolicy.approvalRequired` | Controller | Whether forward proxy gates new domains with operator approval | No |
| | `spec.networkPolicy.defaultDeny`, `allowedEndpoints` | Controller | NetworkPolicy generation | No |
| | `spec.governance.toolPolicyRef.name` | Controller | Compile this ToolPolicy into the sandbox's `/etc/kars/policies/` | Indirect (via `/agt/evaluate`) |
| | `spec.governance.trustThreshold` | Controller → runtime env `AGT_TRUST_THRESHOLD` | Trust floor for accepting KNOCKs | ✅ env |
| | `spec.governance.trustedPeers` | Controller → runtime env `AGT_TRUSTED_PEERS` | Pre-verified peer AMIDs (parent-vouched) | ✅ env |
| | `spec.governance.registryMode` (`local` / `global`) | Controller → runtime env `AGT_REGISTRY_MODE` | Registry namespace scope | ✅ env |
| | `spec.governance.mcpServerRefs[]` | Controller | Materialize `/etc/kars/mcp/<server>/meta.json` for each ref | ✅ via mount |
| | `spec.memoryRef.name` | Controller | Mount KarsMemory binding | Indirect |
| | `spec.inferenceRef.name` | Controller | Compile InferencePolicy into router | No |
| | `spec.agent.*` (Foundry Agent config — NOT `spec.foundry`) | Controller → runtime envs `FOUNDRY_AGENT_ID`, `FOUNDRY_AGENT_TOOLS` | Foundry Agent Service binding | ✅ env |
| | `spec.foundry.projectEndpoint` | Controller → runtime env `FOUNDRY_PROJECT_ENDPOINT` | Foundry project URL | ✅ env |
| | `spec.meshAuth.*` (per-sandbox override) | Controller → runtime envs `MESH_AUTH_BACKEND`, `MESH_AUTH_AUDIENCE` | Per-sandbox override of cluster-wide KarsAuthConfig defaults | ✅ env |
| | `spec.a2a.*` | Controller → mount `/etc/kars/a2a-card/agent.json` | Router enables `/.well-known/agent.json` + `POST /a2a` routes | Indirect |
| | `spec.resources.limits/requests` | Controller | Pod resource limits | No |
| | `spec.azureServices.*` | Controller | Federated credential provisioning for `KARS_AUTH_MODE=workload-identity` | No |
| | `spec.suspended: true` | Controller | Scales deployment to 0 replicas | Indirect |
| | `spec.upstreamCompatibility.*` | Controller | BYO contract version compatibility hint | No |
| `InferencePolicy` | `spec.modelPreference.primary.deployment` | Router | Default model for the sandbox | Indirect |
| | `spec.contentSafety.requirePromptShields` | Router | Pre-flight Prompt Shields call | No |
| | `spec.contentSafety.requireContentFilter` | Router | Inline content safety on model output | No |
| | `spec.tokenBudget.dailyTokens` / `monthlyTokens` / `perRequestTokens` | Router | Budget enforcement (429 when exceeded) | No |
| `ToolPolicy` | `spec.agtProfile.inline` (YAML) | Controller compiles → router loads | Per-action allow/deny/approval/rate-limit | Indirect (via `/agt/evaluate`) |
| | `spec.commerce.*` | Controller | Per-tool commerce policy (AP2) | No |
| | `spec.appliesTo.tool` / `sandboxMatchLabels` | Controller | Which sandboxes this policy applies to | No |
| `KarsMemory` | `spec.storeName` | Controller → router | Foundry Memory Store ID for this binding. **MUST match `memory-<sandbox>` convention** for the plugin's `foundry_memory` tool to consume it | ✅ via convention |
| | `spec.scope` (e.g. `agent:<sandbox>`) | Router | Default scope key the memory tool uses | No |
| | `spec.retentionDays` | Router | TTL on records | No |
| | `spec.sandboxRef.name` | Controller | Back-ref binding | No |
| `McpServer` | `spec.url` | Controller → `meta.json` | Upstream MCP server URL | ✅ via mount |
| | `spec.bearerFromEnv` | Controller → `meta.json` | Env var name holding the bearer token | ✅ via mount |
| | `spec.allowedSandboxes.matchLabels` | Controller | Which sandboxes get this MCP server mounted | No |
| | `spec.allowedTools[]` | Controller → `meta.json` | Whitelist of MCP tool names | ✅ via mount |
| `A2AAgent` | `spec.card.*` | Controller → `/etc/kars/a2a-card/agent.json` | Router enables A2A endpoints | Indirect |
| `KarsAuthConfig` | `spec.meshAuthBackend` (single-level, **NOT** `spec.meshAuth.backend`) | Controller → runtime env `MESH_AUTH_BACKEND` | `EntraAgentIdentity` swaps relay-connect to Entra-bearer path | ✅ env |
| | `spec.meshAuthAudience` | Controller → runtime env `MESH_AUTH_AUDIENCE` | Token audience | ✅ env |
| | `spec.foundryRbac.*` | Controller / router | Foundry RBAC role assignments | No |
| | `spec.downstreamApis[]` | Controller / router | Per-API downstream auth config | No |
| `EgressApproval` | `spec.hosts[]` (plural) | Router | Operator-approved egress destinations | No |
| | `spec.effectiveAt`, `expiresAt` | Router | Time-window enforcement | No |
| `TrustGraph` | `spec.peers[]` | Controller projection → mount `/etc/kars/trustgraph/projection.json` | Per-sandbox trust graph (runtime-side consumption planned) | ⚠️ planned |
| `KarsPairing` | `spec.peer.amid`, `spec.peer.endpoint` | Controller | External / offload agent pairing — affects handoff + mesh trust seeding | Indirect |
| `KarsEval` | `spec.targetSandboxRef.name` | Controller | Eval orchestration; failures can mark sandbox Degraded (operationally affects runtime availability) | Indirect |
| | `spec.failSandboxOnDrift` | Controller | Hard-fail behavior on eval regression | No |

---

## Plugin lifecycle contract

A kars runtime plugin (the in-pod adapter living in `runtimes/<runtime>/`) **MUST**:

1. **Initialise BEFORE any user tool can be called.** Discover `SANDBOX_NAME`, read admin token, build the localhost router HTTP client.
2. **Register the AGT pre-flight hook.** Every tool call goes through `/agt/evaluate` first. Block on the response (modulo fail-closed grace).
3. **Register the kars tool set:**
   - Spawn family: `kars_spawn`, `kars_spawn_status`, `kars_spawn_destroy`, `kars_spawn_list`
   - Mesh family: `kars_mesh_send`, `kars_mesh_inbox`, `kars_mesh_await`, `kars_mesh_transfer_file`, `kars_discover` (mesh family requires a working MeshClient — runtimes without mesh ship these tools as stubs that return a clear "mesh not available in this runtime version" error)
   - Handoff family: `kars_handoff_request`, `kars_handoff_confirm`, `kars_handoff_status`
   - Optional Foundry family (when `KARS_PROVIDER` is unset or set to `azure-openai`): `foundry_code_execute`, `foundry_download_file`, `foundry_image_generation`, `foundry_web_search`, `foundry_file_search`, `foundry_memory`, `foundry_conversations`, `foundry_evaluations`, `foundry_deployments`, `foundry_agents`. **MUST** skip when `KARS_PROVIDER` is `github-copilot` or `github-models`.
   - Always-on: `http_fetch` (routes through `/egress/fetch`)
4. **Translate `/etc/kars/mcp/*/meta.json`** into the runtime's native MCP server config at entrypoint time. Tool dispatch then flows through the runtime's MCP client → router `/mcp` → AGT-governed upstream MCP.
5. **Push trust / reputation events** to `/agt/trust` after successful peer interactions; push signing counters to `/agt/signing-counter` on Ed25519 sign/verify/reject.
6. **Connect to mesh** (if mesh capability is available) using:
   - Ed25519 identity material from `/etc/kars/secrets/mesh-key` (if Entra mode) or runtime-generated (default)
   - POP signing on connect frame
   - Ed25519-Timestamp auth on registry POSTs
   - Pre-seed trust set from `AGT_TRUSTED_PEERS` **BEFORE** calling `connect()`
7. **Honour `KARS_DEV_PROFILE`** — when true, relax governance noise output (already auto-suppressed router-side via the three `KARS_SUPPRESS_*` envs).
8. **Run as UID 1000** in AKS / local-k8s (controller pins this); dev docker runs as root inside the container with the runuser shim.

### Plugin MUST NOT
- Call any IP that isn't `127.0.0.1`. The egress-guard iptables drops everything else from UID 1000.
- Write outside `/sandbox/.<runtime>/`, `/tmp`, and `/sandbox/agent/` on AKS.
- Hold provider API keys directly — always proxy inference through the router, which adds the right auth (workload identity, copilot token, etc.).
- Implement its own content safety, token budgeting, or audit logging — the router does these. Runtime-side duplication is forbidden (would create divergent decisions and obscure the audit trail).

---

## Sub-agent contract (spawn / handoff)

When the runtime's `kars_spawn` tool fires:

1. Plugin gathers parent's verified peer AMIDs (parent + already-spawned siblings) into `trusted_peers` string format `"name1:AMID1,name2:AMID2"`.
2. POST `/sandbox/spawn` with `{agent_id, model?, governance: true, trust_threshold: 500, trusted_peers, learn_egress: <KARS_DEV_PROFILE>}`.
3. Router → controller (k8s) or → Docker (dev) creates the child sandbox.
4. Child inherits parent's runtime kind by default (controller reads `PARENT_RUNTIME_KIND` env or CRD `metadata.annotations.kars.azure.com/parent-runtime` — to be added in A1.2).
5. Plugin polls `/sandbox/{name}/status` until `phase=Running`. Optionally also polls AGT registry for mesh registration.
6. On success, plugin broadcasts a `peers_update` message to existing siblings so they trust the newcomer.

---

## Channel credential contract

`kars credentials update --telegram-token X` writes the token to the `<sandbox>-credentials` Secret (k8s) or sets `TELEGRAM_BOT_TOKEN` env on the docker container.

Runtime entrypoint then **MUST**:

| Token env | Runtime entrypoint action |
|---|---|
| `TELEGRAM_BOT_TOKEN` | Configure the runtime's Telegram channel adapter with this token |
| `TELEGRAM_ALLOW_FROM` | Restrict bot to those user IDs |
| `SLACK_BOT_TOKEN` | Slack adapter |
| `DISCORD_BOT_TOKEN` | Discord adapter |
| `WHATSAPP_ENABLED=true` + WhatsApp creds | WhatsApp adapter |
| `BRAVE_API_KEY`, `TAVILY_API_KEY`, `EXA_API_KEY`, `FIRECRAWL_API_KEY`, `PERPLEXITY_API_KEY` | Register the matching plugin's API key |

For runtimes with their own native channel adapters (like Hermes), the entrypoint translates these env vars into the runtime's native config format (e.g. `hermes config set channels.telegram.token=$TELEGRAM_BOT_TOKEN`).

For runtimes without channel support, the env vars are ignored — kars makes no guarantee of channel availability per runtime.

---

## What kars guarantees / does NOT guarantee per runtime

| Capability | kars guarantees on every runtime | kars does NOT guarantee |
|---|---|---|
| Isolation (pod, seccomp, NetworkPolicy, egress-guard) | ✅ | — |
| Inference governance (InferencePolicy, content safety, token budgets, audit) | ✅ (all calls go through router) | — |
| ToolPolicy enforcement | ✅ (router-side via `/agt/evaluate`) | Runtime opting out of `pre_tool_call` hook — that's a runtime bug |
| Audit log of every tool/inference call | ✅ | — |
| Trust scoring | ✅ (push to `/agt/trust`) | — |
| Sub-agent spawn | ✅ (router calls k8s/docker) | — |
| Mesh inter-agent E2E messaging | Only when runtime has a working AGT MeshClient implementation | Cross-runtime mesh is wire-compatible (same AGT v1.0 spec) once both sides have MeshClient |
| Foundry tools | When runtime ships the wrapper (OpenClaw ✅, Hermes ✅) | Other runtimes may have partial coverage |
| Channel adapters (Telegram/Slack/etc.) | When runtime entrypoint translates the kars secret env into its native config | Channels not implemented in the runtime |
| MCP via McpServer CRD | When runtime entrypoint translates the ConfigMap into native MCP config | Runtime without MCP support |

---

## Versioning

This contract is **v1**. Any breaking change to the env vars, HTTP endpoints, or CRD field consumption requires:
- A new `docs/runtimes/CONTRACT.md` document at the next version (`v2`)
- Migration guide
- Deprecation period of at least one minor kars release for the v1 contract

The current contract version is exposed as the env var `KARS_RUNTIME_CONTRACT_VERSION=v1` injected by the controller; runtime plugins should error loudly if they encounter an unsupported version.

---

## Reference implementation

`runtimes/openclaw/` is the reference v1 implementation. Read alongside this spec:
- `runtimes/openclaw/src/index.ts` — plugin entry; AGT policy gate at lines 2825-2845; spawn at `core/agt-tools/agt.ts:188-340`; tool catalogue at `core/agt-task-tools.ts`
- `sandbox-images/openclaw/Dockerfile` — image build pattern
- `sandbox-images/openclaw/entrypoint.sh` — env translation, channel config emission, MCP config translation, runtime startup

`runtimes/hermes/` is the second v1-conformant runtime, at parity with OpenClaw on the spawn, mesh, handoff, discovery, and governance surfaces.
