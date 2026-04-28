#!/bin/sh
# AzureClaw MAF Python runtime entrypoint (S10.A4 scaffolding).
#
# This script is the in-pod adapter's bootstrap. It sets the env the
# `agent-framework` Python SDK needs to route LLM traffic through the
# AzureClaw router sidecar (so InferencePolicy + Content Safety + token
# budgets apply), points MCP-aware tools at the platform MCP server
# (S10.B), then execs the user agent code.
#
# Hard rules:
#   - Process must already be running as UID 1000 (Dockerfile USER 1000).
#   - No direct LLM endpoint variables: only `127.0.0.1:8443` is allowed.
#     The egress-guard iptables init container blocks every other path.
#
# Status: scaffolding. AAD token shim (DefaultAzureCredential ->
# bearer-on-router) + Azure OpenAI rewrite + AGT-init compat land with
# the real `azureclaw-runtime-maf-python` adapter package.

set -eu

# Router sidecar — only LLM endpoint allowed by NetworkPolicy +
# egress-guard. `agent-framework` consumes `openai`/`azure-openai`
# clients which read these env vars by convention.
export OPENAI_BASE_URL="${OPENAI_BASE_URL:-http://127.0.0.1:8443/openai/v1}"
export AZURE_OPENAI_ENDPOINT="${AZURE_OPENAI_ENDPOINT:-http://127.0.0.1:8443/openai}"

# Platform MCP server (S10.B): Foundry-shim tools every runtime gets
# for free. MAF MCP client points here.
export AZURECLAW_PLATFORM_MCP_URL="${AZURECLAW_PLATFORM_MCP_URL:-http://127.0.0.1:8443/platform/mcp}"

# Surface the controller-supplied language label (for diagnostics).
if [ -n "${RUNTIME_MAF_LANGUAGE:-}" ]; then
    echo "[azureclaw-maf] language: ${RUNTIME_MAF_LANGUAGE}" >&2
fi

# AGT relay — Class B (mesh / spawn / handoff) is per-runtime and
# blocked on AgentMesh-Python availability (see
# `docs/internal/agt-upstream-asks.md` §3). Until then, S10.A4 ships
# Foundry-shim access only via S10.B; mesh tools are placeholders.

exec "$@"
