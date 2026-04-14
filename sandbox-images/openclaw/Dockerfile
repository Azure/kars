# AzureClaw OpenClaw Sandbox Image
# Slim overlay on top of azureclaw-sandbox-base — adds only the router binary,
# AzureClaw plugin, vendored SDK, and entrypoint.
#
# Base image (Dockerfile.base) contains: OS, Node.js, Python, Go tools, OpenClaw.
# This image adds: router binary, plugin code, SDK patches, entrypoint.
#
# Dev:  docker build -f sandbox-images/openclaw/Dockerfile .
# Prod: docker build --platform linux/amd64 -f sandbox-images/openclaw/Dockerfile .
#
# Expects base image to be pre-built:
#   Dev:  azureclaw-sandbox-base:dev
#   Prod: ${ACR}/azureclaw-sandbox-base:latest

ARG SANDBOX_BASE_IMAGE=azureclaw-sandbox-base:dev
ARG INFERENCE_ROUTER_IMAGE=azureclawacr.azurecr.io/azureclaw-inference-router:latest

# ─── Use pre-built inference router image ────────────────────────────────────

FROM ${INFERENCE_ROUTER_IMAGE} AS router-bin

# ─── CLI Plugin Builder (TypeScript → JavaScript) ────────────────────────────
# Uses the base image (has Node.js) to avoid a separate Node.js download.

FROM ${SANDBOX_BASE_IMAGE} AS cli-builder

WORKDIR /build
COPY cli/package.json cli/package-lock.json cli/tsconfig.json ./cli/
COPY cli/src/ ./cli/src/
COPY policy-engine/profiles/ ./policy-engine/profiles/
RUN cd cli && npm ci --ignore-scripts && npm run build

# ─── Runtime Stage (overlay on base) ─────────────────────────────────────────

FROM ${SANDBOX_BASE_IMAGE}

# Proxy bootstrap — preloaded by NODE_OPTIONS to set undici global dispatcher
COPY sandbox-images/openclaw/proxy-bootstrap.js /usr/local/lib/proxy-bootstrap.js

# Copy inference router from pre-built image
COPY --from=router-bin /usr/local/bin/azureclaw-inference-router /usr/local/bin/azureclaw-inference-router

# Copy AzureClaw OpenClaw plugin (built from source in cli-builder stage)
COPY --from=cli-builder /build/cli/dist/ /opt/azureclaw-plugin/
COPY cli/package.json cli/package-lock.json cli/openclaw.plugin.json /opt/azureclaw-plugin/
COPY cli/skills/ /opt/azureclaw-plugin/skills/
COPY cli/policies/ /opt/azureclaw-plugin/policies/

# Install plugin runtime dependencies
RUN cd /opt/azureclaw-plugin && npm ci --omit=dev --ignore-scripts

# Install vendored SDK with its dependencies (libsodium-wrappers etc.)
COPY vendor/agentmesh-sdk/package.json vendor/agentmesh-sdk/package-lock.json vendor/agentmesh-sdk/tsup.config.ts /opt/azureclaw-vendored-sdk/
COPY vendor/agentmesh-sdk/src/ /opt/azureclaw-vendored-sdk/src/
COPY vendor/agentmesh-sdk/dist/ /opt/azureclaw-vendored-sdk/dist/
RUN cd /opt/azureclaw-vendored-sdk && npm ci --omit=dev --ignore-scripts

# Overlay vendored SDK into plugin node_modules (patched dist + deps)
RUN mkdir -p /opt/azureclaw-plugin/node_modules/@agentmesh/sdk && \
    cp -r /opt/azureclaw-vendored-sdk/dist /opt/azureclaw-plugin/node_modules/@agentmesh/sdk/ && \
    cp /opt/azureclaw-vendored-sdk/package.json /opt/azureclaw-plugin/node_modules/@agentmesh/sdk/ && \
    cp -r /opt/azureclaw-vendored-sdk/node_modules/* /opt/azureclaw-plugin/node_modules/ 2>/dev/null || true

# Harden image-level code: root-owned, read-only — prevents agent from tampering
# with the master copy that gets re-installed on every container start.
RUN chmod -R a-w /opt/azureclaw-plugin /opt/azureclaw-vendored-sdk && \
    chmod -R a+rX /opt/azureclaw-plugin /opt/azureclaw-vendored-sdk

# Copy entrypoint that auto-configures OpenClaw from mounted secrets
COPY sandbox-images/openclaw/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

# Labels
LABEL org.opencontainers.image.title="AzureClaw OpenClaw Sandbox" \
      org.opencontainers.image.description="Hardened OpenClaw sandbox on Azure Linux 3" \
      org.opencontainers.image.vendor="Microsoft" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.source="https://github.com/Azure/azureclaw" \
      org.opencontainers.image.base.name="azurelinux/base/core:3.0"

# Entrypoint runs as root to set up iptables and start services under correct UIDs:
#   UID 1001 (router)  — can reach the internet (Foundry API, blocklist feeds)
#   UID 1000 (sandbox) — restricted to localhost + DNS by iptables egress guard
# This mirrors AKS where router is a separate container in the pod.
WORKDIR /sandbox

# Entrypoint auto-configures OpenClaw, then runs the command
ENTRYPOINT ["entrypoint.sh"]
CMD ["sleep", "infinity"]
