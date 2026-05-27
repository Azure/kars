# kars Conformance Runner — multi-stage Rust build (Azure Linux)
#
# Produces a small image whose single entrypoint is the
# `kars-conformance-runner` binary (Slice 6.2). A `KarsEval` run
# spawns this as an ephemeral Job; the runner replays an EvalCorpus
# against the live inference router and writes a `RunReport` JSON
# document to `--output` (and stdout). No kube-apiserver access; no
# secrets minted in-container.

ARG AZURELINUX_BASE=mcr.microsoft.com/azurelinux/base/core:3.0@sha256:35149ae8dd179684f969944f54a337c665a64e702486154eb44253fb39c2505b
ARG VERSION=0.1.0

# ─── Builder ──────────────────────────────────────────────────────────────────

FROM ${AZURELINUX_BASE} AS builder

RUN tdnf install -y gcc glibc-devel binutils make pkg-config openssl-devel ca-certificates curl kernel-headers && tdnf clean all

ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo
ENV PATH="/usr/local/cargo/bin:${PATH}"
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain 1.88.0 --profile minimal

WORKDIR /build
COPY Cargo.toml Cargo.lock* ./
COPY conformance-runner/ conformance-runner/
COPY eval-corpus/ eval-corpus/
COPY controller/ controller/
COPY inference-router/ inference-router/
COPY a2a-gateway/ a2a-gateway/
COPY kars-a2a-core/ kars-a2a-core/
COPY tests/ tests/
COPY cli/blocklists/ cli/blocklists/
COPY cli/profiles/agt/ cli/profiles/agt/

RUN cargo build --release --package kars-conformance-runner

# ─── Runtime ──────────────────────────────────────────────────────────────────

FROM ${AZURELINUX_BASE}

RUN tdnf install -y ca-certificates shadow-utils && tdnf clean all

RUN groupadd -g 1000 runner && useradd -u 1000 -g runner -s /sbin/nologin runner

COPY --from=builder /build/target/release/kars-conformance-runner /usr/local/bin/kars-conformance-runner

# Reports are written here by default; mount a Volume (emptyDir is fine)
# so the spawning reconciler can read the report back out.
RUN mkdir -p /var/run/conformance && chown runner:runner /var/run/conformance

USER runner

LABEL org.opencontainers.image.title="kars Conformance Runner" \
      org.opencontainers.image.description="Replays an EvalCorpus against the kars inference router and emits a RunReport JSON document" \
      org.opencontainers.image.vendor="Microsoft" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.licenses="MIT"

ENTRYPOINT ["kars-conformance-runner"]
