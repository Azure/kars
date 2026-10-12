# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

ARG AZURELINUX_DISTROLESS=mcr.microsoft.com/azurelinux/distroless/base:3.0
FROM golang:1.27.2-bookworm AS build
ENV GOTOOLCHAIN=local CGO_ENABLED=0 GOMAXPROCS=2 GOMEMLIMIT=3GiB
WORKDIR /build
ADD --checksum=sha256:6ec05363ea7aa681966aa47c38fd81b4b5689dcfb455cba2de0ad1e2f967dcfc https://codeload.github.com/prometheus-operator/prometheus-operator/tar.gz/32d1b3dfa05d070762450efe9624bb2483c782be /build/source.tar.gz
COPY sandbox-images/prometheus-operator/operator.go.mod sandbox-images/prometheus-operator/operator.go.sum /build/
COPY sandbox-images/prometheus-operator/build.sh /usr/local/bin/build-operator
RUN sh /usr/local/bin/build-operator /build

FROM ${AZURELINUX_DISTROLESS} AS runtime
COPY --from=build /build/licenses/ /usr/share/licenses/prometheus-operator/
LABEL org.opencontainers.image.version="0.90.1" \
      org.opencontainers.image.revision="32d1b3dfa05d070762450efe9624bb2483c782be" \
      org.opencontainers.image.source="https://github.com/Azure/kars" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.kars.prometheus-operator.security-backport="go1.27.2-net0.60-grpc1.83.2-prometheus0.311.3"
ENV HOME=/tmp
USER 65534:65534

FROM runtime AS operator
COPY --from=build --chmod=0555 /build/operator /bin/operator
ENTRYPOINT ["/bin/operator"]

FROM runtime AS config-reloader
COPY --from=build --chmod=0555 /build/prometheus-config-reloader /bin/prometheus-config-reloader
ENTRYPOINT ["/bin/prometheus-config-reloader"]
