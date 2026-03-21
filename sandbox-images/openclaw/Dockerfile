# AzureClaw OpenClaw Sandbox Image
# Based on Azure Linux 3 for a Microsoft-maintained, hardened container base.
# This same image runs locally (azureclaw dev) and on AKS (azureclaw up).
#
# Build: docker build --platform linux/amd64 -f sandbox-images/openclaw/Dockerfile .
# (Azure Linux base images are x86_64 only; use --platform on ARM hosts)

ARG AZURELINUX_BASE=mcr.microsoft.com/azurelinux/base/core:3.0
ARG INFERENCE_ROUTER_IMAGE=azureclawacr.azurecr.io/azureclaw-inference-router:0.1.0

# ─── Use pre-built inference router image ────────────────────────────────────

FROM ${INFERENCE_ROUTER_IMAGE} AS router-bin

# ─── Node Builder (OpenClaw) ─────────────────────────────────────────────────

FROM ${AZURELINUX_BASE} AS builder

# Install Node.js 22 LTS (OpenClaw requires 22+, AL3 only has v20)
RUN tdnf install -y tar gzip ca-certificates curl git && tdnf clean all
RUN curl -fsSL https://nodejs.org/dist/v22.16.0/node-v22.16.0-linux-x64.tar.gz -o /tmp/node.tar.gz && \
    tar xzf /tmp/node.tar.gz -C /usr/local --strip-components=1 && \
    rm /tmp/node.tar.gz

# Install OpenClaw
RUN npm install -g openclaw@latest

# ─── Runtime Stage ────────────────────────────────────────────────────────────

FROM ${AZURELINUX_BASE}

# Install Node.js 22 LTS runtime and minimal tools
RUN tdnf install -y tar gzip curl git shadow-utils ca-certificates && tdnf clean all
RUN curl -fsSL https://nodejs.org/dist/v22.16.0/node-v22.16.0-linux-x64.tar.gz -o /tmp/node.tar.gz && \
    tar xzf /tmp/node.tar.gz -C /usr/local --strip-components=1 && \
    rm /tmp/node.tar.gz

# Security: create non-root user
RUN groupadd -g 1000 sandbox && \
    useradd -u 1000 -g sandbox -m -d /sandbox -s /bin/bash sandbox

# Set prompt for the sandbox user
RUN echo 'export PS1="azureclaw@\\h:\\w\\$ "' >> /sandbox/.bashrc

# Copy OpenClaw from builder (npm global root is /usr/local/lib/node_modules)
COPY --from=builder /usr/local/lib/node_modules /usr/local/lib/node_modules
RUN ln -sf /usr/local/lib/node_modules/openclaw/openclaw.mjs /usr/local/bin/openclaw

# Copy inference router from pre-built image
COPY --from=router-bin /usr/local/bin/azureclaw-inference-router /usr/local/bin/azureclaw-inference-router

# Copy AzureClaw OpenClaw plugin (pre-built dist/ + skills + policies)
COPY cli/dist/ /opt/azureclaw-plugin/
COPY cli/package.json cli/openclaw.plugin.json /opt/azureclaw-plugin/
COPY cli/skills/ /opt/azureclaw-plugin/skills/
COPY cli/policies/ /opt/azureclaw-plugin/policies/

# Ensure openclaw binary is linked
RUN ln -sf /usr/local/lib/node_modules/openclaw/openclaw.mjs /usr/local/bin/openclaw

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

# Switch to non-root user
USER sandbox
WORKDIR /sandbox

# Entrypoint auto-configures OpenClaw, then runs the command
ENTRYPOINT ["entrypoint.sh"]
CMD ["sleep", "infinity"]
