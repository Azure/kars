# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

ARG AZURELINUX_DISTROLESS=mcr.microsoft.com/azurelinux/distroless/base:3.0
FROM golang:1.27.2-bookworm AS build
ENV GOTOOLCHAIN=local CGO_ENABLED=0 GOMAXPROCS=2 GOMEMLIMIT=3GiB
WORKDIR /build
ADD --checksum=sha256:e1b27ba92ebe34a83a644d57bd14a8b0748c3826a8691485afa9c62c154f00dc https://codeload.github.com/prometheus-community/json_exporter/tar.gz/06fb506a4c5d242186f198b9c8bf072212f3a134 /build/source.tar.gz
COPY sandbox-images/json-exporter/json-exporter.go.mod sandbox-images/json-exporter/json-exporter.go.sum /build/
COPY sandbox-images/json-exporter/build.sh /usr/local/bin/build-json-exporter
RUN sh /usr/local/bin/build-json-exporter /build

FROM ${AZURELINUX_DISTROLESS} AS runtime
COPY --from=build --chmod=0555 /build/json_exporter /bin/json_exporter
COPY --from=build /build/licenses/ /usr/share/licenses/json-exporter/
LABEL org.opencontainers.image.version="0.7.0" \
      org.opencontainers.image.revision="06fb506a4c5d242186f198b9c8bf072212f3a134" \
      org.opencontainers.image.source="https://github.com/Azure/kars" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.kars.json-exporter.security-backport="go1.27.2-net0.60-crypto0.57-oauth0.27"
USER 65534:65534
EXPOSE 7979
ENTRYPOINT ["/bin/json_exporter"]
