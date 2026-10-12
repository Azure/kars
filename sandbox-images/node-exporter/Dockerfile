# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

ARG AZURELINUX_DISTROLESS=mcr.microsoft.com/azurelinux/distroless/base:3.0
FROM golang:1.27.2-bookworm AS build
ENV GOTOOLCHAIN=local CGO_ENABLED=0 GOMAXPROCS=2 GOMEMLIMIT=3GiB
WORKDIR /build
ADD --checksum=sha256:8da6b3d3f4fc8411dabe65cb9a619cf0a17c5088dbc825bd127d314994cc41f9 https://codeload.github.com/prometheus/node_exporter/tar.gz/0dd664dece3f8319f6bec5a221acd2c7ad13a23d /build/source.tar.gz
COPY sandbox-images/node-exporter/node-exporter.go.mod sandbox-images/node-exporter/node-exporter.go.sum /build/
COPY sandbox-images/node-exporter/build.sh /usr/local/bin/build-node-exporter
RUN sh /usr/local/bin/build-node-exporter /build

FROM ${AZURELINUX_DISTROLESS} AS runtime
COPY --from=build --chmod=0555 /build/node_exporter /bin/node_exporter
COPY --from=build /build/licenses/ /usr/share/licenses/node-exporter/
LABEL org.opencontainers.image.version="1.11.1" \
      org.opencontainers.image.revision="0dd664dece3f8319f6bec5a221acd2c7ad13a23d" \
      org.opencontainers.image.source="https://github.com/Azure/kars" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.kars.node-exporter.security-backport="go1.27.2-net0.60-crypto0.57"
USER 65534:65534
EXPOSE 9100
ENTRYPOINT ["/bin/node_exporter"]
