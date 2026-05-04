# Phase H#2 — LangGraph Runtime Adapter

**Status:** complete
**Date:** 2026-05-03
**Scope:** Add LangGraph (LangChain) Python as a first-class AzureClaw
runtime alongside OpenClaw, OpenAI Agents SDK, MAF, Anthropic, and BYO.
CRD already declared `RuntimeKind::LangGraph` and `LangGraphConfig`
(with `language: python | typescript`); this phase replaces the
`AdapterMissing("LangGraph")` short-circuit with a real producer
(`plan_langgraph`), ships the Python adapter package
(`runtimes/langgraph/`), and the sandbox image
(`sandbox-images/langgraph/`). TypeScript flavour is gated as
`ShapeInvalid` until the TS image ships (mirrors the MAF .NET strategy).

## Components

| Component | Location | Notes |
|---|---|---|
| CRD wiring (already merged) | `controller/src/crd.rs:565` (`LangGraphConfig`) | No schema change. |
| Planner | `controller/src/reconciler/runtime.rs::plan_langgraph` | Mirrors `plan_microsoft_agent_framework` (language-gated). |
| Default image helper | `runtime.rs::langgraph_default_image()` | Honours `LANGGRAPH_RUNTIME_IMAGE` env override; falls back to `azureclawacr.azurecr.io/azureclaw-runtime-langgraph:latest`. |
| Python adapter | `runtimes/langgraph/` | 5 modules. aad/mesh/otel ported from `anthropic`/`openai-agents`. `runtime.py` pins **multiple** provider base URLs (OPENAI/AZURE_OPENAI/ANTHROPIC) since LangGraph is provider-agnostic. |
| Sandbox image | `sandbox-images/langgraph/` | `mariner-python:3.12`. UID 1000. AGT wheels overlaid. |
| E2E gate | `tests/e2e/run.sh::test_runtime_langgraph` + `test_runtime_langgraph_typescript_gated` | Asserts namespace creation for python; asserts ShapeInvalid condition for typescript. |

## Threat-model delta (STRIDE)

LangGraph inherits the per-sandbox isolation, egress-guard, and AGT
trust gating of every other adapter. Delta-only review:

### S — Spoofing
**No new identity surface.** Adapter reuses `aad.py` (verbatim) — same
IMDS / Workload Identity broker. No new federated credential.

### T — Tampering
**No new mutable persistence.** Adapter is read-mostly: pins env vars
at bootstrap, then yields control to user graph code. Trust scoring
still flows through AGT TrustManager — adapter does not write.

### R — Repudiation
**Inherits OTel + AGT audit chain unchanged.** `otel.py` is the
verbatim adapter. LangChain's own callbacks layer is opaque to us
but its outbound HTTP calls flow through the router, so the audit
chain still sees every model call.

### I — Information disclosure  *(primary risk vector)*
**Mitigation: multi-provider key sentinels.** LangGraph itself is
provider-agnostic — a typical graph invokes `ChatOpenAI`,
`AzureChatOpenAI`, `ChatAnthropic`, etc. Each LangChain factory
reads its provider's API-key env at construction time. The adapter
sets each known key env to `router-managed`. The router strips
those headers on egress and substitutes the real AAD-attested
credential. **No real provider key ever lives in the sandbox pod.**

Provider base URLs pinned at bootstrap:
- `OPENAI_BASE_URL` → `http://127.0.0.1:8443/openai/v1`
- `AZURE_OPENAI_ENDPOINT` → `http://127.0.0.1:8443/azure-openai`
- `ANTHROPIC_BASE_URL` → `http://127.0.0.1:8443/anthropic/v1`

API-key envs sentinel'd at bootstrap:
- `OPENAI_API_KEY` → `router-managed`
- `AZURE_OPENAI_API_KEY` → `router-managed`
- `ANTHROPIC_API_KEY` → `router-managed`

Defence in depth:
1. Base URLs are pinned **before** user `from langchain_openai
   import ChatOpenAI` runs. LangChain factories cache their config
   from env at instantiation time, so a later mutation by user code
   would only affect new factory instances.
