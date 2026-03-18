# AzureClaw Inference Router — multi-stage Rust build
# Produces a minimal distroless binary (~15MB)

ARG AZURELINUX_BASE=azlpubstagingacroxz2o4gw.azurecr.io/azurelinux/base/core:4.0

# ─── Builder ──────────────────────────────────────────────────────────────────

FROM rust:1.85-slim AS builder

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY Cargo.toml Cargo.lock* ./
COPY inference-router/ inference-router/
COPY controller/ controller/

RUN cargo build --release --package azureclaw-inference-router

# ─── Runtime ──────────────────────────────────────────────────────────────────

FROM ${AZURELINUX_BASE}

RUN tdnf install -y ca-certificates shadow-utils && tdnf clean all

# Non-root user
RUN groupadd -g 1000 router && useradd -u 1000 -g router -s /sbin/nologin router

COPY --from=builder /build/target/release/azureclaw-inference-router /usr/local/bin/azureclaw-inference-router

USER router
EXPOSE 8443

LABEL org.opencontainers.image.title="AzureClaw Inference Router" \
      org.opencontainers.image.description="High-performance inference proxy for AzureClaw" \
      org.opencontainers.image.vendor="Microsoft" \
      org.opencontainers.image.licenses="MIT"

ENTRYPOINT ["azureclaw-inference-router"]
