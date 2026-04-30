# syntax=docker/dockerfile:1.7
#
# AzureClaw runtime image: OpenAI Agents Python (S10.A3, scaffolding).
#
# Status: SCAFFOLDING ONLY. The image is wired through `runtime.rs::plan_openai_agents`
# and `RuntimeKind::OpenAIAgents` is dispatchable in the controller, but the in-pod
# adapter (AAD shim, `OPENAI_BASE_URL` rewrite, AGT-init compat) is **not yet
# implemented**. S10.A3 lands the controller dispatch + skeleton image; the adapter
# package + reference example land as a follow-up before the slice closes.
#
# Contract this image must honor (when complete):
#   - Process runs as UID 1000 (egress-guard pin in `reconciler/mod.rs:873-915`).
#   - LLM traffic goes via `http://127.0.0.1:8443` (router sidecar, governance gate).
#   - Reads MCP gateway from `AZURECLAW_PLATFORM_MCP_URL` (S10.B platform MCP server).
#   - Honors `AZURECLAW_*` env wiring (router URL, AGT relay URL, identity SAN).
#   - Does not bind privileged ports; default `securityContext` only.
#
# Image label so the controller's BYO-style contract verifier can recognise this
# adapter image even when running under `runtime.kind: BYO` for testing.
LABEL org.azureclaw.runtime.contract="v1"
LABEL org.azureclaw.runtime.kind="OpenAIAgents"

FROM mcr.microsoft.com/cbl-mariner/base/python:3.12 AS base

# `openai-agents` is the official OpenAI Python SDK for Agents
# (https://github.com/openai/openai-agents-python). Pinned to a
# minor-version range that can be bumped by the runtime-deps prestage
# pass without recompiling the entire image.
RUN pip install --no-cache-dir \
    'openai-agents>=0.1,<0.2' \
    'openai>=1.50,<2' \
    'httpx>=0.27,<0.28'

# Adapter package shim. The real adapter ships as # ci:stub-ok: Phase 2 stake — real adapter follows in S10.A3 image build
# `azureclaw-runtime-openai-agents` on PyPI in a follow-up commit;
# until then, `entrypoint.sh` exports the env vars by hand and execs
# the user agent code.
COPY entrypoint.sh /usr/local/bin/azureclaw-openai-agents-entrypoint.sh
RUN chmod 0755 /usr/local/bin/azureclaw-openai-agents-entrypoint.sh

# Workspace where the controller mounts user-supplied agent code via
# `agentCode: { oci | git }` (see `OpenAIAgentsConfig` in crd.rs).
RUN mkdir -p /sandbox/agent && chown -R 1000:1000 /sandbox

USER 1000
WORKDIR /sandbox/agent

ENTRYPOINT ["/usr/local/bin/azureclaw-openai-agents-entrypoint.sh"]
CMD ["python", "/sandbox/agent/main.py"]
