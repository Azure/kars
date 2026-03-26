# AzureClaw Inference Router — multi-stage Rust build (Azure Linux)
# Produces a minimal distroless binary (~15MB)

ARG AZURELINUX_BASE=mcr.microsoft.com/azurelinux/base/core:3.0@sha256:a452d39c91576f5a2c983c7d3b62521fabd08e16b4a7237e24bf2be3b06e1651
ARG VERSION=0.1.0

# ─── Builder ──────────────────────────────────────────────────────────────────

FROM ${AZURELINUX_BASE} AS builder

RUN tdnf install -y gcc glibc-devel binutils make pkg-config openssl-devel ca-certificates curl && tdnf clean all

# Install Rust via rustup
ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo
ENV PATH="/usr/local/cargo/bin:${PATH}"
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain 1.88.0 --profile minimal

WORKDIR /build
COPY Cargo.toml Cargo.lock* ./
COPY inference-router/ inference-router/
COPY controller/ controller/

RUN cargo build --release --package azureclaw-inference-router

# ─── Runtime ──────────────────────────────────────────────────────────────────

FROM ${AZURELINUX_BASE}
ARG VERSION=0.1.0

RUN tdnf install -y ca-certificates shadow-utils iptables && tdnf clean all

# Non-root user
RUN groupadd -g 1000 router && useradd -u 1000 -g router -s /sbin/nologin router

COPY --from=builder /build/target/release/azureclaw-inference-router /usr/local/bin/azureclaw-inference-router

USER router
EXPOSE 8443

LABEL org.opencontainers.image.title="AzureClaw Inference Router" \
      org.opencontainers.image.description="High-performance inference proxy for AzureClaw" \
      org.opencontainers.image.vendor="Microsoft" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.licenses="MIT"

ENTRYPOINT ["azureclaw-inference-router"]

# SBOM generation: run `syft` on the final image to produce an SPDX SBOM
# Usage: docker buildx build --sbom=true or syft azureclaw-inference-router:tag -o spdx-json
