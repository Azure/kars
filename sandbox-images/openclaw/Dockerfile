# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# kars OpenClaw Sandbox Image
# Slim overlay on top of kars-sandbox-base — adds only the router binary,
# kars plugin, pinned official AGT SDK, and entrypoint.
#
# Base image (Dockerfile.base) contains: OS, Node.js, Python, Go tools, OpenClaw.
# This image adds: router binary, current-source plugin code, SDK, entrypoint.
#
# Dev:  docker build -f sandbox-images/openclaw/Dockerfile .
# Prod: docker build --platform linux/amd64 -f sandbox-images/openclaw/Dockerfile .
#
# Expects base image to be pre-built:
#   Dev:  kars-sandbox-base:dev
#   Prod: ${ACR}/kars-sandbox-base:latest

ARG SANDBOX_BASE_IMAGE=kars-sandbox-base:dev
ARG INFERENCE_ROUTER_IMAGE=karsacr.azurecr.io/kars-inference-router:latest

# ─── Use pre-built inference router image ────────────────────────────────────

FROM ${INFERENCE_ROUTER_IMAGE} AS router-bin

# ─── Runtime Adapter Builder (TypeScript → JavaScript) ───────────────────────
# Builds runtimes/openclaw/ — the kars runtime adapter for OpenClaw,
# loaded as a non-bundled OpenClaw plugin inside the sandbox pod.
# Uses the base image (has Node.js) to avoid a separate Node.js download.

FROM ${SANDBOX_BASE_IMAGE} AS cli-builder

