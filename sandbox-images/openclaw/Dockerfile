# AzureClaw OpenClaw Sandbox Image
# Based on Azure Linux 3 for a Microsoft-maintained, hardened container base.
# This same image runs locally (azureclaw dev) and on AKS (azureclaw up).
# Multi-arch: builds natively on both x86_64 and ARM64.
#
# Build: docker build -f sandbox-images/openclaw/Dockerfile .

ARG AZURELINUX_BASE=mcr.microsoft.com/azurelinux/base/core:3.0
ARG INFERENCE_ROUTER_IMAGE=azureclawacr.azurecr.io/azureclaw-inference-router:latest

# ─── Use pre-built inference router image ────────────────────────────────────

FROM ${INFERENCE_ROUTER_IMAGE} AS router-bin

# ─── Node Builder (OpenClaw) ─────────────────────────────────────────────────

FROM ${AZURELINUX_BASE} AS builder

# Detect arch for Node.js download (x64 or arm64)
ARG TARGETARCH
RUN NODEJS_ARCH=$(case "${TARGETARCH:-$(uname -m)}" in arm64|aarch64) echo "arm64";; *) echo "x64";; esac) && \
    tdnf install -y tar gzip ca-certificates curl git && tdnf clean all && \
    curl -fsSL "https://nodejs.org/dist/v22.16.0/node-v22.16.0-linux-${NODEJS_ARCH}.tar.gz" -o /tmp/node.tar.gz && \
    tar xzf /tmp/node.tar.gz -C /usr/local --strip-components=1 && \
    rm /tmp/node.tar.gz

# Install OpenClaw
RUN npm install -g openclaw@latest

# Pre-install popular ClawHub skills at build time (VirusTotal-scanned).
# AzureClaw's runtime security (egress guard, Content Safety, read-only rootfs)
# provides defense-in-depth even if a skill is compromised.
RUN openclaw skills install summarize 2>/dev/null || true && \
    openclaw skills install browser 2>/dev/null || true && \
    openclaw skills install multi-search-engine 2>/dev/null || true && \
    openclaw skills install proactive-agent 2>/dev/null || true

# ─── Runtime Stage ────────────────────────────────────────────────────────────

FROM ${AZURELINUX_BASE}

# Detect arch for Node.js download
ARG TARGETARCH
RUN NODEJS_ARCH=$(case "${TARGETARCH:-$(uname -m)}" in arm64|aarch64) echo "arm64";; *) echo "x64";; esac) && \
    tdnf install -y tar gzip curl git shadow-utils ca-certificates iptables util-linux jq tmux && tdnf clean all && \
    curl -fsSL "https://nodejs.org/dist/v22.16.0/node-v22.16.0-linux-${NODEJS_ARCH}.tar.gz" -o /tmp/node.tar.gz && \
    tar xzf /tmp/node.tar.gz -C /usr/local --strip-components=1 && \
    rm /tmp/node.tar.gz

# Install GitHub CLI (enables github skill)
RUN GH_ARCH=$(case "${TARGETARCH:-$(uname -m)}" in arm64|aarch64) echo "arm64";; *) echo "amd64";; esac) && \
    curl -fsSL "https://github.com/cli/cli/releases/download/v2.88.1/gh_2.88.1_linux_${GH_ARCH}.tar.gz" -o /tmp/gh.tar.gz && \
    tar xzf /tmp/gh.tar.gz -C /usr/local --strip-components=1 && \
    rm /tmp/gh.tar.gz

# Security: create non-root user (sandbox=1000) and router user (router=1001)
# UID separation enables iptables egress guard: UID 1000 is restricted to
# localhost + DNS, while UID 1001 (router) can reach the internet for
# Foundry API calls and blocklist feed updates.
RUN groupadd -g 1000 sandbox && \
    useradd -u 1000 -g sandbox -m -d /sandbox -s /bin/bash sandbox && \
    groupadd -g 1001 router && \
    useradd -u 1001 -g router -M -d /tmp -s /bin/false router

# Set prompt for the sandbox user
RUN echo 'export PS1="azureclaw@\\h:\\w\\$ "' >> /sandbox/.bashrc

# Copy OpenClaw from builder (npm global root is /usr/local/lib/node_modules)
COPY --from=builder /usr/local/lib/node_modules /usr/local/lib/node_modules
RUN ln -sf /usr/local/lib/node_modules/openclaw/openclaw.mjs /usr/local/bin/openclaw

# Copy pre-installed ClawHub skills from builder (installed via openclaw skills install)
COPY --from=builder /root/.openclaw/workspace/skills/ /opt/clawhub-skills/

# Proxy bootstrap — preloaded by NODE_OPTIONS to set undici global dispatcher
COPY sandbox-images/openclaw/proxy-bootstrap.js /usr/local/lib/proxy-bootstrap.js

# Copy inference router from pre-built image
COPY --from=router-bin /usr/local/bin/azureclaw-inference-router /usr/local/bin/azureclaw-inference-router

# Copy AzureClaw OpenClaw plugin (pre-built dist/ + skills + policies)
COPY cli/dist/ /opt/azureclaw-plugin/
COPY cli/package.json cli/openclaw.plugin.json /opt/azureclaw-plugin/
COPY cli/skills/ /opt/azureclaw-plugin/skills/
COPY cli/policies/ /opt/azureclaw-plugin/policies/

# Install plugin runtime dependencies
RUN cd /opt/azureclaw-plugin && npm install --omit=dev --ignore-scripts 2>/dev/null || true
# Install vendored SDK with its dependencies (libsodium-wrappers etc.)
COPY vendor/agentmesh-sdk/package.json /opt/azureclaw-vendored-sdk/package.json
COPY vendor/agentmesh-sdk/dist/ /opt/azureclaw-vendored-sdk/dist/
RUN cd /opt/azureclaw-vendored-sdk && npm install --omit=dev --ignore-scripts 2>/dev/null || true
# Overlay vendored SDK into plugin node_modules (patched dist + deps)
RUN mkdir -p /opt/azureclaw-plugin/node_modules/@agentmesh/sdk && \
    cp -r /opt/azureclaw-vendored-sdk/dist /opt/azureclaw-plugin/node_modules/@agentmesh/sdk/ && \
    cp /opt/azureclaw-vendored-sdk/package.json /opt/azureclaw-plugin/node_modules/@agentmesh/sdk/ && \
    cp -r /opt/azureclaw-vendored-sdk/node_modules/* /opt/azureclaw-plugin/node_modules/ 2>/dev/null || true

# Ensure openclaw binary is linked
RUN ln -sf /usr/local/lib/node_modules/openclaw/openclaw.mjs /usr/local/bin/openclaw

# Symlink Control UI assets (the gateway expects dist/control-ui/)
RUN ln -s /usr/local/lib/node_modules/openclaw/dist/canvas-host/a2ui \
          /usr/local/lib/node_modules/openclaw/dist/control-ui

# Create writable directories
RUN mkdir -p /sandbox /tmp && \
    chown -R sandbox:sandbox /sandbox /tmp

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
# This mirrors AKS where router is a separate sidecar container.
WORKDIR /sandbox

# Entrypoint auto-configures OpenClaw, then runs the command
ENTRYPOINT ["entrypoint.sh"]
CMD ["sleep", "infinity"]
