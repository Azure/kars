# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

ARG AZURELINUX_DISTROLESS=mcr.microsoft.com/azurelinux/distroless/base:3.0
FROM golang:1.27.2-bookworm AS build
ENV GOTOOLCHAIN=local CGO_ENABLED=0 GOMAXPROCS=2 GOMEMLIMIT=3GiB
WORKDIR /build
ADD --checksum=sha256:042b98dc0160b7303e4287a3b23abce6860ba1c43d7c79d9e86233e934dbc5de https://codeload.github.com/jkroepke/kube-webhook-certgen/tar.gz/a20e3561ce81addf2749d155fdcf4dc015f07699 /build/source.tar.gz
COPY sandbox-images/webhook-certgen/certgen.go.mod sandbox-images/webhook-certgen/certgen.go.sum /build/
COPY sandbox-images/webhook-certgen/build.sh /usr/local/bin/build-certgen
RUN sh /usr/local/bin/build-certgen /build

FROM ${AZURELINUX_DISTROLESS} AS runtime
COPY --from=build --chmod=0555 /build/kube-webhook-certgen /kube-webhook-certgen
COPY --from=build /build/licenses/ /usr/share/licenses/webhook-certgen/
LABEL org.opencontainers.image.version="1.8.3" \
      org.opencontainers.image.revision="a20e3561ce81addf2749d155fdcf4dc015f07699" \
      org.opencontainers.image.source="https://github.com/Azure/kars" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.kars.webhook-certgen.security-backport="go1.27.2-net0.60"
USER 65534:65534
ENTRYPOINT ["/kube-webhook-certgen"]
