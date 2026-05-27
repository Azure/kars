#!/bin/sh
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# kars LangGraph (LangChain.js, TypeScript / Node.js 22) runtime entrypoint.
#
# Pins each known LLM provider base URL to the router sidecar, points
# MCP-aware tools at the platform MCP server, then invokes the in-pod
# adapter's `bootstrap()` (AAD broker, OTel auto-init, signal
# handlers, provider base-URL pins, API-key sentinels) before
# `exec`-ing the user agent code.
#
# Hard rules:
#   - Process must already be running as UID 1000 (Dockerfile USER 1000).
#   - No direct LLM endpoint variables: only `127.0.0.1:8443` is allowed.
#     The egress-guard iptables init container blocks every other path.
#   - No real provider API key ever reaches this pod. Each provider
#     API-key env (OPENAI_API_KEY, AZURE_OPENAI_API_KEY,
#     ANTHROPIC_API_KEY) is set to a sentinel so LangChain factories
#     construct without erroring; the router sidecar substitutes the
#     real credential on egress.

set -eu

# Provider base URLs — all routed through the inference-router
# sidecar. LangChain.js factories read these at construction time
# (ChatOpenAI, AzureChatOpenAI, ChatAnthropic, ...).
export OPENAI_BASE_URL="${OPENAI_BASE_URL:-http://127.0.0.1:8443/openai/v1}"
export AZURE_OPENAI_ENDPOINT="${AZURE_OPENAI_ENDPOINT:-http://127.0.0.1:8443/azure-openai}"
export ANTHROPIC_BASE_URL="${ANTHROPIC_BASE_URL:-http://127.0.0.1:8443/anthropic/v1}"

# Sentinel API keys — router strips on egress. NEVER real keys.
export OPENAI_API_KEY="${OPENAI_API_KEY:-router-managed}"
export AZURE_OPENAI_API_KEY="${AZURE_OPENAI_API_KEY:-router-managed}"
export ANTHROPIC_API_KEY="${ANTHROPIC_API_KEY:-router-managed}"

# Platform MCP server: 9 Foundry-shim tools every runtime gets for
# free. LangGraph nodes can call this via any MCP client.
export KARS_PLATFORM_MCP_URL="${KARS_PLATFORM_MCP_URL:-http://127.0.0.1:8443/platform/mcp}"

# AGT relay/registry — reverse-proxied by the router so AgentMesh
# traffic shares the same governance gate as LLM traffic.
export KARS_AGT_RELAY_URL="${KARS_AGT_RELAY_URL:-http://127.0.0.1:8443/agt/relay/}"
export KARS_AGT_REGISTRY_URL="${KARS_AGT_REGISTRY_URL:-http://127.0.0.1:8443/agt/registry/}"

# OTel collector — the router exposes `/v1/traces` and `/v1/metrics`.
export OTEL_EXPORTER_OTLP_ENDPOINT="${OTEL_EXPORTER_OTLP_ENDPOINT:-http://127.0.0.1:8443/v1/traces}"

# Surface the controller-supplied language flag (typescript here).
if [ -n "${RUNTIME_LANGGRAPH_LANGUAGE:-}" ]; then
    echo "[kars-langgraph-ts] language: ${RUNTIME_LANGGRAPH_LANGUAGE}" >&2
fi

# In-pod adapter bootstrap. Idempotent (guarded by
# `__KARS_RUNTIME_INITIALIZED__`). Non-fatal if telemetry init
# fails — the adapter logs and continues.
node -e "require('/opt/kars-runtime-langgraph-ts/dist/index.js').bootstrap().catch((e)=>{console.warn('[kars-langgraph-ts] bootstrap failed:', e); process.exit(0);}).then(()=>{})" || true

# Make the adapter resolvable from the user agent code via either the
# package name (`require('@kars/runtime-langgraph-ts')`) or
# absolute path. The package is already installed in
# /opt/kars-runtime-langgraph-ts/node_modules; expose its parent.
export NODE_PATH="${NODE_PATH:-}:/opt/kars-runtime-langgraph-ts/node_modules:/opt/kars-runtime-langgraph-ts"

exec "$@"
