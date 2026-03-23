# AgentMesh Relay Server
# Multi-stage build for minimal final image

FROM rust:1.94-alpine AS builder

# Install build dependencies
RUN apk add --no-cache musl-dev

WORKDIR /app

# Copy manifests
COPY Cargo.toml Cargo.lock* ./

# Create dummy source to cache dependencies
RUN mkdir src && echo "fn main() {}" > src/main.rs

RUN cargo build --release
RUN rm -rf src

# Copy actual source
COPY src ./src

# Build release binary
RUN touch src/main.rs && cargo build --release

# Final stage
FROM alpine:3.19

RUN apk add --no-cache ca-certificates

WORKDIR /app

# Copy binary from builder
COPY --from=builder /app/target/release/agentmesh-relay /app/relay

# Create non-root user
RUN adduser -D -u 1000 agentmesh
USER agentmesh

# Expose WebSocket port
EXPOSE 8765

# Health check
HEALTHCHECK --interval=30s --timeout=3s \
    CMD nc -z localhost 8765 || exit 1

ENV RUST_LOG=agentmesh_relay=info
ENV RELAY_ADDR=0.0.0.0:8765

CMD ["/app/relay"]
