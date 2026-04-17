#!/usr/bin/env bash
# patch-nemoclaw.sh — bolt the azureclaw-mesh plugin into a NemoClaw source tree.
#
# Fresh Azure/azureclaw clone + fresh NemoClaw clone:
#
#   cd azureclaw/mesh-plugin
#   npm run build
#   NEMOCLAW_PATH=~/.nemoclaw/source ./scripts/patch-nemoclaw.sh
#   cd "$NEMOCLAW_PATH" && docker build -t nemoclaw:mesh .
#
# All edits are idempotent — safe to re-run after `git pull` on either repo.
#
# Effects on the target NemoClaw tree:
#   1. Copy mesh-plugin/dist + vendored @agentmesh/sdk into
#      <nemoclaw>/scripts/azureclaw-mesh/
#   2. Patch Dockerfile: add COPY line for the plugin (after openclaw plugins install)
#   3. Patch dist/lib/sandbox-build-context.js: stage azureclaw-mesh into build ctx

set -euo pipefail

NEMOCLAW_PATH="${NEMOCLAW_PATH:-${HOME}/.nemoclaw/source}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLUGIN_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$PLUGIN_DIR/.." && pwd)"

log()  { printf '\033[0;36m[patch-nemoclaw]\033[0m %s\n' "$*"; }
die()  { printf '\033[0;31m[patch-nemoclaw]\033[0m %s\n' "$*" >&2; exit 1; }

# ── Preconditions ───────────────────────────────────────────────────────────
[ -d "$NEMOCLAW_PATH" ]                     || die "NEMOCLAW_PATH not found: $NEMOCLAW_PATH"
[ -f "$NEMOCLAW_PATH/Dockerfile" ]          || die "Not a NemoClaw source tree (no Dockerfile): $NEMOCLAW_PATH"
[ -f "$NEMOCLAW_PATH/dist/lib/sandbox-build-context.js" ] \
    || die "Not a built NemoClaw tree (run npm run build in NemoClaw first): $NEMOCLAW_PATH"
[ -d "$PLUGIN_DIR/dist" ]                   || die "Run 'npm run build' in $PLUGIN_DIR first (dist/ missing)"
[ -d "$REPO_ROOT/vendor/agentmesh-sdk" ]    || die "Vendored SDK missing: $REPO_ROOT/vendor/agentmesh-sdk"

log "NemoClaw tree: $NEMOCLAW_PATH"
log "Mesh plugin:   $PLUGIN_DIR"

# ── 1. Copy plugin files into <nemoclaw>/scripts/azureclaw-mesh/ ────────────
DST="$NEMOCLAW_PATH/scripts/azureclaw-mesh"
log "Installing plugin files → $DST"
mkdir -p "$DST/dist" "$DST/skills"
cp -r "$PLUGIN_DIR/dist/"*       "$DST/dist/"
cp -r "$PLUGIN_DIR/skills/"*     "$DST/skills/"
cp    "$PLUGIN_DIR/openclaw.plugin.json" "$PLUGIN_DIR/package.json" "$DST/"
[ -f "$PLUGIN_DIR/package-lock.json" ] && cp "$PLUGIN_DIR/package-lock.json" "$DST/"

# ── 1b. Resolve runtime deps (ws, @agentmesh/sdk) ─────────────────────────
# The plugin requires `ws` at runtime. Without this step, fresh NemoClaw
# builds would fail to load the plugin with "Cannot find module 'ws'".
log "Resolving plugin runtime deps (npm install --omit=dev)"
(
    cd "$DST"
    if [ -f package-lock.json ]; then
        npm ci --omit=dev --no-audit --no-fund --silent
    else
        npm install --omit=dev --no-audit --no-fund --no-package-lock --silent
    fi
)

# ── 1c. Overlay vendored @agentmesh/sdk (our 11 patches) ──────────────────
log "Overlaying vendored @agentmesh/sdk"
mkdir -p "$DST/node_modules/@agentmesh"
rm -rf "$DST/node_modules/@agentmesh/sdk"
cp -r "$REPO_ROOT/vendor/agentmesh-sdk" "$DST/node_modules/@agentmesh/sdk"

