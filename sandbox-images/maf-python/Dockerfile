# syntax=docker/dockerfile:1.7
#
# AzureClaw runtime image: Microsoft Agent Framework Python (S10.A4, scaffolding).
#
# Status: SCAFFOLDING ONLY. The image is wired through
# `runtime.rs::plan_microsoft_agent_framework` and `RuntimeKind::MicrosoftAgentFramework`
# is dispatchable in the controller, but the in-pod adapter (AAD shim,
# `azure_openai_endpoint` rewrite, AGT-init compat, MAF-specific MCP client glue)
# is **not yet implemented**. S10.A4 lands the controller dispatch + skeleton
# image; the adapter package + reference example land as the immediate follow-up.
#
# MAF is the strategically prioritized Microsoft-aligned runtime. Per
# `plan.md` §S10-runtime-agnostic-rule the MAF .NET path is **deferred to
# Phase 3** pending AgentMesh.Sdk .NET upstream availability.
#
# Contract this image must honor (when complete):
#   - Process runs as UID 1000 (egress-guard pin in `reconciler/mod.rs`).
#   - LLM traffic goes via `http://127.0.0.1:8443` (router sidecar — governance gate).
#   - Reads MCP gateway from `AZURECLAW_PLATFORM_MCP_URL` (S10.B platform MCP server).
#   - Honors `AZURECLAW_*` env wiring (router URL, AGT relay URL, identity SAN).
#   - Does not bind privileged ports; default `securityContext` only.
#
LABEL org.azureclaw.runtime.contract="v1"
LABEL org.azureclaw.runtime.kind="MicrosoftAgentFramework"
LABEL org.azureclaw.runtime.language="python"

FROM mcr.microsoft.com/cbl-mariner/base/python:3.12 AS base

# `agent-framework` is Microsoft's unified agent SDK
# (https://github.com/microsoft/agent-framework). Successor to
# AutoGen v0.4. AGT integration is first-party.
#
# `azure-identity` is the AAD/MSI auth path the adapter wires up at
# entrypoint time (the in-pod adapter package will add the
# `DefaultAzureCredential` shim + token refresh).
RUN pip install --no-cache-dir \
    'agent-framework>=0.1,<0.2' \
    'openai>=1.50,<2' \
    'azure-identity>=1.18,<2' \
    'httpx>=0.27,<0.28'

COPY entrypoint.sh /usr/local/bin/azureclaw-maf-entrypoint.sh
RUN chmod 0755 /usr/local/bin/azureclaw-maf-entrypoint.sh

# Workspace where the controller mounts user-supplied agent code via
# `agentCode: { oci | git }`.
RUN mkdir -p /sandbox/agent && chown -R 1000:1000 /sandbox

USER 1000
WORKDIR /sandbox/agent

ENTRYPOINT ["/usr/local/bin/azureclaw-maf-entrypoint.sh"]
CMD ["python", "/sandbox/agent/main.py"]
