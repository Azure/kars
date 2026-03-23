# AgentMesh Registry API
# Multi-stage build for minimal final image

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
FROM rust:1.94-alpine AS builder

# Use SQLx offline mode - no database needed during build
ENV SQLX_OFFLINE=true

# Install build dependencies
RUN apk add --no-cache musl-dev openssl-dev openssl-libs-static

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
FROM alpine:3.19

RUN apk add --no-cache ca-certificates

WORKDIR /app

# Copy binary and migrations from builder
COPY --from=builder /app/target/release/agentmesh-registry /app/registry
COPY --from=builder /app/migrations /app/migrations

# Copy frontend build output from frontend builder
COPY --from=frontend-builder /app/frontend/dist /app/static

# Create non-root user
RUN adduser -D -u 1000 agentmesh
USER agentmesh

# Expose HTTP port
EXPOSE 8080

# Health check
HEALTHCHECK --interval=30s --timeout=3s \
    CMD wget -q --spider http://localhost:8080/v1/health || exit 1

ENV RUST_LOG=agentmesh_registry=info,actix_web=info
ENV HOST=0.0.0.0
ENV PORT=8080

CMD ["/app/registry"]
