#!/bin/sh
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# kars OpenAI Agents Python runtime entrypoint.
#
# Pins the LLM endpoint to the router sidecar, points MCP-aware tools
# at the platform MCP server, then invokes the in-pod adapter's
# `bootstrap()` (AAD broker, OTel auto-init, signal handlers) before
# `exec`-ing the user agent code.
#
# Hard rules:
#   - Process must already be running as UID 1000 (Dockerfile USER 1000).
#   - No direct LLM endpoint variables: only `127.0.0.1:8443` is allowed.
#     The egress-guard iptables init container blocks every other path.

set -eu

# Router sidecar — the only LLM endpoint allowed by NetworkPolicy +
# egress-guard. Vanilla `openai` SDK reads OPENAI_BASE_URL.
export OPENAI_BASE_URL="${OPENAI_BASE_URL:-http://127.0.0.1:8443/v1}"

# Platform MCP server: 9 Foundry-shim tools every runtime gets for
# free. The OpenAI Agents SDK MCP client points here.
export KARS_PLATFORM_MCP_URL="${KARS_PLATFORM_MCP_URL:-http://127.0.0.1:8443/platform/mcp}"

# AGT relay/registry — reverse-proxied by the router so AgentMesh
# traffic shares the same governance gate as LLM traffic.
export KARS_AGT_RELAY_URL="${KARS_AGT_RELAY_URL:-http://127.0.0.1:8443/agt/relay/}"
export KARS_AGT_REGISTRY_URL="${KARS_AGT_REGISTRY_URL:-http://127.0.0.1:8443/agt/registry/}"

# OTel collector — the router exposes `/v1/traces` and `/v1/metrics`.
export OTEL_EXPORTER_OTLP_ENDPOINT="${OTEL_EXPORTER_OTLP_ENDPOINT:-http://127.0.0.1:8443/v1/traces}"

# Surface the controller-supplied python version to user code (for
# diagnostics / version-pinning logs).
if [ -n "${RUNTIME_PYTHON_VERSION:-}" ]; then
    echo "[kars-openai-agents] python: ${RUNTIME_PYTHON_VERSION}" >&2
fi

# In-pod adapter bootstrap: AAD broker, OTel init, signal handlers,
# `OPENAI_BASE_URL` pin. Idempotent (guarded by
# `__KARS_RUNTIME_INITIALIZED__`). Non-fatal if telemetry init
# fails — the adapter logs and continues.
python3 -c "from kars_runtime_openai_agents.runtime import bootstrap; bootstrap()"

# If no user agent code is mounted at /sandbox/agent/main.py (operator
# spawned this runtime without `agentCode`), fall back to the bundled
# default smoke-test agent so the pod stays Running and proves the
# Foundry inference path through the router.
if [ ! -f /sandbox/agent/main.py ]; then
    echo "[kars-openai-agents] no /sandbox/agent/main.py — running bundled default agent" >&2
    exec python3 /opt/kars-default-agent/main.py
fi

exec "$@"
