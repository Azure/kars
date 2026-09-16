# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# kars Bridge BFF — build with Rust, ship on the same Azure Linux 3 distroless
# runtime as the controller and inference router. The bookworm build's glibc
# is older than the runtime's; no build tools or Debian packages are copied.
#
# Build from the bff/ directory as context:
#   docker build -f bff/Dockerfile -t <registry>/kars-bridge-bff:<tag> bff
ARG AZURELINUX_DISTROLESS=mcr.microsoft.com/azurelinux/distroless/base:3.0
FROM rust:1-bookworm AS build
WORKDIR /src
# Compile only the real sources against the reviewed lockfile.
COPY . .
RUN cargo build --release --locked && strip target/release/kars-bridge-bff

FROM ${AZURELINUX_DISTROLESS} AS runtime
COPY --from=build /src/target/release/kars-bridge-bff /usr/local/bin/kars-bridge-bff
USER 10001:10001
EXPOSE 8081
# The BFF binds BRIDGE_BFF_HOST:BRIDGE_BFF_PORT; in-cluster it must listen on all
# interfaces. The ServiceAccount token is mounted by Kubernetes and picked up by
# the in-cluster kube config automatically.
ENV BRIDGE_BFF_HOST=0.0.0.0 BRIDGE_BFF_PORT=8081
ENTRYPOINT ["/usr/local/bin/kars-bridge-bff"]
