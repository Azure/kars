# AzureClaw OpenClaw Sandbox Image
# Based on Azure Linux 3 for a Microsoft-maintained, hardened container base.
# This same image runs locally (azureclaw dev) and on AKS (azureclaw up).
# Multi-arch: builds natively on both x86_64 and ARM64.
#
# Build: docker build -f sandbox-images/openclaw/Dockerfile .

ARG AZURELINUX_BASE=mcr.microsoft.com/azurelinux/base/core:3.0@sha256:a452d39c91576f5a2c983c7d3b62521fabd08e16b4a7237e24bf2be3b06e1651
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
# NOTE: Docker caches RUN layers by instruction text, so @latest won't
# re-pull once cached.  Pass --build-arg OPENCLAW_CACHE_BUST=$(date +%s)
# or change the value below to force a fresh pull.
ARG OPENCLAW_VERSION=latest
ARG OPENCLAW_CACHE_BUST=0
RUN echo "openclaw cache-bust: ${OPENCLAW_CACHE_BUST}" && npm install -g openclaw@${OPENCLAW_VERSION}

# Patch vulnerable transitive npm deps (tar, minimatch, glob, brace-expansion)
# without waiting for upstream OpenClaw to update them.
RUN cd /usr/local/lib/node_modules && npm audit fix --force 2>/dev/null || true

# Pre-install popular ClawHub skills at build time (VirusTotal-scanned).
# AzureClaw's runtime security (egress guard, Content Safety, read-only rootfs)
# provides defense-in-depth even if a skill is compromised.
RUN openclaw skills install summarize 2>/dev/null || true && \
    openclaw skills install browser 2>/dev/null || true && \
    openclaw skills install multi-search-engine 2>/dev/null || true && \
    openclaw skills install proactive-agent 2>/dev/null || true && \
    mkdir -p /root/.openclaw/workspace/skills

# Install npm CLIs that unlock built-in OpenClaw skills
RUN npm install -g clawhub mcporter @steipete/oracle 2>/dev/null || true

# ─── CLI Plugin Builder (TypeScript → JavaScript) ────────────────────────────

FROM ${AZURELINUX_BASE} AS cli-builder

ARG TARGETARCH
RUN NODEJS_ARCH=$(case "${TARGETARCH:-$(uname -m)}" in arm64|aarch64) echo "arm64";; *) echo "x64";; esac) && \
    tdnf install -y tar gzip ca-certificates curl && tdnf clean all && \
    curl -fsSL "https://nodejs.org/dist/v22.16.0/node-v22.16.0-linux-${NODEJS_ARCH}.tar.gz" -o /tmp/node.tar.gz && \
    tar xzf /tmp/node.tar.gz -C /usr/local --strip-components=1 && \
    rm /tmp/node.tar.gz

WORKDIR /build
COPY cli/package.json cli/tsconfig.json ./cli/
COPY cli/src/ ./cli/src/
COPY policy-engine/profiles/ ./policy-engine/profiles/
RUN cd cli && npm install --ignore-scripts 2>/dev/null || true && npm run build

# ─── Go Builder (CLI tools for OpenClaw skills) ──────────────────────────────

FROM golang:1.25-alpine AS go-builder

# Build Go CLI tools that enable built-in OpenClaw skills.
# Each tool is a small static binary (~5-15MB) — discarded build cache doesn't bloat the runtime image.
# GOTOOLCHAIN=auto lets Go download a newer toolchain if a module requires it (e.g. go >= 1.25).
RUN apk add --no-cache git && \
    GOTOOLCHAIN=auto CGO_ENABLED=0 go install github.com/steipete/blucli/cmd/blu@latest && \
    GOTOOLCHAIN=auto CGO_ENABLED=0 go install github.com/steipete/eightctl/cmd/eightctl@latest && \
    GOTOOLCHAIN=auto CGO_ENABLED=0 go install github.com/steipete/gifgrep/cmd/gifgrep@latest && \
    GOTOOLCHAIN=auto CGO_ENABLED=0 go install github.com/steipete/ordercli/cmd/ordercli@latest && \
    GOTOOLCHAIN=auto CGO_ENABLED=0 go install github.com/steipete/sonoscli/cmd/sonos@latest && \
    GOTOOLCHAIN=auto CGO_ENABLED=0 go install github.com/steipete/wacli/cmd/wacli@latest && \
    GOTOOLCHAIN=auto CGO_ENABLED=0 go install github.com/steipete/camsnap/cmd/camsnap@latest && \
    GOTOOLCHAIN=auto CGO_ENABLED=0 go install github.com/steipete/goplaces/cmd/goplaces@latest && \
    GOTOOLCHAIN=auto CGO_ENABLED=0 go install github.com/steipete/songsee/cmd/songsee@latest

