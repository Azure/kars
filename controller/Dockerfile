# AzureClaw Controller — multi-stage Rust build (Azure Linux)

ARG AZURELINUX_BASE=mcr.microsoft.com/azurelinux/base/core:3.0

# ─── Builder ──────────────────────────────────────────────────────────────────

FROM ${AZURELINUX_BASE} AS builder

RUN tdnf install -y gcc glibc-devel binutils make pkg-config openssl-devel ca-certificates curl && tdnf clean all

# Install Rust via rustup (no distro packages needed)
ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo
ENV PATH="/usr/local/cargo/bin:${PATH}"
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain 1.88.0 --profile minimal

WORKDIR /build
COPY Cargo.toml Cargo.lock* ./
COPY controller/ controller/
COPY inference-router/ inference-router/

RUN cargo build --release --package azureclaw-controller

# ─── Runtime ──────────────────────────────────────────────────────────────────

FROM ${AZURELINUX_BASE}

RUN tdnf install -y ca-certificates shadow-utils && tdnf clean all

# Non-root user
RUN groupadd -g 1000 controller && useradd -u 1000 -g controller -s /sbin/nologin controller

COPY --from=builder /build/target/release/azureclaw-controller /usr/local/bin/azureclaw-controller

USER controller

LABEL org.opencontainers.image.title="AzureClaw Controller" \
      org.opencontainers.image.description="Kubernetes operator for AzureClaw sandbox lifecycle" \
      org.opencontainers.image.vendor="Microsoft" \
      org.opencontainers.image.licenses="MIT"

ENTRYPOINT ["azureclaw-controller"]
