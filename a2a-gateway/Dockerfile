# syntax=docker/dockerfile:1.7
#
# `azureclaw-a2a-gateway` — distroless static image for the public
# A2A ingress edge (Phase 2 S3.5, ADR-0001 #4).
#
# Two-stage build:
#   1. `builder` (rust:bookworm) — cargo build with musl target.
#   2. `runtime` (gcr.io/distroless/static) — copy the static binary,
#      no shell, no package manager, no libc.
#
# Hardening:
#   - musl static link → no glibc symbol surface.
#   - distroless static → no shell, no `apt`, no SSH agent.
#   - runs as nonroot (UID 65532 in distroless, K8s overrides to 1002).
#   - Single ENTRYPOINT, no CMD shell expansion.

FROM rust:1.88-bookworm AS builder

# musl cross-compile toolchain
RUN rustup target add x86_64-unknown-linux-musl \
    && apt-get update \
    && apt-get install -y --no-install-recommends musl-tools \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /work

# Bring in the workspace manifest + every crate Cargo.toml so cargo
# can resolve the workspace lock graph. Source files come next; this
# ordering lets Docker cache the dependency build separately from
# our own code edits.
COPY Cargo.toml Cargo.lock ./
COPY a2a-gateway/Cargo.toml a2a-gateway/Cargo.toml
COPY azureclaw-a2a-core/Cargo.toml azureclaw-a2a-core/Cargo.toml
COPY inference-router/Cargo.toml inference-router/Cargo.toml
COPY controller/Cargo.toml controller/Cargo.toml
COPY tests/chaos/Cargo.toml tests/chaos/Cargo.toml
COPY tests/cncf-conformance/Cargo.toml tests/cncf-conformance/Cargo.toml

# Source. Workspace-wide copy is wasteful but keeps the image build
# context honest with the workspace shape (cargo refuses to build a
# member without seeing the others' manifests).
COPY a2a-gateway/src a2a-gateway/src
COPY a2a-gateway/testdata a2a-gateway/testdata
COPY azureclaw-a2a-core/src azureclaw-a2a-core/src
COPY inference-router/src inference-router/src
COPY controller/src controller/src
COPY tests/chaos/src tests/chaos/src
COPY tests/cncf-conformance/src tests/cncf-conformance/src

RUN cargo build \
        --release \
        --bin azureclaw-a2a-gateway \
        --target x86_64-unknown-linux-musl

FROM gcr.io/distroless/static-debian12:nonroot

COPY --from=builder \
    /work/target/x86_64-unknown-linux-musl/release/azureclaw-a2a-gateway \
    /usr/local/bin/azureclaw-a2a-gateway

USER 65532:65532

EXPOSE 8443 9090

ENTRYPOINT ["/usr/local/bin/azureclaw-a2a-gateway"]
