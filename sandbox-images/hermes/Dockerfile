# syntax=docker/dockerfile:1.7
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

#
# kars runtime image: Hermes Agent (Nous Research)
#
# Hermes is a Python 3.11+ agent harness with a built-in plugin system,
# 20+ messaging gateway adapters, 70+ tools, and 18+ inference provider
# integrations. Kars wraps Hermes with:
#   - the kars in-pod adapter (`runtimes/hermes/`) — a Hermes plugin that
#     registers kars_spawn, kars_handoff_*, foundry_*, http_fetch tools
#     and the AGT policy pre_tool_call hook;
#   - inference + Foundry routing through the inference-router sidecar on
#     localhost:8443 (provider base URLs pinned by entrypoint);
#   - kars governance — every tool call gates on /agt/evaluate before
#     execution, every inference call hits the router which applies
#     InferencePolicy / Content Safety / token budgets.
#
# Runtime isolation (documented in docs/runtimes/CONTRACT.md):
#   - Mesh tools use the shared Python AGT MeshClient installed below.
#   - Hermes' own terminal backends (docker, modal, daytona, ssh,
#     singularity) are DISABLED inside the kars pod — only `local` is
#     valid; the others would break out of K8s isolation.
#
# AGT-Python owner wheels (core, CLI and A2A) and native ACS come from
# `runtimes/build-agt-wheels.sh` and ride in `runtimes/wheels/`.
# Build them from the official source at `vendor/agt/pin.json`.
#
# Contract this image must honor (see docs/runtimes/CONTRACT.md v1):
#   - Runs as UID 1000 in AKS / local-k8s (controller pins this).
#   - LLM traffic via http://127.0.0.1:8443/v1 (router sidecar).
#   - Reads /etc/kars/secrets/admin-token at plugin init.
#   - Honors KARS_*, AGT_*, FOUNDRY_*, HERMES_* env wiring.
#   - Translates /etc/kars/mcp/<server>/meta.json → hermes mcp_servers
#     config before starting.
#   - Maps TELEGRAM_BOT_TOKEN / SLACK_BOT_TOKEN / DISCORD_BOT_TOKEN /
#     channel envs to `hermes config set channels.<x>.token=...`.
#
# Build context: repo root (so the COPY paths below resolve to
# runtimes/, sandbox-images/, etc.).

ARG HERMES_RUNTIME_BASE=base
FROM mcr.microsoft.com/azurelinux/base/python:3.12@sha256:d1a693353b7db0383e66a61416a049f617fdfb89435dcaac543749fb4f14950a AS base

LABEL org.kars.runtime.contract="v1"
LABEL org.kars.runtime.kind="Hermes"
LABEL org.kars.runtime.language="python"
LABEL org.opencontainers.image.title="kars Hermes Sandbox" \
      org.opencontainers.image.description="Hermes Agent runtime on kars, governed by AGT + Foundry" \
      org.opencontainers.image.vendor="Microsoft" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.source="https://github.com/Azure/kars"

# ---- System deps Hermes needs (minimal) ---------------------------------
# - git: hermes installs some optional skills via git clone
# - jq: entrypoint uses it to parse /etc/kars/mcp/*/meta.json
# - curl: already in base image; explicit listing keeps the intent clear
# - ca-certificates: TLS to PyPI / Hermes' optional skill tap repos
# - nodejs / npm: Hermes' built-in dep_ensure unconditionally probes for
#   node/npm via shutil.which() for the TUI + browser-tools paths
#   (`hermes_cli/dep_ensure.py::_DEP_CHECKS["node"]`). When absent inside
#   a kars sandbox, it tries to fetch and run the upstream
#   `scripts/install.sh` which downloads Node 22 from nodejs.org — that
#   download stalls behind the egress-guard's HTTP-CONNECT proxy at UID
#   1000 and blocks every `hermes -z` invocation for 60+ seconds before
#   ultimately failing. Pre-installing the system nodejs makes the
#   check pass and skips the install hook entirely.
#
# Note: Hermes' built-in `file_search` tool prefers ripgrep but falls back
# to grep when absent. Azure Linux 3 tdnf doesn't ship ripgrep; we skip
# the optional dep rather than pulling cargo just to build it.
RUN tdnf update -y --refresh \
    && tdnf install -y \
        git jq ca-certificates nodejs nodejs-npm tar xz \
    && tdnf clean all

