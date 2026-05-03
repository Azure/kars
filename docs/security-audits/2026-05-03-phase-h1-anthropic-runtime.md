# Phase H#1 — Anthropic Claude Agent SDK Runtime Adapter

**Status:** complete
**Date:** 2026-05-03
**Scope:** Add Anthropic's Claude Agent SDK as a first-class AzureClaw
runtime alongside OpenClaw, OpenAI Agents SDK, MAF, and BYO. CRD already
declared `RuntimeKind::Anthropic` and `AnthropicConfig`; this phase
replaces the `AdapterMissing("Anthropic")` short-circuit with a real
producer (`plan_anthropic`), ships the Python adapter package
(`runtimes/anthropic/`) and the sandbox image
(`sandbox-images/anthropic/`).

## Components

| Component | Location | Notes |
|---|---|---|
| CRD wiring (already merged) | `controller/src/crd.rs:589` (`AnthropicConfig`) | No schema change. |
| Planner | `controller/src/reconciler/runtime.rs::plan_anthropic` | Mirrors `plan_openai_agents`. |
| Default image helper | `runtime.rs::anthropic_default_image()` | Honours `ANTHROPIC_RUNTIME_IMAGE` env override; falls back to `azureclawacr.azurecr.io/azureclaw-runtime-anthropic:latest`. |
| Python adapter | `runtimes/anthropic/` | 5 modules (aad, mesh, otel, runtime, `__init__`). aad/mesh/otel ported verbatim from `openai-agents`. |
| Sandbox image | `sandbox-images/anthropic/` | Base `mariner-python:3.12`. UID 1000. AGT wheels overlaid. |
| E2E gate | `tests/e2e/run.sh::test_runtime_anthropic` | Asserts namespace creation + (when reconciled) image carries the `anthropic` tag. |

## Threat-model delta (STRIDE)

The Anthropic adapter inherits the per-sandbox isolation, egress-guard,
and AGT trust gating of the existing runtime adapters. The delta-only
review:

### S — Spoofing
**No new identity surface.** Adapter reuses `aad.py` (verbatim) — same
IMDS / Workload Identity broker as OpenAI Agents. No new client cert,
no new federated credential, no new audience.

### T — Tampering
**No new mutable persistence.** Adapter is read-mostly: it consumes
`PLATFORM_MCP_URL`, `AGT_RELAY_URL`, `OTEL_*` env vars set by the
controller. Trust scoring still flows through `AGT_TRUST_THRESHOLD`
and the AGT TrustManager — adapter does not write to that store.

### R — Repudiation
**Inherits OTel + AGT audit chain unchanged.** `otel.py` is the
verbatim adapter; the inference router still produces the
hash-chained / Phase-D-Merkle audit log. No bypass.

### I — Information disclosure  *(primary risk vector)*
**Mitigation: router-managed key sentinel.** The Claude Agent SDK
refuses to construct without `ANTHROPIC_API_KEY` set. The adapter
sets it to the literal string `router-managed`. The real
Anthropic-compatible credential is materialised by the
inference-router on egress (it strips the sentinel header and
substitutes the AAD-attested provider credential). **No real
Anthropic key ever lives in the sandbox pod's process memory or env.**

Defence in depth:
1. `ANTHROPIC_BASE_URL` is pinned to `http://127.0.0.1:8443/anthropic/v1`
   so the SDK is *unable* to dial `api.anthropic.com` directly.
2. The egress-guard iptables init container drops UID-1000 packets
   destined for non-loopback / non-DNS targets — the SDK couldn't
   reach the public Anthropic endpoint even if base-url were leaked.
3. The router enforces the same Content Safety + AGT + AP2 +
   ToolPolicy pipeline as for any other provider.

### D — Denial of service
**No new amplification.** The adapter does not introduce retry loops
or request fan-out beyond what the SDK itself produces. Existing
rate-limiting (RateLimiter component) and token-budget enforcement
in the router apply uniformly across providers.

### E — Elevation of privilege
**No new capability.** UID 1000, seccomp-strict, NetworkPolicy, CRD
RBAC unchanged. No `CAP_NET_RAW`, no `setuid`, no privileged volumes.
The image inherits the same hardening baseline as the OpenAI Agents
runtime image.

## Design choices (auditable)

### No `tools.py` module
The OpenAI Agents adapter exposes a 184-LOC `tools.py` that wraps
the platform MCP server's tools as `agents.function_tool`-decorated
callables. **Claude Agent SDK supports MCP servers natively** via
`mcp_servers=[...]` on `ClaudeAgentOptions`. Wrapping the MCP server
again would duplicate transport logic and create a second governance
seam. User code wires the platform MCP URL directly:

```python
from claude_agent_sdk import ClaudeAgent, ClaudeAgentOptions

agent = ClaudeAgent(options=ClaudeAgentOptions(
    mcp_servers=[{"url": os.environ["PLATFORM_MCP_URL"]}],
))
```

The platform MCP server is the same one OpenAI Agents and OpenClaw
talk to, so the governance gate is identical.

### Reused `aad.py`, `mesh.py`, `otel.py` verbatim
Only the Python package name and OTel service-name string differ.
Reuse minimises the audit surface — no new transport, no new crypto,
no new IMDS handler.

### Producer keeps reserved-prefix env clean
`plan_anthropic` only emits `RUNTIME_PYTHON_VERSION` (non-reserved)
plus the user's `extra_env` map. The deployment builder owns the
`ANTHROPIC_*` and `AZURECLAW_*` namespaces.

## Deferred items
- **TypeScript variant of the Claude Agent SDK** — Claude SDK is
  Python-first; TS support is incomplete upstream. Re-evaluate when
  Anthropic ships parity.
- **SDK-specific tool wrappers** — only worth shipping if a customer
  needs adapter-side tools that aren't expressible via MCP. None
  identified at this time.
- **Telegram / Slack channel hooks** — Claude Agent SDK doesn't ship
  channel adapters; deferred to a future phase that adds a generic
  channel bridge.

## Verification

- `cargo build --package azureclaw-controller` ✅ clean
- `cargo clippy --package azureclaw-controller --all-targets -- -D warnings` ✅ clean
- `cargo test --package azureclaw-controller --bin azureclaw-controller reconciler::runtime -- --test-threads=1` ✅ 35/35 (4 new Anthropic tests)
- `tests/e2e/run.sh::test_runtime_anthropic` registered for both
  `AZURECLAW_E2E_RUNTIME=anthropic` and `=all` lanes.

## CI gates passed locally
- `BASE_REF=origin/dev bash ci/check-loc.sh`
- `BASE_REF=origin/dev bash ci/no-stubs.sh`
- `BASE_REF=origin/dev bash ci/no-custom-crypto.sh`
- `BASE_REF=origin/dev bash ci/security-audit-required.sh`
- `BASE_REF=origin/dev bash ci/check-copyright-headers.sh`
