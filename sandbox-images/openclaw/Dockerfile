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

ARG AZURELINUX_BASE=azlpubstagingacroxz2o4gw.azurecr.io/azurelinux/base/core:4.0

# ─── Builder Stage ────────────────────────────────────────────────────────────

FROM ${AZURELINUX_BASE} AS builder

# Install Node.js 22, git, curl
RUN tdnf install -y nodejs22 npm git curl ca-certificates && \
    tdnf clean all

# Install OpenClaw
RUN npm install -g openclaw@latest

# ─── Runtime Stage ────────────────────────────────────────────────────────────

FROM ${AZURELINUX_BASE}

# Install Node.js runtime only (no npm in prod)
RUN tdnf install -y nodejs22 git curl ca-certificates shadow-utils && \
    tdnf clean all

# Security: create non-root user
RUN groupadd -g 1000 sandbox && \
    useradd -u 1000 -g sandbox -m -d /sandbox -s /bin/bash sandbox

# Copy OpenClaw from builder
COPY --from=builder /usr/local/lib/node_modules /usr/local/lib/node_modules
COPY --from=builder /usr/local/bin/openclaw /usr/local/bin/openclaw

# Ensure openclaw binary is linked
RUN ln -sf /usr/local/lib/node_modules/openclaw/dist/index.js /usr/local/bin/openclaw

# Create writable directories
RUN mkdir -p /sandbox /tmp && \
    chown -R sandbox:sandbox /sandbox /tmp

# Labels
LABEL org.opencontainers.image.title="AzureClaw OpenClaw Sandbox" \
      org.opencontainers.image.description="Hardened OpenClaw sandbox on Azure Linux 4" \
      org.opencontainers.image.vendor="Microsoft" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.source="https://github.com/Azure/azureclaw" \
      org.opencontainers.image.base.name="azurelinux/base/core:4.0"

# Switch to non-root user
USER sandbox
WORKDIR /sandbox

# Default: start OpenClaw gateway
CMD ["openclaw", "gateway", "--port", "18789"]
