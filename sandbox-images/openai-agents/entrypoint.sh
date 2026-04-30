#!/bin/sh
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# AzureClaw OpenAI Agents Python runtime entrypoint (S10.A3 scaffolding).
#
# This script is the in-pod adapter's bootstrap. It sets the environment
# the OpenAI Agents SDK needs to route LLM traffic through the AzureClaw
# router sidecar (so InferencePolicy + Content Safety + token budgets
# apply), points MCP-aware tools at the platform MCP server (S10.B),
# then execs the user agent code.
#
# Hard rules:
#   - Process must already be running as UID 1000 (Dockerfile USER 1000).
#   - No direct LLM endpoint variables: only `127.0.0.1:8443` is allowed.
#     The egress-guard iptables init container blocks every other path.
#
# Status: scaffolding. AAD token shim + Azure OpenAI rewrite (when
# `InferencePolicy` targets AOAI) are PENDING — they land with the real
# `azureclaw-runtime-openai-agents` adapter package.

set -eu

# Router sidecar — the only LLM endpoint allowed by NetworkPolicy +
# egress-guard. Vanilla `openai` SDK reads OPENAI_BASE_URL.
export OPENAI_BASE_URL="${OPENAI_BASE_URL:-http://127.0.0.1:8443/openai/v1}"

# Platform MCP server (S10.B): Foundry-shim tools every runtime gets
# for free. The OpenAI Agents SDK MCP client points here.
export AZURECLAW_PLATFORM_MCP_URL="${AZURECLAW_PLATFORM_MCP_URL:-http://127.0.0.1:8443/platform/mcp}"

# Surface the controller-supplied python version to user code (for
# diagnostics / version-pinning logs). This is the producer-side
# default from `plan_openai_agents`; user `extraEnv` may override.
if [ -n "${RUNTIME_PYTHON_VERSION:-}" ]; then
    echo "[azureclaw-openai-agents] python: ${RUNTIME_PYTHON_VERSION}" >&2
fi

# AGT relay — Class B (mesh / spawn / handoff) is per-runtime and
# blocked on AgentMesh-Python availability (see
# `docs/internal/agt-upstream-asks.md` §3). Until then, S10.A3 ships
# Foundry-shim access only via S10.B; mesh tools are placeholders.

exec "$@"