# ─── Runtime Stage ────────────────────────────────────────────────────────────

FROM ${AZURELINUX_BASE}

# Detect arch for Node.js download
ARG TARGETARCH
RUN tdnf install -y tar gzip curl git shadow-utils ca-certificates iptables util-linux jq tmux unzip \
                    python3 python3-pip && tdnf clean all

# Vendored Python packages (zero PyPI dependency)
COPY vendor/sandbox-wheels/ /tmp/sandbox-wheels/
RUN pip3 install --no-cache-dir --no-index --find-links=/tmp/sandbox-wheels/ \
      requests httpx beautifulsoup4 lxml cssselect \
      pandas numpy matplotlib seaborn scipy sympy \
      pyyaml toml python-dotenv \
      aiohttp websockets \
      tabulate rich \
      pdfplumber pypdf python-docx openpyxl python-pptx \
      Pillow chardet python-dateutil \
      xmltodict html5lib markdown html2text jinja2 \
      pydantic jsonpath-ng \
      sqlalchemy cryptography tiktoken \
      dnspython networkx geopy \
      ftfy unidecode qrcode fpdf2 nano-pdf && \
    rm -rf /tmp/sandbox-wheels

# Node.js (vendored binary)
RUN NODEJS_ARCH=$(case "${TARGETARCH:-$(uname -m)}" in arm64|aarch64) echo "arm64";; *) echo "x64";; esac) && \
    curl -fsSL "https://nodejs.org/dist/v22.16.0/node-v22.16.0-linux-${NODEJS_ARCH}.tar.gz" -o /tmp/node.tar.gz && \
    tar xzf /tmp/node.tar.gz -C /usr/local --strip-components=1 && \
    rm /tmp/node.tar.gz

# AGT governance SDK — install from vendored wheels (GitHub release v3.0.0)
# Avoids PyPI dependency during build (files.pythonhosted.org can be unreachable)
# Includes agentmesh-platform, agent-os-kernel, agent-hypervisor, and their
# runtime dependencies (email-validator, jsonschema).
# pydantic/pyyaml/dnspython are already installed by the main pip layer above.
# Install each wheel individually — skip platform-incompatible ones (e.g.
# pydantic_core x86_64 on aarch64 dev builds; already satisfied from above).
COPY vendor/agt-wheels/ /tmp/agt-wheels/
RUN for whl in /tmp/agt-wheels/*.whl; do \
      pip3 install --no-cache-dir --no-deps "$whl" 2>/dev/null || true; \
    done && rm -rf /tmp/agt-wheels

# Install GitHub CLI (enables github skill)
RUN GH_ARCH=$(case "${TARGETARCH:-$(uname -m)}" in arm64|aarch64) echo "arm64";; *) echo "amd64";; esac) && \
    curl -fsSL "https://github.com/cli/cli/releases/download/v2.88.1/gh_2.88.1_linux_${GH_ARCH}.tar.gz" -o /tmp/gh.tar.gz && \
    tar xzf /tmp/gh.tar.gz -C /usr/local --strip-components=1 && \
    rm /tmp/gh.tar.gz

# Install ripgrep (enables session-logs skill)
RUN RG_ARCH=$(case "${TARGETARCH:-$(uname -m)}" in arm64|aarch64) echo "aarch64-unknown-linux-gnu";; *) echo "x86_64-unknown-linux-musl";; esac) && \
    curl -fsSL "https://github.com/BurntSushi/ripgrep/releases/download/14.1.1/ripgrep-14.1.1-${RG_ARCH}.tar.gz" -o /tmp/rg.tar.gz && \
    tar xzf /tmp/rg.tar.gz -C /tmp && \
    cp /tmp/ripgrep-*/rg /usr/local/bin/ && \
    rm -rf /tmp/rg.tar.gz /tmp/ripgrep-*

