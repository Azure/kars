# syntax=docker/dockerfile:1.7
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
# Act 1 limitations (documented in docs/runtimes/CONTRACT.md):
#   - Mesh tools (kars_mesh_send, _inbox, _await, _transfer_file) are
#     non-functional stubs. Act 2 ships a Python AGT MeshClient at
#     parity with the TypeScript SDK, after which mesh is live.
#   - Hermes' own terminal backends (docker, modal, daytona, ssh,
#     singularity) are DISABLED inside the kars pod — only `local` is
#     valid; the others would break out of K8s isolation.
#
# AGT-Python wheels (`a2a_agentmesh`, `agent_sandbox`) come from
# `runtimes/build-agt-wheels.sh` and ride in `runtimes/wheels/`. Same
# pattern as the pydantic-ai / langgraph / anthropic adapters.
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

FROM mcr.microsoft.com/azurelinux/base/python:3.12@sha256:485299b016fe5ae745ffee27f0b8a850576841205ed1d420c9a84b126198e320 AS base

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
RUN tdnf install -y --refresh \
        git jq ca-certificates nodejs nodejs-npm \
    && tdnf clean all

# ---- Install AGT-Python wheels (governance primitives only in Act 1) ----
# The wheels directory rides in the build context via
# runtimes/build-agt-wheels.sh. If empty (rare — only happens when an
# operator builds without first running the wheel script), pip install
# becomes a no-op and the Python AGT modules fall back to whatever pip
# can resolve at runtime.
COPY runtimes/wheels/ /tmp/agt-wheels/
RUN if ls /tmp/agt-wheels/*.whl >/dev/null 2>&1; then \
        pip install --no-cache-dir /tmp/agt-wheels/*.whl ; \
    fi

# ---- Install Hermes Agent ------------------------------------------------
# Pinned to a specific release tag for build reproducibility. Operators
# bumping to a newer Hermes should also re-verify the kars runtime
# contract is still honored (entrypoint env shape + plugin context API).
ARG HERMES_VERSION=0.15.2
RUN pip install --no-cache-dir "hermes-agent==${HERMES_VERSION}"

# ---- Install the kars-runtime-hermes plugin -----------------------------
# This is the in-pod adapter that registers kars_spawn, foundry_*,
# governance pre_tool_call hook, channel translation, etc.
COPY runtimes/hermes/ /opt/kars-runtime-hermes/
RUN pip install --no-cache-dir /opt/kars-runtime-hermes

# ---- Install kars-agt-mesh (Python AGT MeshClient, Act 2.1) ------------
# The mesh client is a separate package so non-Hermes Python runtimes
# (pydantic-ai, anthropic, langgraph, …) can reuse the same TS-SDK-parity
# E2E encrypted transport. The Hermes plugin's `mesh.py` adapter wraps
# it. Install BEFORE the plugin stage so the package is importable when
# Hermes' plugin discovery imports `kars_runtime_hermes.plugin.mesh`.
COPY runtimes/agt-mesh-python/ /opt/kars-agt-mesh/
RUN pip install --no-cache-dir /opt/kars-agt-mesh

# ---- Pre-stage Hermes plugin into ~/.hermes/plugins/kars/ ---------------
# Hermes discovers plugins from $HERMES_HOME/plugins/ at import time.
# We pre-stage the kars plugin into the image so the entrypoint doesn't
# need to materialize it at every container start. HERMES_HOME defaults
# to /sandbox/.hermes (writable emptyDir mount in k8s); the entrypoint
# copies this staged tree there on first boot.
RUN mkdir -p /opt/kars-hermes-stage/plugins/kars
COPY runtimes/hermes/src/kars_runtime_hermes/plugin/ /opt/kars-hermes-stage/plugins/kars/
RUN chmod -R a+rX /opt/kars-hermes-stage

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

# ---- Entrypoint ---------------------------------------------------------
COPY sandbox-images/hermes/entrypoint.sh /usr/local/bin/kars-hermes-entrypoint.sh
RUN chmod 0755 /usr/local/bin/kars-hermes-entrypoint.sh

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