WORKDIR /build
# Preserve file: dependency paths for both consumers of the same pinned SDK.
COPY vendor/agt/*.tgz vendor/agt/SHA256SUMS ./vendor/agt/
COPY mesh-plugin/package.json mesh-plugin/package-lock.json mesh-plugin/tsconfig.json mesh-plugin/openclaw.plugin.json ./mesh-plugin/
COPY mesh-plugin/src/ ./mesh-plugin/src/
COPY mesh-plugin/skills/ ./mesh-plugin/skills/
COPY runtimes/openclaw/package.json runtimes/openclaw/package-lock.json runtimes/openclaw/tsconfig.json ./runtimes/openclaw/
COPY runtimes/openclaw/src/ ./runtimes/openclaw/src/

# Older callers may stage the canonical tarball; never replace the locked SDK.
ARG MESH_PROVIDER=agt
ARG AGT_SDK_TARBALL=
COPY .agt-sdk/ ./legacy-agt-sdk/
RUN test "$MESH_PROVIDER" = agt && \
    cd /build/vendor/agt && sha256sum -c SHA256SUMS && \
    if [ -n "$AGT_SDK_TARBALL" ]; then \
      test "$AGT_SDK_TARBALL" = "$(basename "$AGT_SDK_TARBALL")" && \
      cmp "/build/legacy-agt-sdk/$AGT_SDK_TARBALL" "/build/vendor/agt/$AGT_SDK_TARBALL"; \
    fi

# Build both adapters from source, then retain only their production dependency
# graphs. IGNORE_SCRIPTS also propagates to npm's nested file: dependency work.
RUN --mount=type=secret,id=npmrc,target=/root/.npmrc \
    export NPM_CONFIG_IGNORE_SCRIPTS=true && \
    cd /build/mesh-plugin && npm ci --ignore-scripts && npm run build && \
    cd /build/runtimes/openclaw && npm ci --ignore-scripts && npm run build && \
    npm prune --omit=dev --ignore-scripts && \
    cd /build/mesh-plugin && npm prune --omit=dev --ignore-scripts

# ─── Runtime Stage (overlay on base) ─────────────────────────────────────────

FROM ${SANDBOX_BASE_IMAGE}

# Proxy bootstrap — preloaded by NODE_OPTIONS to set undici global dispatcher
COPY sandbox-images/openclaw/proxy-bootstrap.js /usr/local/lib/proxy-bootstrap.js

# Copy inference router from pre-built image
COPY --from=router-bin /usr/local/bin/kars-inference-router /usr/local/bin/kars-inference-router

# Copy kars OpenClaw runtime adapter (built from source in cli-builder stage)
COPY --from=cli-builder /build/runtimes/openclaw/dist/ /opt/kars-plugin/
COPY runtimes/openclaw/package.json runtimes/openclaw/package-lock.json runtimes/openclaw/openclaw.plugin.json /opt/kars-plugin/
COPY runtimes/openclaw/skills/ /opt/kars-plugin/skills/

# The runtime's @kars/mesh symlink resolves to /mesh-plugin. Its own dependency
# tree must travel with it; sibling runtime node_modules cannot satisfy imports.
COPY --from=cli-builder /build/mesh-plugin/package.json /mesh-plugin/package.json
COPY --from=cli-builder /build/mesh-plugin/dist/ /mesh-plugin/dist/
COPY --from=cli-builder /build/mesh-plugin/node_modules/ /mesh-plugin/node_modules/
COPY --from=cli-builder /build/runtimes/openclaw/node_modules/ /opt/kars-plugin/node_modules/

# Harden image-level code: root-owned, read-only — prevents agent from tampering
# with the canonical copy that gets re-installed on every container start.
RUN chmod -R a-w /opt/kars-plugin /mesh-plugin && \
    chmod -R a+rX /opt/kars-plugin /mesh-plugin

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
# We keep:
#   - 3 always-required runtime cores (speech-core, image-generation-core,
#     media-understanding-core) — without these OpenClaw fails to boot.
#   - Channel plugins (telegram, slack, discord, whatsapp, imessage) — these
#     are referenced from openclaw.json's channels.* / plugins.allow blocks
#     by the entrypoint when a credential env var is set. Without them
#     present in the bundled-plugins root, the gateway logs
#     "channels.telegram: unknown channel id" and silently drops the channel.
#   - Third-party tool plugins matching the entrypoint's auto-enable list
#     (brave, tavily, exa, firecrawl, perplexity, openai) — same stale-config
#     pitfall as channels: they're written into plugins.allow when the
#     corresponding API-key env var is present.
#   - Generic-utility plugins that don't depend on a model provider:
#       browser            — headless browse / Playwright tool
#       duckduckgo         — no-API-key web search fallback
#       webhooks           — generic inbound webhook channel
#       file-transfer      — AGT mesh file transfer
#       document-extract   — PDF / Office extract (pairs with foundry_file_search)
#       web-readability    — strip chrome/ads from fetched pages
#       diffs              — code-diff renderer
#
# Hardlinks (cp -al) keep the disk overhead negligible. We deliberately
# do NOT keep pi-ai model-provider plugins (anthropic, openai-provider
# mode, xai, mistral, groq, deepseek, ollama, bedrock, openrouter,
# litellm, codex, copilot-proxy, github-copilot, vercel-ai-gateway, etc.)
# because each one re-enables a slice of the pi-ai catalog enumeration
# that wedges models.list for ~50s on first WebUI connect; kars
# routes all model traffic through the inference router instead.
RUN OC=/usr/local/lib/node_modules/openclaw && \
    if [ -d "$OC/dist" ] && [ ! -d "$OC/dist-runtime" ]; then \
      cp -al "$OC/dist" "$OC/dist-runtime" && \
      find "$OC/dist-runtime/extensions" -mindepth 1 -maxdepth 1 -type d \
        ! -name speech-core ! -name image-generation-core ! -name media-understanding-core \
        ! -name telegram ! -name slack ! -name discord ! -name whatsapp ! -name imessage \
        ! -name brave ! -name tavily ! -name exa ! -name firecrawl ! -name perplexity ! -name openai \
        ! -name browser ! -name duckduckgo ! -name webhooks ! -name file-transfer \
        ! -name document-extract ! -name web-readability ! -name diffs \
        -exec rm -rf {} + && \
      KEPT=$(find "$OC/dist-runtime/extensions" -mindepth 1 -maxdepth 1 -type d -printf '%f ' 2>/dev/null) && \
      KEPT_COUNT=$(printf '%s\n' "$KEPT" | wc -w) && \
      echo "Built dist-runtime ($KEPT_COUNT plugins kept): $KEPT"; \
    fi

# Pre-create router-readable config dirs (root-owned, world-traversable).
# /etc/kars/policies/  — AGT governance policy YAMLs (populated by entrypoint)
# /etc/kars/blocklist/ — egress blocklist (populated by entrypoint)
# These live OUTSIDE OpenClaw's data dir so the inference router (UID 1001)
# can read them even after OpenClaw 2026.4.x re-locks ~/.openclaw/ to 0700.
RUN mkdir -p /etc/kars/policies /etc/kars/blocklist && \
    chmod 755 /etc/kars /etc/kars/policies /etc/kars/blocklist

# Copy entrypoint that auto-configures OpenClaw from mounted secrets
COPY sandbox-images/openclaw/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

# Labels
LABEL org.opencontainers.image.title="kars OpenClaw Sandbox" \
      org.opencontainers.image.description="Hardened OpenClaw sandbox on Azure Linux 3" \
      org.opencontainers.image.vendor="Microsoft" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.source="https://github.com/Azure/kars" \
      org.opencontainers.image.base.name="azurelinux/base/core:3.0"

# HEALTHCHECK: probes the OpenClaw gateway on its in-pod port. The
# inference-router sidecar has its own HEALTHCHECK in its base image.
# This applies to `docker compose` / `docker ps` operators; K8s uses
# the readiness/liveness probes from the Deployment instead.
HEALTHCHECK --interval=30s --timeout=3s --start-period=30s --retries=3 \
  CMD curl -fsS --max-time 2 http://127.0.0.1:18789/ > /dev/null || exit 1

# Entrypoint runs as root to set up iptables and start services under correct UIDs:
#   UID 1001 (router)  — can reach the internet (Foundry API, blocklist feeds)
#   UID 1000 (sandbox) — restricted to localhost + DNS by iptables egress guard
# This mirrors AKS where router is a separate container in the pod.
WORKDIR /sandbox

# Entrypoint auto-configures OpenClaw, then runs the command
ENTRYPOINT ["entrypoint.sh"]
CMD ["sleep", "infinity"]