# Install 1Password CLI (enables 1password skill)
RUN OP_ARCH=$(case "${TARGETARCH:-$(uname -m)}" in arm64|aarch64) echo "arm64";; *) echo "amd64";; esac) && \
    OP_VER="2.30.3" && \
    curl -fsSL "https://cache.agilebits.com/dist/1P/op2/pkg/v${OP_VER}/op_linux_${OP_ARCH}_v${OP_VER}.zip" -o /tmp/op.zip && \
    unzip -qo /tmp/op.zip -d /usr/local/bin/ && \
    rm /tmp/op.zip

# Install himalaya (enables himalaya email skill)
RUN HIM_ARCH=$(case "${TARGETARCH:-$(uname -m)}" in arm64|aarch64) echo "aarch64";; *) echo "x86_64";; esac) && \
    curl -fsSL "https://github.com/pimalaya/himalaya/releases/download/v1.2.0/himalaya.${HIM_ARCH}-linux.tgz" -o /tmp/himalaya.tgz && \
    tar xzf /tmp/himalaya.tgz -C /usr/local/bin/ && \
    rm /tmp/himalaya.tgz

# Copy Go CLI tools from go-builder (enables blucli, eightctl, gifgrep, etc.)
COPY --from=go-builder /go/bin/* /usr/local/bin/

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
RUN ln -sf /usr/local/lib/node_modules/openclaw/openclaw.mjs /usr/local/bin/openclaw && \
    for bin in clawhub mcporter oracle; do \
      [ -f "/usr/local/lib/node_modules/.bin/$bin" ] && ln -sf "/usr/local/lib/node_modules/.bin/$bin" "/usr/local/bin/$bin"; \
    done; true

# Copy pre-installed ClawHub skills from builder (installed via openclaw skills install)
COPY --from=builder /root/.openclaw/workspace/skills/ /opt/clawhub-skills/

# Proxy bootstrap — preloaded by NODE_OPTIONS to set undici global dispatcher
COPY sandbox-images/openclaw/proxy-bootstrap.js /usr/local/lib/proxy-bootstrap.js

# Copy inference router from pre-built image
COPY --from=router-bin /usr/local/bin/azureclaw-inference-router /usr/local/bin/azureclaw-inference-router

# Copy AzureClaw OpenClaw plugin (built from source in cli-builder stage)
COPY --from=cli-builder /build/cli/dist/ /opt/azureclaw-plugin/
COPY cli/package.json cli/openclaw.plugin.json /opt/azureclaw-plugin/
COPY cli/skills/ /opt/azureclaw-plugin/skills/
COPY cli/policies/ /opt/azureclaw-plugin/policies/


# Install plugin runtime dependencies
RUN cd /opt/azureclaw-plugin && npm install --omit=dev --ignore-scripts 2>/dev/null || true
# Install vendored SDK with its dependencies (libsodium-wrappers etc.)
COPY vendor/agentmesh-sdk/package.json vendor/agentmesh-sdk/tsup.config.ts /opt/azureclaw-vendored-sdk/
COPY vendor/agentmesh-sdk/src/ /opt/azureclaw-vendored-sdk/src/
COPY vendor/agentmesh-sdk/dist/ /opt/azureclaw-vendored-sdk/dist/
RUN cd /opt/azureclaw-vendored-sdk && npm install --ignore-scripts 2>/dev/null || true && \
    npm prune --omit=dev 2>/dev/null || true
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

# AGT governance sidecar server (dev mode — started by entrypoint.sh)
COPY sidecar-images/agt-governance/server.py /opt/agt-governance/server.py

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
