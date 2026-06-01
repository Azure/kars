# kars Inference Router — distroless (build-once pattern)
#
# Built from a pre-compiled binary produced by the `build-rust` CI job
# (cargo build --release on the host glibc target). This image
# does NOT run cargo — the binary is COPY'd in. Result: small image,
# zero build cycle inside Docker, identical artefact to host tests.
#
# Hardening:
#   - Microsoft Azure Linux 3 distroless: no shell, no package manager,
#     curated MS supply chain.
#   - Glibc-linked binary on AL3 distroless: the base image ships
#     glibc 2.38 + libcrypto.so.3 + ld-linux. We compile against the
#     CI runner's glibc 2.35 (ubuntu-22.04) — older glibc, so the
#     binary runs on AL3's newer glibc (backward compat).
#   - Runs as non-root.
#
# Migrating to AL4 (preview) or AL5 (future GA): flip AZURELINUX_DISTROLESS
# below. The binary itself doesn't care about the base image version,
# only that the runtime glibc is ≥ build-host glibc.

ARG AZURELINUX_DISTROLESS=mcr.microsoft.com/azurelinux/distroless/base:3.0
ARG VERSION=0.1.0
ARG BIN_PATH=bin/kars-inference-router

FROM ${AZURELINUX_DISTROLESS}

ARG VERSION
ARG BIN_PATH
COPY ${BIN_PATH} /usr/local/bin/kars-inference-router

USER 1000:1000
EXPOSE 8443

LABEL org.opencontainers.image.title="kars Inference Router" \
      org.opencontainers.image.description="High-performance inference proxy for kars" \
      org.opencontainers.image.vendor="Microsoft" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.source="https://github.com/Azure/kars"

ENTRYPOINT ["/usr/local/bin/kars-inference-router"]
