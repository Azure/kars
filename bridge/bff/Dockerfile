# kars Bridge BFF — container image. Multi-stage: build the Rust binary against a
# glibc base, then ship it on a slim Debian runtime. Cloud-agnostic: the image
# runs identically on AKS, EKS, GKE, and local kind.
#
# Build from the bff/ directory as context:
#   docker build -f bff/Dockerfile -t <registry>/kars-bridge-bff:<tag> bff
FROM rust:1-bookworm AS build
WORKDIR /src
# Compile only the real sources against the reviewed lockfile.
COPY . .
RUN cargo build --release --locked && strip target/release/kars-bridge-bff

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/* && \
    useradd --uid 10001 --user-group --home-dir /home/bridge --create-home bridge
COPY --from=build /src/target/release/kars-bridge-bff /usr/local/bin/kars-bridge-bff
USER 10001
EXPOSE 8081
# The BFF binds BRIDGE_BFF_HOST:BRIDGE_BFF_PORT; in-cluster it must listen on all
# interfaces. The ServiceAccount token is mounted by Kubernetes and picked up by
# the in-cluster kube config automatically.
ENV BRIDGE_BFF_HOST=0.0.0.0 BRIDGE_BFF_PORT=8081
ENTRYPOINT ["/usr/local/bin/kars-bridge-bff"]
