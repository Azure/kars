# kars runtime adapter — Microsoft Agent Framework (Python)

`kars_runtime_maf_python` is the in-pod adapter that wires the
[Microsoft Agent Framework Python SDK](https://github.com/microsoft/agent-framework)
into the kars sandbox. It ships inside the
`sandbox-images/maf-python` container image and is invoked from
`entrypoint.sh` before the user's agent code runs.

## What `bootstrap()` does

1. **AAD token broker** (`aad.py`) — `WorkloadIdentityCredential` with a
   5-minute skew cache (matches the OpenAI-Agents adapter so cross-runtime
   helpers behave identically).
2. **OpenTelemetry** (`otel.py`) — sets up tracer + meter providers with
   an OTLP/HTTP exporter pointed at the router sidecar
   (`OTEL_EXPORTER_OTLP_ENDPOINT`, default
   `http://127.0.0.1:8443/v1/traces`). Auto-instruments `httpx`.
3. **AgentMesh bridge** (`mesh.py`) — thin wrapper over
   `a2a_agentmesh` types, transported over the router's
   `/agt/relay` and `/agt/registry` reverse-proxy endpoints.
4. **Foundry tools** (`tools.py`) — wraps each of the 9 platform
   Foundry tools as a MAF `@tool`-decorated callable that delegates to
   the platform MCP server. `register_foundry_tools(agent)` attaches
   them to a MAF `ChatAgent` (or any object with a mutable
   `tools` attribute).
5. **Router base URLs** — sets `OPENAI_BASE_URL` and
   `AZURE_OPENAI_ENDPOINT` to the local router so `OpenAIChatClient`
   and `AzureOpenAIChatClient` route through the kars governance
   gate.

`bootstrap()` is idempotent via the `__KARS_RUNTIME_INITIALIZED__`
env var.

## Usage

```python
# main.py — user agent code
from agent_framework import ChatAgent
from agent_framework.openai import OpenAIChatClient
from kars_runtime_maf_python import bootstrap, register_foundry_tools

bootstrap()  # invoked from entrypoint.sh in production

agent = ChatAgent(
    name="Researcher",
    chat_client=OpenAIChatClient(model_id="gpt-4o-mini"),
)
register_foundry_tools(agent)

response = await agent.run("What's new this week?")
print(response)
```

## Local development

```bash
cd runtimes/maf-python
pip install -e ".[dev]"
pytest -v
```
