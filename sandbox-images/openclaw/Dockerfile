# Kars OpenClaw Sandbox Image
# Slim overlay on top of kars-sandbox-base — adds only the router binary,
# Kars plugin, vendored SDK, and entrypoint.
#
# Base image (Dockerfile.base) contains: OS, Node.js, Python, Go tools, OpenClaw.
# This image adds: router binary, plugin code, SDK patches, entrypoint.
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
# Builds runtimes/openclaw/ — the Kars runtime adapter for OpenClaw,
# loaded as a non-bundled OpenClaw plugin inside the sandbox pod.
# Uses the base image (has Node.js) to avoid a separate Node.js download.

FROM ${SANDBOX_BASE_IMAGE} AS cli-builder

WORKDIR /build
# mesh-plugin is a file: dep of runtimes/openclaw (npm resolves
# `@kars/mesh` to `../../mesh-plugin`). It must be present at
# /build/mesh-plugin BEFORE `npm ci` runs, or tsc fails to resolve
# the import. We copy the pre-built dist so npm doesn't need to run
# mesh-plugin's `prepare` script (which needs tsc as a devDep).
COPY mesh-plugin/package.json mesh-plugin/package-lock.json ./mesh-plugin/
COPY mesh-plugin/dist/ ./mesh-plugin/dist/
COPY runtimes/openclaw/package.json runtimes/openclaw/package-lock.json runtimes/openclaw/tsconfig.json ./runtimes/openclaw/
COPY runtimes/openclaw/src/ ./runtimes/openclaw/src/
# NPM_CONFIG_IGNORE_SCRIPTS=true propagates to nested installs of
# file: deps; --ignore-scripts on the outer command alone does not.
# Belt-and-braces: also strip mesh-plugin's `prepare` script, which
# would otherwise try to invoke tsc (missing in this stage).
RUN node -e "const f='./mesh-plugin/package.json'; const p=require(f); delete p.scripts.prepare; require('fs').writeFileSync(f, JSON.stringify(p,null,2));" && \
    cd runtimes/openclaw && \
    NPM_CONFIG_IGNORE_SCRIPTS=true npm ci --ignore-scripts && \
    npm run build

# ─── Runtime Stage (overlay on base) ─────────────────────────────────────────

FROM ${SANDBOX_BASE_IMAGE}

# Proxy bootstrap — preloaded by NODE_OPTIONS to set undici global dispatcher
COPY sandbox-images/openclaw/proxy-bootstrap.js /usr/local/lib/proxy-bootstrap.js

# Copy inference router from pre-built image
COPY --from=router-bin /usr/local/bin/kars-inference-router /usr/local/bin/kars-inference-router

# Copy Kars OpenClaw runtime adapter (built from source in cli-builder stage)
COPY --from=cli-builder /build/runtimes/openclaw/dist/ /opt/kars-plugin/
COPY runtimes/openclaw/package.json runtimes/openclaw/package-lock.json runtimes/openclaw/openclaw.plugin.json /opt/kars-plugin/
COPY runtimes/openclaw/skills/ /opt/kars-plugin/skills/

# mesh-plugin is referenced by the runtime as a `file:../../mesh-plugin` dep.
# `npm ci` below creates a symlink at
# /opt/kars-plugin/node_modules/@kars/mesh → ../../../../mesh-plugin
# which resolves to /mesh-plugin. Stage the prebuilt mesh-plugin there so the
# symlink is not dangling at runtime. Without this the runtime falls back to
# the vendored SDK silently (`mesh provider swap failed, staying on vendored`).
COPY mesh-plugin/package.json /mesh-plugin/package.json
COPY mesh-plugin/dist/ /mesh-plugin/dist/
RUN node -e "const f='/mesh-plugin/package.json'; const p=require(f); delete p.scripts.prepare; require('fs').writeFileSync(f, JSON.stringify(p,null,2));"

# Install plugin runtime dependencies
RUN cd /opt/kars-plugin && npm ci --omit=dev --ignore-scripts

# Optional: install upstream Microsoft AGT SDK when MESH_PROVIDER=agt build arg
# is passed. Not installed by default so vendored builds stay slim and offline.
# AGT_SDK_TARBALL: when set (dev mode pinning a locally-patched AGT branch) the
# build context must contain `.agt-sdk/<tarball-name>`. The CLI stages this for
# `kars dev --mesh-provider agt --build`. The directory always exists with
# at least a .keep file so the COPY never fails.
ARG MESH_PROVIDER=vendored
ARG AGT_SDK_TARBALL=
COPY .agt-sdk/ /opt/kars-agt-sdk/
RUN cd /opt/kars-plugin && \
    if [ -n "$AGT_SDK_TARBALL" ] && [ -f "/opt/kars-agt-sdk/$AGT_SDK_TARBALL" ]; then \
      echo "Installing AGT SDK from local tarball: $AGT_SDK_TARBALL" && \
      npm install --omit=dev --ignore-scripts --no-save \
        "/opt/kars-agt-sdk/$AGT_SDK_TARBALL" ; \
    else \
      echo "Installing AGT SDK from npm: @microsoft/agent-governance-sdk@^3.5.0" && \
      npm install --omit=dev --ignore-scripts --no-save \
        '@microsoft/agent-governance-sdk@^3.5.0' ; \
    fi

# Harden image-level code: root-owned, read-only — prevents agent from tampering
# with the master copy that gets re-installed on every container start.
RUN chmod -R a-w /opt/kars-plugin && \
    chmod -R a+rX /opt/kars-plugin

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
# that wedges models.list for ~50s on first WebUI connect; Kars
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
LABEL org.opencontainers.image.title="Kars OpenClaw Sandbox" \
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
