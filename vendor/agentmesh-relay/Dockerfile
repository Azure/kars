# AgentMesh Relay Server
# Multi-stage build for minimal final image (Azure Linux 3)

ARG AZURELINUX_BASE=mcr.microsoft.com/azurelinux/base/core:3.0

# ─── Builder ──────────────────────────────────────────────────────────────────

FROM ${AZURELINUX_BASE} AS builder

RUN tdnf install -y gcc glibc-devel binutils make pkg-config ca-certificates curl kernel-headers && tdnf clean all

# Install Rust via rustup
ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo
ENV PATH="/usr/local/cargo/bin:${PATH}"
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain 1.88.0 --profile minimal

WORKDIR /app

# Copy manifests
COPY Cargo.toml Cargo.lock* ./

# Create dummy source to cache dependencies
RUN mkdir src && echo "fn main() {}" > src/main.rs

RUN cargo build --release
RUN rm -rf src

# Copy actual source
COPY src ./src

# Cache-bust: changing this ARG invalidates the cargo build layer
ARG CACHE_BUST=0

# Build release binary
RUN touch src/main.rs && cargo build --release

# ─── Runtime ──────────────────────────────────────────────────────────────────

FROM ${AZURELINUX_BASE}

RUN tdnf install -y ca-certificates shadow-utils && tdnf clean all

WORKDIR /app

# Copy binary from builder
COPY --from=builder /app/target/release/agentmesh-relay /app/relay

# Create non-root user
RUN useradd -r -u 1000 -M -s /sbin/nologin agentmesh
USER agentmesh

# Expose WebSocket port
EXPOSE 8765

ENV RUST_LOG=agentmesh_relay=info
ENV RELAY_ADDR=0.0.0.0:8765

CMD ["/app/relay"]
