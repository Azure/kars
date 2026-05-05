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

# ─── Runtime Adapter Builder (TypeScript → JavaScript) ───────────────────────
# Builds runtimes/openclaw/ — the AzureClaw runtime adapter for OpenClaw,
# loaded as a non-bundled OpenClaw plugin inside the sandbox pod.
# Uses the base image (has Node.js) to avoid a separate Node.js download.

FROM ${SANDBOX_BASE_IMAGE} AS cli-builder

WORKDIR /build
COPY runtimes/openclaw/package.json runtimes/openclaw/package-lock.json runtimes/openclaw/tsconfig.json ./runtimes/openclaw/
COPY runtimes/openclaw/src/ ./runtimes/openclaw/src/
RUN cd runtimes/openclaw && npm ci --ignore-scripts && npm run build

# ─── Runtime Stage (overlay on base) ─────────────────────────────────────────

FROM ${SANDBOX_BASE_IMAGE}

# Proxy bootstrap — preloaded by NODE_OPTIONS to set undici global dispatcher
COPY sandbox-images/openclaw/proxy-bootstrap.js /usr/local/lib/proxy-bootstrap.js

# Copy inference router from pre-built image
COPY --from=router-bin /usr/local/bin/azureclaw-inference-router /usr/local/bin/azureclaw-inference-router

# Copy AzureClaw OpenClaw runtime adapter (built from source in cli-builder stage)
COPY --from=cli-builder /build/runtimes/openclaw/dist/ /opt/azureclaw-plugin/
COPY runtimes/openclaw/package.json runtimes/openclaw/package-lock.json runtimes/openclaw/openclaw.plugin.json /opt/azureclaw-plugin/
COPY runtimes/openclaw/skills/ /opt/azureclaw-plugin/skills/
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

# Build a pruned dist-runtime/extensions/ tree mirroring dist/, but containing
# only the 3 always-required runtime cores. Files are hardlinked (`cp -al`)
# from dist/, so disk overhead is ~0.
#
# Why: OpenClaw's facade loader rejects symlinks whose realpath escapes the
# bundled-plugins root (Symlink escapes bundled plugin directory). Setting
# OPENCLAW_DISABLE_BUNDLED_PLUGINS=1 short-circuits the override and points
# at an empty tmp dir, breaking speech-core/image-generation-core/
# media-understanding-core resolution. By instead pointing
# OPENCLAW_BUNDLED_PLUGINS_DIR at this pruned tree (which is under
# <packageRoot>/dist-runtime/extensions and so passes resolveTrustedExistingOverride),
# we get:
#   - manifest discovery scans only 3 plugins → no 50s pi-ai catalog wedge
#   - facade loader's openBoundaryFileSync sees realpaths under dist-runtime/ → ok
#   - hardlinked top-level chunks (redact-*.js, etc.) sit at dist-runtime/
#     so the cores' relative `require("../../redact-*")` resolves correctly
RUN OC=/usr/local/lib/node_modules/openclaw && \
    if [ -d "$OC/dist" ] && [ ! -d "$OC/dist-runtime" ]; then \
      cp -al "$OC/dist" "$OC/dist-runtime" && \
      find "$OC/dist-runtime/extensions" -mindepth 1 -maxdepth 1 -type d \
        ! -name speech-core ! -name image-generation-core ! -name media-understanding-core \
        -exec rm -rf {} + && \
      echo "Built dist-runtime with only 3 cores: $(ls $OC/dist-runtime/extensions)"; \
    fi

# Pre-create router-readable config dirs (root-owned, world-traversable).
# /etc/azureclaw/policies/  — AGT governance policy YAMLs (populated by entrypoint)
# /etc/azureclaw/blocklist/ — egress blocklist (populated by entrypoint)
# These live OUTSIDE OpenClaw's data dir so the inference router (UID 1001)
# can read them even after OpenClaw 2026.4.x re-locks ~/.openclaw/ to 0700.
RUN mkdir -p /etc/azureclaw/policies /etc/azureclaw/blocklist && \
    chmod 755 /etc/azureclaw /etc/azureclaw/policies /etc/azureclaw/blocklist

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