# ---- Pin Node 22 for the Hermes TUI ------------------------------------
# Azure Linux 3 ships Node 24, but the Hermes ui-tui bundle ships a
# pre-built JS that crashes (SIGSEGV, ~380MB core dump) on Node 24 —
# its esbuild pre-build target is Node 22. The TUI is what backs the
# dashboard's in-browser "Chat" tab (and `hermes chat --tui` on the
# CLI), so a SIGSEGV here = the web chat renders, opens its WS, then
# the spawned TUI child dies silently and the operator can't type.
#
# Install only the vendor Node 22 executable and license, not a second npm
# dependency tree. HERMES_NODE selects this executable for the TUI; the
# system Node/npm remain on PATH for dep_ensure and browser tools.
ARG NODE22_VERSION=22.23.3
COPY sandbox-images/hermes/node22-SHASUMS256.txt /opt/node22-SHASUMS256.txt
RUN ARCH="$(uname -m | sed 's/aarch64/arm64/;s/x86_64/x64/')" \
    && NODE_DIR="node-v${NODE22_VERSION}-linux-${ARCH}" \
    && mkdir -p /opt/node22 \
    && cd /opt/node22 \
    && curl -fsSL -o "${NODE_DIR}.tar.xz" \
       "https://nodejs.org/dist/v${NODE22_VERSION}/${NODE_DIR}.tar.xz" \
    && sha256sum --check --ignore-missing /opt/node22-SHASUMS256.txt \
    && tar -xJf "${NODE_DIR}.tar.xz" --strip-components=1 \
       "${NODE_DIR}/bin/node" "${NODE_DIR}/LICENSE" \
    && rm -f "${NODE_DIR}.tar.xz" \
    && /opt/node22/bin/node --version

# ---- Install the complete pinned AGT-Python wheel set -------------------
# Fail before pip can substitute a PyPI release for a missing AGT package.
COPY sandbox-images/hermes/hermes-dependency-fixes.patch \
     sandbox-images/hermes/hermes-uv.lock \
     sandbox-images/hermes/hermes-core-constraints.txt \
     sandbox-images/hermes/hermes-source-SHA256SUMS /opt/hermes-build/
RUN cd /opt/hermes-build && sha256sum --strict --check hermes-source-SHA256SUMS
COPY runtimes/wheels/ /tmp/agt-wheels/
COPY runtimes/install-agt-wheels.py /tmp/install-agt-wheels.py
RUN python3 /tmp/install-agt-wheels.py --constraint /opt/hermes-build/hermes-core-constraints.txt

# ---- Install Hermes Agent ------------------------------------------------
# Preserve genuine editable project metadata and the qualified security fixes.
ARG HERMES_COMMIT=f97608f178d1ffeca59860195ab7da295f7c8e5f
RUN git init -q /opt/hermes \
    && git -C /opt/hermes fetch --depth=1 https://github.com/NousResearch/hermes-agent.git "${HERMES_COMMIT}" \
    && git -C /opt/hermes checkout --detach -q FETCH_HEAD \
    && test "$(git -C /opt/hermes rev-parse HEAD)" = "${HERMES_COMMIT}" \
    && test "$(git -C /opt/hermes hash-object pyproject.toml)" = "1a98d5528c88ddc3d4e32439d3e8322487ce31bc" \
    && git -C /opt/hermes apply /opt/hermes-build/hermes-dependency-fixes.patch \
    && cp /opt/hermes-build/hermes-uv.lock /opt/hermes/uv.lock \
    && echo "2c87202b1566e4daed16b1472bc5d5e3651af9f6fd64acc953412f43432fefb0  /opt/hermes/pyproject.toml" | sha256sum --check - \
    && printf '%s\n' \
       "6dd85804ac28a0f2d326030e69c4f65a9ae8968478af87d2267d6ad89cc7230a  /opt/hermes/package.json" \
       "2eee7bf49d29e6d0b218dcc960a174b99cc177039f6941f0355ebff9a93e33f7  /opt/hermes/package-lock.json" \
       "14c2e288f07d4b6290946c1d0031fce51d7ab466e11225c8acbcd4331fc7b846  /opt/hermes/ui-tui/package.json" \
       | sha256sum --check - \
    && pip install --no-cache-dir --constraint /opt/hermes-build/hermes-core-constraints.txt --editable /opt/hermes \
    && python3 -c "import importlib.metadata as m, json; d=m.distribution('hermes-agent'); assert d.version == '0.21.5'; u=json.loads(d.read_text('direct_url.json')); assert u['dir_info']['editable'] and u['url'] == 'file:///opt/hermes'"