2. The egress-guard iptables init container drops UID-1000 packets
   destined for non-loopback / non-DNS targets — even if a base URL
   were leaked, the SDK couldn't reach the public endpoint.
3. The router enforces the same Content Safety + AGT + AP2 +
   ToolPolicy pipeline as for any other provider.

**Residual risk** *(documented, accepted)*: LangChain may add new
providers we haven't pre-pinned. Those would fall through to the
provider's default base URL — which is then blocked by egress-guard.
The user gets a connection-refused error rather than a silent
egress. Mitigation: add new providers to `PROVIDER_BASE_URLS` in
`runtime.py` as the LangChain ecosystem evolves.

### D — Denial of service
**No new amplification.** LangGraph's graph-execution model can
produce loops (`add_edge(a, b); add_edge(b, a)`); upstream
`recursion_limit` defaults to 25. Existing rate-limiting and
token-budget enforcement in the router apply uniformly.

### E — Elevation of privilege
**No new capability.** UID 1000, seccomp-strict, NetworkPolicy, CRD
RBAC unchanged. No `CAP_NET_RAW`, no `setuid`, no privileged volumes.

## Design choices (auditable)

### Multi-provider env pins (vs single provider)
The Anthropic adapter pins one provider (`ANTHROPIC_BASE_URL`)
because the SDK is single-provider. LangGraph is intentionally
provider-agnostic — the same graph might mix OpenAI and Anthropic
nodes. Pinning only one would leave the others free to egress, so
the adapter pins all three known providers. This is documented in
the adapter README.

### TypeScript gated as `ShapeInvalid`
Mirrors the MAF .NET strategy. Operators get a clear error in the
Conditions chain rather than a silently mis-imaged pod. The CRD
already accepts the value (it's a valid enum); the controller's
`plan_langgraph` rejects with a message that explicitly names the
deferred state and points at the recommended workaround
(`language: python` or wait for the TS adapter).

### No SDK-specific MCP wrapper
Same reasoning as Anthropic Phase H#1 — LangGraph nodes can call
the platform MCP server (`http://127.0.0.1:8443/platform/mcp`) via
any standard MCP client. Wrapping the 9 tools as LangChain
`Tool`-decorated callables would duplicate transport logic for no
governance benefit.

### Reused `aad.py`, `mesh.py`, `otel.py` verbatim
Same as Anthropic — only package + service-name renames. Minimises
audit surface.

### Producer keeps reserved-prefix env clean
`plan_langgraph` only emits `RUNTIME_LANGGRAPH_LANGUAGE`
(non-reserved) plus the user's `extra_env`. The deployment builder
owns the `OPENAI_*` / `ANTHROPIC_*` / `AZURE_*` namespaces — they
are filtered in any user `extra_env` before being injected.

## Deferred items
- **TypeScript / `@langchain/langgraph`** — Python adapter ships
  first; TS image is a separate follow-up phase.
- **Per-graph-node MCP wiring** — user code calls the platform MCP
  server directly via any MCP client.
- **LangSmith integration** — LangChain's tracing platform is
  out-of-band by design (sends telemetry to LangSmith Cloud). Not
  enabled in the adapter — would conflict with the no-direct-egress
  policy. Users who need it can configure their own LangSmith
  endpoint and rely on the OTel-via-router path for governance.

## Verification

- `cargo build --package azureclaw-controller` ✅ clean
- `cargo clippy --package azureclaw-controller --all-targets -- -D warnings` ✅ clean
- `cargo test --package azureclaw-controller --bin azureclaw-controller reconciler::runtime -- --test-threads=1` ✅ 40/40 (5 new LangGraph tests)
- `tests/e2e/run.sh::test_runtime_langgraph` and
  `test_runtime_langgraph_typescript_gated` registered for both
  `AZURECLAW_E2E_RUNTIME=langgraph` and `=all` lanes.

## CI gates passed locally
- `BASE_REF=origin/dev bash ci/check-loc.sh`
- `BASE_REF=origin/dev bash ci/no-stubs.sh`
- `BASE_REF=origin/dev bash ci/no-custom-crypto.sh`
- `BASE_REF=origin/dev bash ci/security-audit-required.sh`
- `BASE_REF=origin/dev bash ci/check-copyright-headers.sh`
