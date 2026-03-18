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

# ─── Rust Builder (inference router) ─────────────────────────────────────────

FROM rust:slim AS rust-builder

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY Cargo.toml Cargo.lock* ./
COPY inference-router/ inference-router/
COPY controller/ controller/

RUN cargo build --release --package azureclaw-inference-router

# ─── Node Builder (OpenClaw) ─────────────────────────────────────────────────

FROM ${AZURELINUX_BASE} AS builder

# Install Node.js 22, npm, git
RUN dnf install -y nodejs22 npm git && \
    dnf clean all

# Install OpenClaw
RUN npm install -g openclaw@latest

# ─── Runtime Stage ────────────────────────────────────────────────────────────

FROM ${AZURELINUX_BASE}

# Install Node.js runtime and minimal tools
RUN dnf install -y nodejs22 git shadow-utils && \
    dnf clean all

# Security: create non-root user
RUN groupadd -g 1000 sandbox && \
    useradd -u 1000 -g sandbox -m -d /sandbox -s /bin/bash sandbox

# Set prompt for the sandbox user
RUN echo 'export PS1="azureclaw@\\h:\\w\\$ "' >> /sandbox/.bashrc

# Copy OpenClaw from builder
COPY --from=builder /usr/local/lib/node_modules /usr/local/lib/node_modules
COPY --from=builder /usr/local/bin/openclaw /usr/local/bin/openclaw

# Copy inference router from Rust builder
COPY --from=rust-builder /build/target/release/azureclaw-inference-router /usr/local/bin/azureclaw-inference-router

# Ensure openclaw binary is linked
RUN ln -sf /usr/local/lib/node_modules/openclaw/dist/index.js /usr/local/bin/openclaw

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
      org.opencontainers.image.base.name="azurelinux/base/core:4.0"

# Switch to non-root user
USER sandbox
WORKDIR /sandbox

# Entrypoint auto-configures OpenClaw, then runs the command
ENTRYPOINT ["entrypoint.sh"]
CMD ["sleep", "infinity"]