# Git source does not carry release-wheel UI bundles. Build both now and select
# Hermes' supported prebuilt TUI path, rather than installing on first chat.
RUN cd /opt/hermes \
    && npm ci --ignore-scripts --include=dev --workspace ui-tui --workspace web --include-workspace-root --no-audit --no-fund \
    && (cd ui-tui && node -e "require('node:assert/strict').equal(require('undici/package.json').version, '6.28.1')") \
    && npm run build --workspace ui-tui \
    && npm run build --workspace web \
    && test -s ui-tui/dist/entry.js \
    && test -s hermes_cli/web_dist/index.html \
    && rm -rf /opt/hermes/node_modules /opt/hermes/ui-tui/node_modules /opt/hermes/web/node_modules
ENV HERMES_TUI_DIR=/opt/hermes/ui-tui

# ---- Channel adapter libraries -----------------------------------------
# Hermes auto-detects channels (Telegram / Slack / Discord) from env
# vars (TELEGRAM_BOT_TOKEN, SLACK_BOT_TOKEN, DISCORD_BOT_TOKEN) and
# tries to instantiate an adapter per channel. Each adapter is a
# soft-optional dep — Hermes itself doesn't pull them — so we install
# them here so the kars runtime image is "channels work out of the box"
# when a credentials secret carries the token. Pinned to the
# adapter-stable major:
#   - python-telegram-bot 21.x   (Bot API 7.x, async-first)
#   - slack-bolt 1.30 + slack-sdk 3.x (async Web + Socket Mode)
#   - discord.py 2.x             (gateway client)
# Bumping these requires re-verifying the Hermes channel adapters.
RUN pip install --no-cache-dir --constraint /opt/hermes-build/hermes-core-constraints.txt \
    "python-telegram-bot>=21,<22" \
    "slack-bolt==1.30.0" \
    "slack-sdk>=3,<4" \
    "aiohttp>=3.14.3,<4" \
    "discord.py>=2,<3" \
    && python3 -c "from slack_bolt.async_app import AsyncApp; from slack_bolt.adapter.socket_mode.async_handler import AsyncSocketModeHandler; from slack_sdk.web.async_client import AsyncWebClient"

# ---- Hermes dashboard web UI deps ---------------------------------------
# `hermes dashboard` (the in-browser PTY chat the Headlamp SRE Console
# embeds) needs FastAPI + Uvicorn + WebSockets + Jinja2 to start. These
# are soft-optional in hermes-agent itself, so we pull them here so the
# dashboard is "Just Works" inside every kars sandbox without an
# operator having to pip-install at runtime. Pins follow Hermes 0.15.x
# upstream's tested matrix, with the shared mesh transport supporting
# WebSockets 12 through 15 for the Hermes refresh.
RUN pip install --no-cache-dir --constraint /opt/hermes-build/hermes-core-constraints.txt \
    "fastapi>=0.110,<1" \
    "uvicorn[standard]>=0.30,<1" \
    "websockets>=12,<16" \
    "jinja2>=3.1,<4" \
    "python-multipart>=0.0.9,<1" \
    "ptyprocess>=0.7,<1"

