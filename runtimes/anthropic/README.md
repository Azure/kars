# kars runtime adapter — Anthropic Claude Agent SDK

`kars_runtime_anthropic` is the in-pod adapter that wires the
[Anthropic Claude Agent SDK](https://github.com/anthropics/claude-agent-sdk-python)
into the kars sandbox. It ships inside the
`sandbox-images/anthropic` container image and is invoked from
`entrypoint.sh` before the user's agent code runs.

## What `bootstrap()` does

1. **AAD token broker** (`aad.py`) — `WorkloadIdentityCredential` with a
   5-minute skew cache. Used by anything in-process that needs a bearer
   for `https://cognitiveservices.azure.com/.default`.
2. **OpenTelemetry** (`otel.py`) — sets up tracer + meter providers
   with an OTLP/HTTP exporter pointed at the router sidecar. Auto-
   instruments `httpx` so every Claude call is a span (the Anthropic
   Python SDK uses `httpx` underneath).
3. **AgentMesh bridge** (`mesh.py`) — thin wrapper over the upstream
   `a2a_agentmesh` types (`AgentCard`, `TaskEnvelope`, `TrustGate`).
   Sends and receives messages over the router's `/agt/relay` and
   `/agt/registry` reverse-proxy endpoints.
4. **Router base URL** — pins `ANTHROPIC_BASE_URL` to
   `http://127.0.0.1:8443/anthropic/v1` so the SDK cannot reach
   `api.anthropic.com` directly. The router sidecar handles AAD
   attestation, content-safety, and credential brokering.
5. **API key sentinel** — sets `ANTHROPIC_API_KEY=router-managed` so
   the SDK constructs without erroring; the router strips this header
   on egress and substitutes its own credential. **No Anthropic API
   keys ever live inside the sandbox pod.**

## Foundry tools

The router sidecar exposes the 9 Foundry MCP tools at
`http://127.0.0.1:8443/platform/mcp`. The Claude Agent SDK supports MCP
servers natively via its `mcp_servers=[...]` option, so user code wires
the platform MCP server in directly:

```python
from claude_agent_sdk import Agent
from kars_runtime_anthropic import bootstrap, PLATFORM_MCP_URL

bootstrap()  # idempotent

agent = Agent(
    model="claude-sonnet-4",
    mcp_servers=[{"url": PLATFORM_MCP_URL, "transport": "http-stream"}],
)
```

The platform MCP server publishes the canonical 9-tool catalog mirrored
from `inference-router/src/mcp/platform.rs::foundry_tool_catalog`. No
SDK-specific wrapper module is needed (and doing so would be a
duplication of upstream MCP transport logic).

## Container contract

- Process runs as UID 1000 (egress-guard pin in `reconciler/mod.rs`).
- LLM traffic goes via `http://127.0.0.1:8443/anthropic/v1` (router sidecar).
- Reads MCP gateway from `KARS_PLATFORM_MCP_URL` (defaults to the
  in-cluster MCP server).
- Honors `KARS_*` env wiring (router URL, AGT relay URL, identity SAN).
- Does not bind privileged ports; default `securityContext` only.

## Building the wheel

```bash
cd runtimes/anthropic
pip install build
python -m build --wheel
```

The Dockerfile installs from `runtimes/wheels/` so build the AGT-Python
wheels first via `runtimes/build-agt-wheels.sh`.
