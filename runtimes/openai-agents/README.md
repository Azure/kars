# Kars runtime adapter — OpenAI Agents (Python)

`kars_runtime_openai_agents` is the in-pod adapter that wires the
[OpenAI Agents Python SDK](https://github.com/openai/openai-agents-python)
into the Kars sandbox. It ships inside the
`sandbox-images/openai-agents` container image and is invoked from
`entrypoint.sh` before the user's agent code runs.

## What `bootstrap()` does

1. **AAD token broker** (`aad.py`) — `WorkloadIdentityCredential` with a
   5-minute skew cache. Used by anything in-process that needs a bearer
   for `https://cognitiveservices.azure.com/.default`.
2. **OpenTelemetry** (`otel.py`) — sets up tracer + meter providers with
   an OTLP/HTTP exporter pointed at the router sidecar
   (`OTEL_EXPORTER_OTLP_ENDPOINT`, default
   `http://127.0.0.1:8443/v1/traces`). Auto-instruments `httpx` so every
   LLM call is a span; OpenAI client traffic is captured transitively
   because the SDK uses `httpx`.
3. **AgentMesh bridge** (`mesh.py`) — thin wrapper over the upstream
   `a2a_agentmesh` types (`AgentCard`, `TaskEnvelope`, `TrustGate`).
   Sends and receives messages over the router's `/agt/relay` and
   `/agt/registry` reverse-proxy endpoints.
4. **AgentMesh tools** (`mesh_tools.py`) — registers `mesh_send` and
   `mesh_inbox` as `function_tool`s on a user-supplied `Agent`. The
   `mesh_inbox` description includes an explicit "INBOX-FIRST" nudge so
   models reliably pick up parent messages on resume.
5. **Foundry tools** (`tools.py`) — registers the 9 platform Foundry
   tools as `function_tool`s on a user-supplied `Agent` instance. Each
   tool fans out to the platform MCP server at `/platform/mcp` over
   streamable-HTTP MCP.
6. **Router base URL** — sets `OPENAI_BASE_URL` to the local router so
   the vanilla `openai` SDK (used by `openai-agents`) routes everything
   through the Kars governance gate.

`bootstrap()` is idempotent: if `__KARS_RUNTIME_INITIALIZED__` is
already set in the environment it is a no-op.

## Usage

```python
# main.py — user agent code
from agents import Agent, Runner
from kars_runtime_openai_agents import (
    bootstrap,
    register_foundry_tools,
    register_mesh_tools,
)

bootstrap()  # invoked from entrypoint.sh in production; here for local dev

agent = Agent(name="Researcher", instructions="...")
register_foundry_tools(agent)
register_mesh_tools(agent)  # mesh_send + mesh_inbox first-class tools

result = Runner.run_sync(agent, "What is the weather in Seattle?")
print(result.final_output)
```

## Local development

```bash
cd runtimes/openai-agents
pip install -e ".[dev]"
pytest -v
```

The package depends on `a2a_agentmesh` and `agent_sandbox` from the
upstream agent-governance-toolkit. Build local wheels with
`runtimes/build-agt-wheels.sh` if they are not yet on PyPI for your
target Python.
