# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

ARG AZURELINUX_DISTROLESS=mcr.microsoft.com/azurelinux/distroless/base:3.0
FROM registry.k8s.io/metrics-server/metrics-server@sha256:d9862115e7c7881280d3d75ca26bda8ffc0fc213315979575bf23ce9826205c0 AS upstream
FROM ${AZURELINUX_DISTROLESS}

COPY --from=upstream /metrics-server /usr/local/bin/metrics-server
ADD --chmod=0444 --checksum=sha256:b40930bbcf80744c86c46a12bc9da056641d722716c378f5659b9e555ef833e1 https://raw.githubusercontent.com/kubernetes-sigs/metrics-server/v0.9.0/LICENSE /usr/share/licenses/metrics-server/LICENSE
LABEL org.opencontainers.image.title="Kars metrics-server" \
      org.opencontainers.image.description="Upstream metrics-server v0.9.0 on Azure Linux 3 distroless" \
      org.opencontainers.image.source="https://github.com/kubernetes-sigs/metrics-server" \
      org.opencontainers.image.licenses="Apache-2.0"
USER 10001:10001
EXPOSE 10250
ENTRYPOINT ["/usr/local/bin/metrics-server"]
