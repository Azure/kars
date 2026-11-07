# syntax=docker/dockerfile:1.7
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

FROM mcr.microsoft.com/azurelinux/base/python:3.12@sha256:71a1f5a1821f4c56e45aff52e792ed6c0500631e201cfc140c4c9d22886b29f9 AS build
RUN tdnf update -y --refresh && tdnf clean all
COPY runtimes/wheels/ /tmp/agt-wheels/
COPY runtimes/install-agt-wheels.py /tmp/install-agt-wheels.py
COPY sandbox-images/hermes/hermes-core-constraints.txt /tmp/constraints.txt
COPY sandbox-images/agentmesh/install.py /tmp/install-agentmesh.py
RUN python3 /tmp/install-agentmesh.py
ARG COMPONENT=registry
RUN python3 -c 'import sys; assert sys.argv[1] in ("registry", "relay"), "Unsupported AgentMesh component"' "${COMPONENT}"

FROM mcr.microsoft.com/azurelinux/distroless/python:3.12@sha256:10bcfa3c563fd05f9b11495551bd36bfe919b4a2f32b68388e1c393bbeea64ac
ARG COMPONENT=registry
COPY --from=build /opt/agentmesh /opt/agentmesh
COPY sandbox-images/agentmesh/entrypoint.py /opt/kars/launch-agentmesh.py
COPY vendor/agt/pin.json /usr/share/kars/agentmesh/source.json
COPY LICENSE /usr/share/licenses/kars/LICENSE
LABEL org.opencontainers.image.title="Kars AgentMesh" \
      org.opencontainers.image.source="https://github.com/Azure/kars" \
      org.opencontainers.image.licenses="MIT" \
      org.kars.agentmesh.version="5.0.0"
ENV AGENTMESH_COMPONENT=${COMPONENT} \
    PYTHONPATH=/opt/agentmesh \
    PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    HOST=0.0.0.0 \
    LOG_LEVEL=info \
    HOME=/tmp
USER 1000:1000
WORKDIR /opt/kars
EXPOSE 8082 8083
ENTRYPOINT ["/usr/bin/python3", "/opt/kars/launch-agentmesh.py"]
