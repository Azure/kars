# syntax=docker/dockerfile:1.7
#
# Kars runtime image: OpenAI Agents Python.
#
# Ships the in-pod adapter package `kars_runtime_openai_agents`,
# which is invoked from `entrypoint.sh::bootstrap` before the user
# agent code runs. The adapter:
#   - pins `OPENAI_BASE_URL` to the router sidecar (governance gate);
#   - initializes OpenTelemetry against the router's OTLP collector;
#   - brokers AAD tokens via Workload Identity (5-min skew cache);
#   - bridges AgentMesh A2A messages over `/agt/relay`;
#   - registers the 9 Foundry MCP tools on the user's Agent.
#
# Contract this image must honor:
#   - Process runs as UID 1000 (egress-guard pin in `reconciler/mod.rs`).
#   - LLM traffic goes via `http://127.0.0.1:8443` (router sidecar — governance gate).
#   - Reads MCP gateway from `KARS_PLATFORM_MCP_URL` (S10.B platform MCP server).
#   - Honors `KARS_*` env wiring (router URL, AGT relay URL, identity SAN).
#   - Does not bind privileged ports; default `securityContext` only.
#
# AGT-Python wheels (`a2a_agentmesh`, `agent_sandbox`) are built from
# the upstream `agent-governance-toolkit` repo by
# `runtimes/build-agt-wheels.sh` and copied into the build context as
# `runtimes/wheels/*.whl`. This keeps the image hermetic without
# publishing private wheels to PyPI.

FROM mcr.microsoft.com/azurelinux/base/python:3.12 AS base

LABEL org.kars.runtime.contract="v1"
LABEL org.kars.runtime.kind="OpenAIAgents"
LABEL org.kars.runtime.language="python"

# ---- Install AGT-Python wheels and the Kars adapter ----------------
# The wheels directory must exist (even if empty) — the build context
# always includes `runtimes/wheels/.gitkeep`. Operators run
# `runtimes/build-agt-wheels.sh` to populate it before `docker build`.
COPY runtimes/wheels/ /tmp/agt-wheels/
RUN if ls /tmp/agt-wheels/*.whl >/dev/null 2>&1; then \
        pip install --no-cache-dir /tmp/agt-wheels/*.whl ; \
    fi

COPY runtimes/openai-agents/ /opt/kars-runtime-openai-agents/
RUN pip install --no-cache-dir /opt/kars-runtime-openai-agents

# ---- Adapter entrypoint -------------------------------------------------
COPY sandbox-images/openai-agents/entrypoint.sh /usr/local/bin/kars-openai-agents-entrypoint.sh
RUN chmod 0755 /usr/local/bin/kars-openai-agents-entrypoint.sh

# Workspace where the controller mounts user-supplied agent code via
# `agentCode: { oci | git }` (see `OpenAIAgentsConfig` in crd.rs).
RUN mkdir -p /sandbox/agent && chown -R 1000:1000 /sandbox

# Default fallback agent — runs when the operator spawns this runtime
# without supplying `agentCode`. Proves framework load + router
# inference path; replace by mounting your own /sandbox/agent/main.py.
COPY --chown=1000:1000 sandbox-images/openai-agents/default-agent/ /opt/kars-default-agent/

USER 1000
WORKDIR /sandbox/agent

ENTRYPOINT ["/usr/local/bin/kars-openai-agents-entrypoint.sh"]
CMD ["python3", "/sandbox/agent/main.py"]
