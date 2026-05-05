#!/bin/sh
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# AzureClaw MAF Python runtime entrypoint.
#
# Pins the LLM endpoints to the router sidecar, points MCP-aware tools
# at the platform MCP server, then invokes the in-pod adapter's
# `bootstrap()` (AAD broker, OTel auto-init, signal handlers) before
# `exec`-ing the user agent code.
#
# Hard rules:
#   - Process must already be running as UID 1000 (Dockerfile USER 1000).
#   - No direct LLM endpoint variables: only `127.0.0.1:8443` is allowed.
#     The egress-guard iptables init container blocks every other path.

set -eu

# Router sidecar — only LLM endpoint allowed by NetworkPolicy +
# egress-guard. `agent-framework` consumes `openai`/`azure-openai`
# clients which read these env vars by convention.
export OPENAI_BASE_URL="${OPENAI_BASE_URL:-http://127.0.0.1:8443/openai/v1}"
export AZURE_OPENAI_ENDPOINT="${AZURE_OPENAI_ENDPOINT:-http://127.0.0.1:8443/openai}"

# Platform MCP server: 9 Foundry-shim tools every runtime gets for
# free. MAF MCP client points here.
export AZURECLAW_PLATFORM_MCP_URL="${AZURECLAW_PLATFORM_MCP_URL:-http://127.0.0.1:8443/platform/mcp}"

# AGT relay/registry — reverse-proxied by the router so AgentMesh
# traffic shares the same governance gate as LLM traffic.
export AZURECLAW_AGT_RELAY_URL="${AZURECLAW_AGT_RELAY_URL:-http://127.0.0.1:8443/agt/relay/}"
export AZURECLAW_AGT_REGISTRY_URL="${AZURECLAW_AGT_REGISTRY_URL:-http://127.0.0.1:8443/agt/registry/}"

# OTel collector — the router exposes `/v1/traces` and `/v1/metrics`.
export OTEL_EXPORTER_OTLP_ENDPOINT="${OTEL_EXPORTER_OTLP_ENDPOINT:-http://127.0.0.1:8443/v1/traces}"

# Surface the controller-supplied language label (for diagnostics).
if [ -n "${RUNTIME_MAF_LANGUAGE:-}" ]; then
    echo "[azureclaw-maf] language: ${RUNTIME_MAF_LANGUAGE}" >&2
fi

# In-pod adapter bootstrap: AAD broker, OTel init, signal handlers,
# `OPENAI_BASE_URL` + `AZURE_OPENAI_ENDPOINT` pin. Idempotent (guarded
# by `__AZURECLAW_RUNTIME_INITIALIZED__`). Non-fatal if telemetry init
# fails — the adapter logs and continues.
python3 -c "from azureclaw_runtime_maf_python.runtime import bootstrap; bootstrap()"

# If no user agent code is mounted at /sandbox/agent/main.py (operator
# spawned this runtime without `agentCode`), fall back to the bundled
# default smoke-test agent so the pod stays Running and proves the
# Foundry inference path through the router.
if [ ! -f /sandbox/agent/main.py ]; then
    echo "[azureclaw-maf-python] no /sandbox/agent/main.py — running bundled default agent" >&2
    exec python3 /opt/azureclaw-default-agent/main.py
fi

exec "$@"
