# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

ARG AZURELINUX_DISTROLESS=mcr.microsoft.com/azurelinux/distroless/base:3.0
FROM golang:1.27.2-bookworm AS build
ENV GOTOOLCHAIN=local CGO_ENABLED=0 GOMAXPROCS=2 GOMEMLIMIT=3GiB
WORKDIR /build
ADD --checksum=sha256:7e1370b14f9dc79185475d9e271413b2c7b33e85238e129be33b231b128bacb5 https://codeload.github.com/kubernetes/kube-state-metrics/tar.gz/2ef24549bff9865db214ccb03cc36f1fc214d1be /build/source.tar.gz
COPY sandbox-images/kube-state-metrics/metrics.go.mod sandbox-images/kube-state-metrics/metrics.go.sum /build/
COPY sandbox-images/kube-state-metrics/build.sh /usr/local/bin/build-kube-state-metrics
RUN sh /usr/local/bin/build-kube-state-metrics /build

FROM ${AZURELINUX_DISTROLESS} AS runtime
COPY --from=build --chmod=0555 /build/kube-state-metrics /kube-state-metrics
COPY --from=build /build/licenses/ /usr/share/licenses/kube-state-metrics/
LABEL org.opencontainers.image.version="2.19.0" \
      org.opencontainers.image.revision="2ef24549bff9865db214ccb03cc36f1fc214d1be" \
      org.opencontainers.image.source="https://github.com/Azure/kars" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.kars.kube-state-metrics.security-backport="go1.27.2-net0.60-crypto0.57-grpc1.83.2"
USER 65534:65534
EXPOSE 8080 8081
ENTRYPOINT ["/kube-state-metrics"]
CMD ["--port=8080", "--telemetry-port=8081"]
