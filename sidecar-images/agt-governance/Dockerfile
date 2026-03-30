# AGT Governance Sidecar — wraps the official Agent Governance Toolkit
# (microsoft/agent-governance-toolkit v3.0.0).
#
# For AKS production, you can also use the upstream image directly:
#   agentmesh/governance-sidecar:0.3.0
# This Dockerfile exists for dev mode and ACR builds.
#
# When the AGT Rust SDK ships, this sidecar is removed entirely.

ARG AZURELINUX_BASE=mcr.microsoft.com/azurelinux/base/core:3.0@sha256:a452d39c91576f5a2c983c7d3b62521fabd08e16b4a7237e24bf2be3b06e1651

FROM ${AZURELINUX_BASE}

RUN tdnf install -y python3 python3-pip ca-certificates python3-idna PyYAML && tdnf clean all

WORKDIR /app

# Install AGT from vendored wheels (GitHub release v3.0.0, SHA256-verified)
# Includes agentmesh-platform, agent-os-kernel, agent-hypervisor, and their
# runtime dependencies (email-validator, jsonschema). All wheels are pure-Python.
COPY vendor/agt-wheels/ /tmp/agt-wheels/
RUN pip3 install --no-cache-dir --no-deps /tmp/agt-wheels/*.whl && \
    pip3 install --no-cache-dir pydantic dnspython 2>/dev/null \
      || echo "WARN: PyPI unreachable — pydantic/dnspython skipped" && \
    rm -rf /tmp/agt-wheels

COPY sidecar-images/agt-governance/server.py ./

RUN groupadd -g 1002 agt && useradd -u 1002 -g agt -m agt
USER 1002

EXPOSE 8081 9091
CMD ["python3", "server.py"]
