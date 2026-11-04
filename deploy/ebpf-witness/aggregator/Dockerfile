# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
# No aggregator image has been published merely because this Dockerfile exists.
ARG PYTHON_BUILD_BASE=mcr.microsoft.com/azurelinux/base/python:3.12@sha256:71a1f5a1821f4c56e45aff52e792ed6c0500631e201cfc140c4c9d22886b29f9
ARG PYTHON_BASE=mcr.microsoft.com/azurelinux/distroless/python:3.12@sha256:10bcfa3c563fd05f9b11495551bd36bfe919b4a2f32b68388e1c393bbeea64ac
FROM golang:1.27.2-bookworm AS toolchain

FROM ${PYTHON_BUILD_BASE} AS client
ARG PYTHON_BUILD_BASE
ARG PYTHON_BASE
ARG SOURCE_REVISION
ARG TARGETARCH
ENV TARGETARCH=${TARGETARCH}
ENV PATH=/usr/local/go/bin:${PATH}
COPY --from=toolchain /usr/local/go /usr/local/go
WORKDIR /build
ADD --checksum=sha256:934060feae4ecab32644704c4e272e06b7c0da5a4f67429ba04b0113751946e4 https://codeload.github.com/inspektor-gadget/inspektor-gadget/tar.gz/7a314380ff61534ea70df9fb086505f958b2dff9 /build/source.tar.gz
COPY deploy/ebpf-witness/aggregator/build-runtime.py deploy/ebpf-witness/aggregator/client.go.mod deploy/ebpf-witness/aggregator/client.go.sum /build/
COPY deploy/ebpf-witness/aggregator/witness.py deploy/helm/kars-datapath-witness/THIRD_PARTY_NOTICES.md /build/
COPY deploy/ebpf-witness/aggregator/health /build/health
COPY deploy/ebpf-witness/aggregator/lifecycle /build/lifecycle
RUN python3 /build/build-runtime.py /build

FROM ${PYTHON_BASE}
ARG SOURCE_REVISION
LABEL org.opencontainers.image.source="https://github.com/Azure/kars" \
      org.opencontainers.image.revision="${SOURCE_REVISION}" \
      org.opencontainers.image.licenses="MIT AND Apache-2.0" \
      io.kars.witness.ig-version="v0.53.2"
COPY --from=client /build/runtime-root/ /
ENV PYTHONDONTWRITEBYTECODE=1 PYTHONUNBUFFERED=1 HOME=/tmp
USER 10001:10001
ENTRYPOINT ["/usr/bin/python3", "/opt/witness/witness.py"]
