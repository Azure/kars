# AgentMesh Registry API
# Multi-stage build for minimal final image (Azure Linux 3)

ARG AZURELINUX_BASE=mcr.microsoft.com/azurelinux/base/core:3.0

# Stage 1: Build React frontend
FROM node:20-alpine AS frontend-builder

WORKDIR /app/frontend

# Copy frontend package files
COPY frontend/package*.json ./

# Install dependencies
RUN npm ci

# Copy frontend source
COPY frontend/ ./

# Build frontend
RUN npm run build

# Stage 2: Build Rust backend
FROM ${AZURELINUX_BASE} AS builder

# Use SQLx offline mode - no database needed during build
ENV SQLX_OFFLINE=true

# Install build dependencies
RUN tdnf install -y gcc glibc-devel binutils make pkg-config openssl-devel ca-certificates curl kernel-headers zstd-devel && tdnf clean all

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

# Copy actual source, migrations, and SQLx cache
COPY src ./src
COPY migrations ./migrations
COPY .sqlx ./.sqlx

# Build release binary
RUN touch src/main.rs && cargo build --release

# Stage 3: Final runtime image
FROM ${AZURELINUX_BASE}

RUN tdnf install -y ca-certificates shadow-utils curl && tdnf clean all

WORKDIR /app

# Copy binary and migrations from builder
COPY --from=builder /app/target/release/agentmesh-registry /app/registry
COPY --from=builder /app/migrations /app/migrations

# Copy frontend build output from frontend builder
COPY --from=frontend-builder /app/frontend/dist /app/static

# Create non-root user
RUN useradd -r -u 1000 -M -s /sbin/nologin agentmesh
USER agentmesh

# Expose HTTP port
EXPOSE 8080

ENV RUST_LOG=agentmesh_registry=info,actix_web=info
ENV HOST=0.0.0.0
ENV PORT=8080

CMD ["/app/registry"]
