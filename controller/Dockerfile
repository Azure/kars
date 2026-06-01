# kars Controller — distroless (build-once pattern)
#
# Built from a pre-compiled binary produced by the `build-rust` CI job
# (cargo build --release on the host glibc target). This image
# does NOT run cargo — the binary is COPY'd in. Result: small image,
# zero build cycle inside Docker, identical artefact to host tests.
#
# Migrating to AL4 (preview) or AL5 (future GA): flip AZURELINUX_DISTROLESS
# below. The binary itself doesn't care about the base image version,
# only that the runtime glibc is ≥ build-host glibc.

ARG AZURELINUX_DISTROLESS=mcr.microsoft.com/azurelinux/distroless/base:3.0
ARG VERSION=0.1.0
ARG BIN_PATH_PREFIX=bin

FROM ${AZURELINUX_DISTROLESS}

ARG VERSION
ARG BIN_PATH_PREFIX
# TARGETARCH is auto-set by buildx ("amd64" or "arm64") so we pick
# the right pre-built binary from ./bin/<arch>/.
ARG TARGETARCH
COPY ${BIN_PATH_PREFIX}/${TARGETARCH}/kars-controller /usr/local/bin/kars-controller

USER 1000:1000

LABEL org.opencontainers.image.title="kars Controller" \
      org.opencontainers.image.description="Kubernetes operator for kars sandbox lifecycle" \
      org.opencontainers.image.vendor="Microsoft" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.source="https://github.com/Azure/kars"

ENTRYPOINT ["/usr/local/bin/kars-controller"]
