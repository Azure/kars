#!/bin/sh
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# AzureClaw Anthropic Claude Agent SDK runtime entrypoint.
#
# Pins the LLM endpoint to the router sidecar, points MCP-aware tools
# at the platform MCP server, then invokes the in-pod adapter's
# `bootstrap()` (AAD broker, OTel auto-init, signal handlers,
# ANTHROPIC_BASE_URL pin, ANTHROPIC_API_KEY sentinel) before
# `exec`-ing the user agent code.
#
# Hard rules:
#   - Process must already be running as UID 1000 (Dockerfile USER 1000).
#   - No direct LLM endpoint variables: only `127.0.0.1:8443` is allowed.
#     The egress-guard iptables init container blocks every other path.
#   - No real Anthropic API key ever reaches this pod. The
#     ANTHROPIC_API_KEY env is set to a sentinel so the SDK constructs
#     without erroring; the router sidecar substitutes the real
#     credential on egress.

set -eu

# Router sidecar — the only LLM endpoint allowed by NetworkPolicy +
# egress-guard. Anthropic Python SDK reads ANTHROPIC_BASE_URL.
export ANTHROPIC_BASE_URL="${ANTHROPIC_BASE_URL:-http://127.0.0.1:8443/anthropic/v1}"

# Sentinel API key — router strips on egress. NEVER a real key.
# The bootstrap() call below also defaults this, but exporting here
# makes the intent visible for any pre-import code (e.g. user wrappers
# that read env at import time).
export ANTHROPIC_API_KEY="${ANTHROPIC_API_KEY:-router-managed}"

# Platform MCP server: 9 Foundry-shim tools every runtime gets for
# free. Claude Agent SDK MCP client points here via mcp_servers=[...].
export AZURECLAW_PLATFORM_MCP_URL="${AZURECLAW_PLATFORM_MCP_URL:-http://127.0.0.1:8443/platform/mcp}"

# AGT relay/registry — reverse-proxied by the router so AgentMesh
# traffic shares the same governance gate as LLM traffic.
export AZURECLAW_AGT_RELAY_URL="${AZURECLAW_AGT_RELAY_URL:-http://127.0.0.1:8443/agt/relay/}"
export AZURECLAW_AGT_REGISTRY_URL="${AZURECLAW_AGT_REGISTRY_URL:-http://127.0.0.1:8443/agt/registry/}"

# OTel collector — the router exposes `/v1/traces` and `/v1/metrics`.
export OTEL_EXPORTER_OTLP_ENDPOINT="${OTEL_EXPORTER_OTLP_ENDPOINT:-http://127.0.0.1:8443/v1/traces}"

# Surface the controller-supplied python version to user code (for
# diagnostics / version-pinning logs).
if [ -n "${RUNTIME_PYTHON_VERSION:-}" ]; then
    echo "[azureclaw-anthropic] python: ${RUNTIME_PYTHON_VERSION}" >&2
fi

# In-pod adapter bootstrap. Idempotent (guarded by
# `__AZURECLAW_RUNTIME_INITIALIZED__`). Non-fatal if telemetry init
# fails — the adapter logs and continues.
python -c "from azureclaw_runtime_anthropic.runtime import bootstrap; bootstrap()"

exec "$@"
