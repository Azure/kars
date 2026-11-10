<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# `@kars/runtime-langgraph-ts`

In-pod adapter for **LangGraph (TypeScript / Node.js 22)** running on
kars. Mirrors the Python adapter at
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
npm run typecheck
npm run build
npm test
```

The sandbox image (`sandbox-images/langgraph-ts/`) installs the
already-built `dist/` output. Its builder and production dependency stages both
use the committed lockfile with `npm ci`.

Azure Identity requires `^4.14.1`, whose maintained parent dependency requires
MSAL Node `^7.1.0`; the lock currently resolves MSAL 7.2.0. This addresses
CVE-2026-109461 without an unsupported major override. The minimum is in the
published library's dependencies, not just a repository-root override, so fresh
consumer resolution also excludes the affected MSAL range. Node >=22 remains
required. `tests/identity.test.ts` checks installed and locked versions and runs
the real Workload Identity → MSAL assertion exchange, cache, and rejection paths
with a test-only HTTP transport. These are local compatibility checks, not live
Entra acceptance.

The adapter also supplies LangGraph's supported Zod peer as `^4.6.6` rather than
allowing the historical Zod 3 resolution. This repairs the additional local-audit
finding GHSA-mrhx-c7xc-m3w5 (oversized constrained collection validation).
`tests/schema-compatibility.test.ts` exercises real LangChain structured tools
and LangGraph schema execution with the supported Zod 4 API.

Telemetry initialization targets the locked OpenTelemetry 2.x SDK APIs. The
regressions use real SDK providers and a local HTTP collector to verify trace
and metric exports, resource attributes, endpoint precedence and idempotent
initialization. CI runs these checks under the existing required runtime build
job; a best-effort initialization warning is not accepted as working telemetry.
