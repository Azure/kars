# kars Conformance Runner — distroless (build-once pattern)
#
# A `KarsEval` run spawns this as an ephemeral K8s Job; the runner
# replays an EvalCorpus against the live inference router and writes
# a `RunReport` JSON document to `--output` (and stdout). No
# kube-apiserver access; no secrets minted in-container.
#
# Built from a pre-compiled binary produced by the `build-rust` CI job
# (cargo build --release on the host glibc target). This image
# does NOT run cargo — the binary is COPY'd in.
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
COPY ${BIN_PATH_PREFIX}/${TARGETARCH}/kars-conformance-runner /usr/local/bin/kars-conformance-runner

USER 1000:1000

LABEL org.opencontainers.image.title="kars Conformance Runner" \
      org.opencontainers.image.description="Replays an EvalCorpus against the kars inference router and emits a RunReport JSON document" \
      org.opencontainers.image.vendor="Microsoft" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.source="https://github.com/Azure/kars"

ENTRYPOINT ["/usr/local/bin/kars-conformance-runner"]
