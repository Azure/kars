# AzureClaw Controller — multi-stage Rust build

ARG AZURELINUX_BASE=azlpubstagingacroxz2o4gw.azurecr.io/azurelinux/base/core:4.0

# ─── Builder ──────────────────────────────────────────────────────────────────

FROM rust:1.85-slim AS builder

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY Cargo.toml Cargo.lock* ./
COPY controller/ controller/
COPY inference-router/ inference-router/

RUN cargo build --release --package azureclaw-controller

# ─── Runtime ──────────────────────────────────────────────────────────────────

FROM ${AZURELINUX_BASE}

RUN dnf install -y ca-certificates shadow-utils && dnf clean all

# Non-root user
RUN groupadd -g 1000 controller && useradd -u 1000 -g controller -s /sbin/nologin controller

COPY --from=builder /build/target/release/azureclaw-controller /usr/local/bin/azureclaw-controller

USER controller

LABEL org.opencontainers.image.title="AzureClaw Controller" \
      org.opencontainers.image.description="Kubernetes operator for AzureClaw sandbox lifecycle" \
      org.opencontainers.image.vendor="Microsoft" \
      org.opencontainers.image.licenses="MIT"

ENTRYPOINT ["azureclaw-controller"]
