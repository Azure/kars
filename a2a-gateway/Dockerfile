# kars A2A Gateway — distroless (build-once pattern)
#
# Public A2A 1.0.0 ingress edge. Built from a pre-compiled binary
# produced by the `build-rust` CI job; the binary is COPY'd in.
# (See ADR-0001 #4 for the front-edge design.)
#
# Migration history:
#   - Was: rust:bookworm builder → gcr.io/distroless/static-debian12
#   - Now: pre-built binary + AL3 distroless (single MS supply chain
#     across all kars Rust images)
#
# Migrating to AL4 (preview) or AL5 (future GA): flip
# AZURELINUX_DISTROLESS below.

ARG AZURELINUX_DISTROLESS=mcr.microsoft.com/azurelinux/distroless/base:3.0
ARG VERSION=0.1.0
ARG BIN_PATH_PREFIX=bin

FROM ${AZURELINUX_DISTROLESS}

ARG VERSION
ARG BIN_PATH_PREFIX
# TARGETARCH is auto-set by buildx ("amd64" or "arm64") so we pick
# the right pre-built binary from ./bin/<arch>/.
ARG TARGETARCH
COPY ${BIN_PATH_PREFIX}/${TARGETARCH}/kars-a2a-gateway /usr/local/bin/kars-a2a-gateway

USER 1000:1000
EXPOSE 8443 9090

LABEL org.opencontainers.image.title="kars A2A Gateway" \
      org.opencontainers.image.description="Public-ingress A2A 1.0.0 peer edge for kars" \
      org.opencontainers.image.vendor="Microsoft" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.source="https://github.com/Azure/kars"

ENTRYPOINT ["/usr/local/bin/kars-a2a-gateway"]
