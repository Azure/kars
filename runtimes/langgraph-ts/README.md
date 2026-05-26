# `@kars/runtime-langgraph-ts`

In-pod adapter for **LangGraph (TypeScript / Node.js 22)** running on
Kars. Mirrors the Python adapter at
[`runtimes/langgraph/`](../langgraph) with the same v1 contract:

- pins each LLM provider base URL (`OPENAI_BASE_URL`,
  `AZURE_OPENAI_ENDPOINT`, `ANTHROPIC_BASE_URL`) to the router sidecar
  at `http://127.0.0.1:8443/...`;
- stamps each provider API-key env with the `router-managed` sentinel
  so LangChain factories construct without erroring — the router
  substitutes the real credential on egress;
- initializes OpenTelemetry (traces + metrics) against the router's
  OTLP/HTTP collector;
- exposes a thin `MeshClient` over the router's reverse-proxied
  `/agt/relay` and `/agt/registry` endpoints for AgentMesh
  inter-agent comms;
- brokers AAD tokens via `WorkloadIdentityCredential` with a 5-minute
  skew cache.

Foundry's 9 platform MCP tools are reachable at
`http://127.0.0.1:8443/platform/mcp` via any MCP client — LangGraph
nodes can invoke them directly.

## Usage

The sandbox image's `entrypoint.sh` calls `bootstrap()` automatically;
user agent code does not normally import this package.

```ts
import { bootstrap } from '@kars/runtime-langgraph-ts';
await bootstrap();
// ... user graph code ...
```

## Build

```bash
npm ci
npm run build
```

The sandbox image (`sandbox-images/langgraph-ts/`) installs the
already-built `dist/` output.
