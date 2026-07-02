# Kars datapath-witness aggregator image.
#
# A tiny toolbox that runs the aggregator loop in-cluster: kubectl-gadget (to
# read the continuous headless gadgets) + kubectl (to read declared allowlists
# and publish the verdict ConfigMap) + python3 (the cross-check). The loop
# script and compute step are mounted from a ConfigMap at /opt/witness, so the
# image is pure tooling and logic changes never require a rebuild.
ARG IG_VERSION=v0.53.2
ARG KUBECTL_VERSION=v1.31.4

FROM alpine:3.20 AS tools
ARG IG_VERSION
ARG KUBECTL_VERSION
ARG TARGETARCH=arm64
RUN apk add --no-cache curl tar
RUN curl -sSL "https://dl.k8s.io/release/${KUBECTL_VERSION}/bin/linux/${TARGETARCH}/kubectl" -o /kubectl \
    && chmod +x /kubectl
RUN curl -sSL "https://github.com/inspektor-gadget/inspektor-gadget/releases/download/${IG_VERSION}/kubectl-gadget-linux-${TARGETARCH}-${IG_VERSION}.tar.gz" \
    | tar -xz -C / kubectl-gadget

FROM alpine:3.20
RUN apk add --no-cache python3 bash coreutils \
    && adduser -D -u 1000 witness
COPY --from=tools /kubectl /usr/local/bin/kubectl
COPY --from=tools /kubectl-gadget /usr/local/bin/kubectl-gadget
USER 1000
ENTRYPOINT ["/bin/sh", "/opt/witness/publish-witness.sh"]
