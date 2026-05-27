# syntax=docker/dockerfile:1.7
#
# kars runtime image: LangGraph (LangChain.js) for TypeScript /
# Node.js 22.
#
# Ships the in-pod adapter package `@kars/runtime-langgraph-ts`,
# which is invoked from `entrypoint.sh` before the user agent code
# runs. The adapter:
#   - pins each LLM provider base URL (OPENAI_BASE_URL,
#     AZURE_OPENAI_ENDPOINT, ANTHROPIC_BASE_URL) to the router sidecar;
#   - sets each provider API-key env to a sentinel — router
#     substitutes the real cred on egress;
#   - initializes OpenTelemetry against the router's OTLP collector;
#   - brokers AAD tokens via Workload Identity (5-min skew cache);
#   - bridges AgentMesh A2A messages over `/agt/relay`.
#
# Foundry tools (9 platform MCP tools) are reachable at
# `http://127.0.0.1:8443/platform/mcp` via any MCP client — LangGraph
# nodes can invoke them directly. No SDK-specific tool wrapper module
# is shipped.
#
# Contract this image must honor:
#   - Process runs as UID 1000 (egress-guard pin in `reconciler/mod.rs`).
#   - LLM traffic goes via `http://127.0.0.1:8443` (router sidecar).
#   - Reads MCP gateway from `KARS_PLATFORM_MCP_URL`.
#   - Honors `KARS_*` env wiring (router URL, AGT relay URL, identity SAN).
#   - Does not bind privileged ports; default `securityContext` only.
#
# Build context MUST be the repo root (the COPY paths are anchored at
# `runtimes/langgraph-ts/` and `sandbox-images/langgraph-ts/`).

FROM mcr.microsoft.com/azurelinux/base/nodejs:24 AS builder

WORKDIR /build/runtime

COPY runtimes/langgraph-ts/package.json runtimes/langgraph-ts/tsconfig.json ./
RUN npm install --no-audit --no-fund --no-package-lock

COPY runtimes/langgraph-ts/src ./src
RUN ./node_modules/.bin/tsc -p .

# ---- Production stage ----------------------------------------------------
FROM mcr.microsoft.com/azurelinux/base/nodejs:24

LABEL org.kars.runtime.contract="v1"
LABEL org.kars.runtime.kind="LangGraph"
LABEL org.kars.runtime.language="typescript"

WORKDIR /opt/kars-runtime-langgraph-ts

COPY runtimes/langgraph-ts/package.json ./
RUN npm install --omit=dev --no-audit --no-fund --no-package-lock

COPY --from=builder /build/runtime/dist ./dist

# ---- Adapter entrypoint --------------------------------------------------
COPY sandbox-images/langgraph-ts/entrypoint.sh /usr/local/bin/kars-langgraph-ts-entrypoint.sh
RUN chmod 0755 /usr/local/bin/kars-langgraph-ts-entrypoint.sh

# Workspace where the controller mounts user-supplied agent code via
# `agentCode: { oci | git }` (see `LangGraphConfig` in crd.rs).
RUN mkdir -p /sandbox/agent && chown -R 1000:1000 /sandbox

USER 1000
WORKDIR /sandbox/agent

ENTRYPOINT ["/usr/local/bin/kars-langgraph-ts-entrypoint.sh"]
CMD ["node", "/sandbox/agent/index.js"]
