# AzureClaw OpenClaw Sandbox Image
# Based on Azure Linux 4 for a Microsoft-maintained, hardened container base.
#
# Azure Linux 4 Alpha is available via ACR (limited availability):
#   azlpubstagingacroxz2o4gw.azurecr.io/azurelinux/base/core:4.0
#
# ⚠ Alpha access required — see:
#   Azure Linux 4:          https://eng.ms/docs/products/azure-linux/overview/AzureLinux4Alpha1
#   Azure Container Linux:  https://dev.azure.com/mariner-org/mariner/_wiki/wikis/Azure%20Container%20Linux%20Plan/6490/Azure-Container-Linux-Alpha
#
# This same image runs locally (azureclaw dev) and on AKS (azureclaw up).

ARG AZURELINUX_BASE=mcr.microsoft.com/azurelinux/base/core:3.0
ARG INFERENCE_ROUTER_IMAGE=azureclawacr.azurecr.io/azureclaw-inference-router:0.1.0

# ─── Use pre-built inference router image ────────────────────────────────────

FROM ${INFERENCE_ROUTER_IMAGE} AS router-bin

# ─── Node Builder (OpenClaw) ─────────────────────────────────────────────────

FROM ${AZURELINUX_BASE} AS builder

# Install Node.js 20, npm, git, ca-certificates (AL3 bundles npm with nodejs)
RUN tdnf install -y nodejs npm git ca-certificates && \
    tdnf clean all

# Install OpenClaw
RUN npm install -g openclaw@latest

# ─── Runtime Stage ────────────────────────────────────────────────────────────

FROM ${AZURELINUX_BASE}

# Install Node.js runtime and minimal tools
RUN tdnf install -y nodejs git shadow-utils && \
    tdnf clean all

# Security: create non-root user
RUN groupadd -g 1000 sandbox && \
    useradd -u 1000 -g sandbox -m -d /sandbox -s /bin/bash sandbox

# Set prompt for the sandbox user
RUN echo 'export PS1="azureclaw@\\h:\\w\\$ "' >> /sandbox/.bashrc

# Copy OpenClaw from builder (AL3 npm global root is /usr/lib/node_modules)
COPY --from=builder /usr/lib/node_modules /usr/lib/node_modules
RUN ln -s /usr/lib/node_modules/openclaw/openclaw.mjs /usr/local/bin/openclaw

# Copy inference router from pre-built image
COPY --from=router-bin /usr/local/bin/azureclaw-inference-router /usr/local/bin/azureclaw-inference-router

# Copy AzureClaw OpenClaw plugin (pre-built dist/)
COPY cli/dist/ /opt/azureclaw-plugin/
COPY cli/package.json cli/openclaw.plugin.json /opt/azureclaw-plugin/

# Ensure openclaw binary is linked (must use openclaw.mjs entry for plugin CLI support)
RUN ln -sf /usr/lib/node_modules/openclaw/openclaw.mjs /usr/local/bin/openclaw

# Create writable directories
RUN mkdir -p /sandbox /tmp && \
    chown -R sandbox:sandbox /sandbox /tmp

# Copy entrypoint that auto-configures OpenClaw from mounted secrets
COPY sandbox-images/openclaw/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

# Labels
LABEL org.opencontainers.image.title="AzureClaw OpenClaw Sandbox" \
      org.opencontainers.image.description="Hardened OpenClaw sandbox on Azure Linux 4" \
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