# ---- Install kars-agt-mesh (Python AGT MeshClient, Act 2.1) ------------
# The mesh client is a separate package so non-Hermes Python runtimes
# (pydantic-ai, anthropic, langgraph, …) can reuse the same TS-SDK-parity
# E2E encrypted transport. The Hermes plugin's `mesh.py` adapter wraps
# it. Install BEFORE the plugin stage so the package is importable when
# Hermes' plugin discovery imports `kars_runtime_hermes.plugin.mesh`.
COPY runtimes/agt-mesh-python/ /opt/kars-agt-mesh/
RUN pip install --no-cache-dir --constraint /opt/hermes-build/hermes-core-constraints.txt /opt/kars-agt-mesh \
    && pip check

# ---- Router-readable config dirs ----------------------------------------
# Mirrors the openclaw image. The inference-router sidecar (UID 1001 in
# AKS, root in docker dev) reads these. /tmp is the writable tmpfs.
RUN mkdir -p /etc/kars/policies /etc/kars/blocklist /etc/kars/mcp \
    && chmod 755 /etc/kars /etc/kars/policies /etc/kars/blocklist /etc/kars/mcp

# ---- Workspace ----------------------------------------------------------
# Where the controller mounts user-supplied agent code via
# `spec.runtime.hermes.agentCode: { oci | git }`. Hermes' own working
# directory is HERMES_HOME=/sandbox/.hermes (emptyDir).
RUN mkdir -p /sandbox/agent /sandbox/.hermes \
    && chown -R 1000:1000 /sandbox

# ---- Default agent (for smoke testing) ----------------------------------
COPY --chown=1000:1000 sandbox-images/hermes/default-agent/ /opt/kars-default-agent/

# ---- Security: drop to UID 1000 (matches AKS pod spec) ------------------
# In docker dev mode, the entrypoint may run as root briefly to set up
# iptables (egress guard) and then drops to UID 1000 via runuser. In
# AKS, the pod spec sets runAsUser:1000 directly so the entrypoint is
# already UID 1000 from PID 1.
USER 1000
WORKDIR /sandbox/agent

# ---- Healthcheck --------------------------------------------------------
# Hermes' TUI mode doesn't expose an HTTP endpoint; the gateway mode does
# bind a control socket at $HERMES_HOME/gateway.sock. For container
# health, we just check the python process responds (Hermes self-installs
# a `hermes` CLI). K8s uses the deployment's readiness/liveness probes.
HEALTHCHECK --interval=30s --timeout=3s --start-period=30s --retries=3 \
    CMD hermes --version >/dev/null 2>&1 || exit 1

ENTRYPOINT ["/usr/local/bin/kars-hermes-entrypoint.sh"]
CMD ["hermes"]

# Pure-Python wheel construction is separate from the offline runtime install.
# Qualified adapter-only refreshes may supply this stage as a named context.
FROM base AS adapter-build
USER 0
COPY runtimes/hermes/pyproject.toml runtimes/hermes/README.md /opt/kars-runtime-hermes/
COPY runtimes/hermes/src/ /opt/kars-runtime-hermes/src/
RUN pip wheel --no-cache-dir --no-deps --wheel-dir /opt/kars-adapter-wheels /opt/kars-runtime-hermes

FROM ${HERMES_RUNTIME_BASE} AS runtime
USER 0
# Remove obsolete source and discovered-plugin files when reusing a qualified image.
RUN rm -rf /opt/kars-runtime-hermes /opt/kars-hermes-stage/plugins/kars
COPY runtimes/hermes/pyproject.toml runtimes/hermes/README.md /opt/kars-runtime-hermes/
COPY runtimes/hermes/src/ /opt/kars-runtime-hermes/src/
COPY --from=adapter-build /opt/kars-adapter-wheels/ /tmp/kars-adapter-wheels/
RUN pip install --no-cache-dir --no-index --no-deps --force-reinstall \
        --constraint /opt/hermes-build/hermes-core-constraints.txt /tmp/kars-adapter-wheels/*.whl \
    && pip check \
    && rm -rf /tmp/kars-adapter-wheels
COPY runtimes/hermes/src/kars_runtime_hermes/plugin/ /opt/kars-hermes-stage/plugins/kars/
RUN chmod -R a+rX /opt/kars-hermes-stage
COPY sandbox-images/hermes/entrypoint.sh /usr/local/bin/kars-hermes-entrypoint.sh
RUN chmod 0755 /usr/local/bin/kars-hermes-entrypoint.sh
USER 1000
