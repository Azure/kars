# kars-runtime-langgraph

In-pod adapter for the [LangGraph](https://github.com/langchain-ai/langgraph)
agent framework (Python). Ships pre-installed in the
`kars-runtime-langgraph` sandbox image; user agent code only needs to
build the graph.

## What this adapter does

| Concern | Behaviour |
|---|---|
| **LLM provider routing** | Pins `OPENAI_BASE_URL`, `AZURE_OPENAI_ENDPOINT`, `ANTHROPIC_BASE_URL` to the inference-router sidecar at bootstrap so LangChain model factories cannot reach the public endpoints directly. |
| **API keys** | Each provider API-key env (`OPENAI_API_KEY`, `AZURE_OPENAI_API_KEY`, `ANTHROPIC_API_KEY`) is set to the literal `router-managed`. The router strips this header on egress and substitutes the real AAD-attested credential. **No real provider key ever lives in the sandbox pod.** |
| **AAD broker** | `aad.py` re-exposes the IMDS / Workload Identity broker used by every other kars runtime. |
| **AgentMesh** | `mesh.py` wires the `a2a_agentmesh` SDK to the in-cluster relay so LangGraph nodes can `spawn` / `handoff` to peer agents under AGT trust gating. |
| **OTel** | `otel.py` auto-initialises tracing/metrics with `service.name=kars-runtime-langgraph`. |

## Usage

```python
from kars_runtime_langgraph import bootstrap

bootstrap()  # idempotent; safe to call multiple times

from langgraph.graph import StateGraph
from langchain_openai import ChatOpenAI

llm = ChatOpenAI(model="gpt-4o-mini")  # OPENAI_BASE_URL already pinned by bootstrap()
# ... build your graph normally
```

The entrypoint script (`sandbox-images/langgraph/entrypoint.sh`) calls
`bootstrap()` for you before invoking your code, so the example above
works out of the box.

## Why a dedicated adapter (vs BYO)

The LangChain provider factories construct their HTTP clients at
import / instantiation time and read env vars **once**. Without a
bootstrap that pins the base URLs *before* the user's `from
langchain_openai import ChatOpenAI` line runs, the SDK would either
fail (no key) or attempt direct egress (blocked by the egress-guard).
The adapter inverts this by populating the env at pod start.

## Deferred

- **TypeScript / `@langchain/langgraph`** — Python adapter ships
  first; TS is a follow-up phase. The CRD already declares
  `language: typescript`; the controller currently rejects it with a
  clear `ShapeInvalid` error.
- **Per-graph-node MCP wiring** — LangGraph nodes can already call
  the platform MCP server at `http://127.0.0.1:8443/platform/mcp`
  via any standard MCP client. We do not ship a dedicated wrapper.
