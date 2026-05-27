# kars Controller — multi-stage Rust build (Azure Linux)

ARG AZURELINUX_BASE=mcr.microsoft.com/azurelinux/base/core:3.0@sha256:35149ae8dd179684f969944f54a337c665a64e702486154eb44253fb39c2505b
ARG VERSION=0.1.0

# ─── Builder ──────────────────────────────────────────────────────────────────

FROM ${AZURELINUX_BASE} AS builder

RUN tdnf install -y gcc glibc-devel binutils make pkg-config openssl-devel ca-certificates curl kernel-headers && tdnf clean all

# Install Rust via rustup (no distro packages needed)
ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo
ENV PATH="/usr/local/cargo/bin:${PATH}"
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain 1.88.0 --profile minimal

WORKDIR /build
COPY Cargo.toml Cargo.lock* ./
COPY controller/ controller/
COPY inference-router/ inference-router/
COPY a2a-gateway/ a2a-gateway/
COPY kars-a2a-core/ kars-a2a-core/
COPY eval-corpus/ eval-corpus/
COPY conformance-runner/ conformance-runner/
COPY tests/ tests/
COPY cli/blocklists/ cli/blocklists/
COPY cli/profiles/agt/ cli/profiles/agt/

RUN cargo build --release --package kars-controller

# ─── Runtime ──────────────────────────────────────────────────────────────────

FROM ${AZURELINUX_BASE}

RUN tdnf install -y ca-certificates shadow-utils && tdnf clean all

# Non-root user
RUN groupadd -g 1000 controller && useradd -u 1000 -g controller -s /sbin/nologin controller

COPY --from=builder /build/target/release/kars-controller /usr/local/bin/kars-controller

USER controller

LABEL org.opencontainers.image.title="kars Controller" \
      org.opencontainers.image.description="Kubernetes operator for kars sandbox lifecycle" \
      org.opencontainers.image.vendor="Microsoft" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.licenses="MIT"

ENTRYPOINT ["kars-controller"]

# SBOM generation: run `syft` on the final image to produce an SPDX SBOM
# Usage: docker buildx build --sbom=true or syft kars-controller:tag -o spdx-json
