# syntax=docker/dockerfile:1.7
#
# AzureClaw runtime image: Pydantic-AI for Python.
#
# Ships the in-pod adapter package `azureclaw_runtime_pydantic_ai`,
# which is invoked from `entrypoint.sh::bootstrap` before the user
# agent code runs. The adapter:
#   - pins each LLM provider base URL (OPENAI_BASE_URL,
#     AZURE_OPENAI_ENDPOINT, ANTHROPIC_BASE_URL) to the router sidecar;
#   - sets each provider API-key env to a sentinel — router
#     substitutes the real cred on egress;
#   - initializes OpenTelemetry against the router's OTLP collector;
#   - brokers AAD tokens via Workload Identity (5-min skew cache);
#   - bridges AgentMesh A2A messages over `/agt/relay`.
#
# Foundry tools (9 platform MCP tools) are reachable at
# `http://127.0.0.1:8443/platform/mcp` via any MCP client — Pydantic-AI
# nodes can invoke them directly. No SDK-specific tool wrapper module
# is shipped.
#
# Contract this image must honor:
#   - Process runs as UID 1000 (egress-guard pin in `reconciler/mod.rs`).
#   - LLM traffic goes via `http://127.0.0.1:8443` (router sidecar).
#   - Reads MCP gateway from `AZURECLAW_PLATFORM_MCP_URL`.
#   - Honors `AZURECLAW_*` env wiring (router URL, AGT relay URL, identity SAN).
#   - Does not bind privileged ports; default `securityContext` only.
#
# AGT-Python wheels (`a2a_agentmesh`, `agent_sandbox`) are built from
# the upstream `agent-governance-toolkit` repo by
# `runtimes/build-agt-wheels.sh` and copied into the build context as
# `runtimes/wheels/*.whl`. This keeps the image hermetic without
# publishing private wheels to PyPI.

FROM mcr.microsoft.com/azurelinux/base/python:3.12 AS base

LABEL org.azureclaw.runtime.contract="v1"
LABEL org.azureclaw.runtime.kind="PydanticAi"
LABEL org.azureclaw.runtime.language="python"

# ---- Install AGT-Python wheels and the AzureClaw adapter ----------------
COPY runtimes/wheels/ /tmp/agt-wheels/
RUN if ls /tmp/agt-wheels/*.whl >/dev/null 2>&1; then \
        pip install --no-cache-dir /tmp/agt-wheels/*.whl ; \
    fi

COPY runtimes/pydantic-ai/ /opt/azureclaw-runtime-pydantic-ai/
RUN pip install --no-cache-dir /opt/azureclaw-runtime-pydantic-ai

# ---- Adapter entrypoint -------------------------------------------------
COPY sandbox-images/pydantic-ai/entrypoint.sh /usr/local/bin/azureclaw-pydantic-ai-entrypoint.sh
RUN chmod 0755 /usr/local/bin/azureclaw-pydantic-ai-entrypoint.sh

# Workspace where the controller mounts user-supplied agent code via
# `agentCode: { oci | git }` (see `PydanticAiConfig` in crd.rs).
RUN mkdir -p /sandbox/agent && chown -R 1000:1000 /sandbox

COPY --chown=1000:1000 sandbox-images/pydantic-ai/default-agent/ /opt/azureclaw-default-agent/

USER 1000
WORKDIR /sandbox/agent

ENTRYPOINT ["/usr/local/bin/azureclaw-pydantic-ai-entrypoint.sh"]
CMD ["python3", "/sandbox/agent/main.py"]