# ── 2. Patch Dockerfile (idempotent) ────────────────────────────────────────
DOCKERFILE="$NEMOCLAW_PATH/Dockerfile"
if grep -qF "scripts/azureclaw-mesh/" "$DOCKERFILE"; then
    log "Dockerfile already references azureclaw-mesh — skipping"
else
    log "Patching Dockerfile — adding azureclaw-mesh COPY"
    grep -qF 'openclaw plugins install /opt/nemoclaw' "$DOCKERFILE" \
        || die "Couldn't locate anchor 'openclaw plugins install /opt/nemoclaw' in Dockerfile — patch manually:
  COPY --chown=sandbox:sandbox scripts/azureclaw-mesh/ /sandbox/.openclaw-data/extensions/azureclaw-mesh/"
    python3 - "$DOCKERFILE" <<'PY'
import sys, re, pathlib
p = pathlib.Path(sys.argv[1])
src = p.read_text()
anchor_re = re.compile(r'(.*openclaw plugins install /opt/nemoclaw[^\n]*\n)')
insert = """
# Install AzureClaw Mesh plugin (federation + cloud offload)
COPY --chown=sandbox:sandbox scripts/azureclaw-mesh/ /sandbox/.openclaw-data/extensions/azureclaw-mesh/
"""
new = anchor_re.sub(lambda m: m.group(1) + insert, src, count=1)
if new == src:
    sys.exit("failed to insert COPY line")
p.write_text(new)
PY
fi

# ── 3. Patch dist/lib/sandbox-build-context.js (idempotent) ─────────────────
CTX="$NEMOCLAW_PATH/dist/lib/sandbox-build-context.js"
if grep -qF 'azureclaw-mesh' "$CTX"; then
    log "sandbox-build-context.js already references azureclaw-mesh — skipping"
else
    log "Patching sandbox-build-context.js — adding mesh plugin staging"
    python3 - "$CTX" <<'PY'
import sys, re, pathlib
p = pathlib.Path(sys.argv[1])
src = p.read_text()
anchor_re = re.compile(
    r'(fs\.copyFileSync\(path\.join\(rootDir, "scripts", "nemoclaw-start\.sh"\),[^\n]*\n)'
)
insert = '''    // AzureClaw mesh plugin (if present in scripts/)
    const meshPluginSrc = path.join(rootDir, "scripts", "azureclaw-mesh");
    if (fs.existsSync(meshPluginSrc)) {
        fs.cpSync(meshPluginSrc, path.join(stagedScriptsDir, "azureclaw-mesh"), { recursive: true });
    }
'''
new = anchor_re.sub(lambda m: m.group(1) + insert, src, count=1)
if new == src:
    sys.exit("failed to insert mesh plugin staging — anchor not found")
p.write_text(new)
PY
fi

# ── 4. Install azureclaw-mesh policy preset (so `nemoclaw onboard` lists it) ──
PRESETS_DIR="$NEMOCLAW_PATH/nemoclaw-blueprint/policies/presets"
PRESET_DST="$PRESETS_DIR/azureclaw-mesh.yaml"
PLUGIN_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PRESET_SRC="$PLUGIN_ROOT/nemoclaw/setup.sh"

if [ ! -d "$PRESETS_DIR" ]; then
    log "NemoClaw presets dir not found ($PRESETS_DIR) — skipping preset install"
elif [ -f "$PRESET_DST" ] && grep -qF 'AzureClaw mesh federation' "$PRESET_DST"; then
    log "azureclaw-mesh preset already installed — skipping"
else
    log "Installing azureclaw-mesh preset into NemoClaw blueprint"
    if [ ! -x "$PRESET_SRC" ]; then
        chmod +x "$PRESET_SRC" 2>/dev/null || true
    fi
    NEMOCLAW_PRESETS="$PRESETS_DIR" bash "$PRESET_SRC" --install
fi

log "✅ Patched. Rebuild the NemoClaw sandbox image:"
log "   cd $NEMOCLAW_PATH && npm run build && docker build -t nemoclaw:mesh ."
log ""
log "The 'azureclaw-mesh' preset will now appear in \`nemoclaw onboard\` policy selection."
