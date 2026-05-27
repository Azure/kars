#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# patch-nemoclaw.sh — bolt the kars-mesh plugin into a NemoClaw source tree.
#
# Fresh Azure/kars clone + fresh NemoClaw clone:
#
#   cd kars/mesh-plugin
#   npm run build
#   NEMOCLAW_PATH=~/.nemoclaw/source ./scripts/patch-nemoclaw.sh
#   cd "$NEMOCLAW_PATH" && docker build -t nemoclaw:mesh .
#
# All edits are idempotent — safe to re-run after `git pull` on either repo.
#
# Effects on the target NemoClaw tree:
#   1. Copy mesh-plugin/dist into <nemoclaw>/scripts/kars-mesh/
#   2. Patch Dockerfile: add COPY line for the plugin (after openclaw plugins install)
#   3. Patch dist/lib/sandbox-build-context.js: stage kars-mesh into build ctx

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

log "NemoClaw tree: $NEMOCLAW_PATH"
log "Mesh plugin:   $PLUGIN_DIR"

# ── 1. Copy plugin files into <nemoclaw>/scripts/kars-mesh/ ────────────
DST="$NEMOCLAW_PATH/scripts/kars-mesh"
log "Installing plugin files → $DST"
mkdir -p "$DST/dist" "$DST/skills"
cp -r "$PLUGIN_DIR/dist/"*       "$DST/dist/"
cp -r "$PLUGIN_DIR/skills/"*     "$DST/skills/"
cp    "$PLUGIN_DIR/openclaw.plugin.json" "$PLUGIN_DIR/package.json" "$DST/"
[ -f "$PLUGIN_DIR/package-lock.json" ] && cp "$PLUGIN_DIR/package-lock.json" "$DST/"

# ── 1b. Resolve runtime deps (ws, @microsoft/agent-governance-sdk) ─────────
# The plugin requires `ws` and the AGT SDK at runtime. Without this step,
# fresh NemoClaw builds would fail to load the plugin with "Cannot find
# module 'ws'" or similar.
log "Resolving plugin runtime deps (npm install --omit=dev)"
(
    cd "$DST"
    if [ -f package-lock.json ]; then
        npm ci --omit=dev --no-audit --no-fund --silent
    else
        npm install --omit=dev --no-audit --no-fund --no-package-lock --silent
    fi
)

# ── 2. Patch Dockerfile (idempotent) ────────────────────────────────────────
DOCKERFILE="$NEMOCLAW_PATH/Dockerfile"
if grep -qF "scripts/kars-mesh/" "$DOCKERFILE"; then
    log "Dockerfile already references kars-mesh — skipping"
else
    log "Patching Dockerfile — adding kars-mesh COPY"
    grep -qF 'openclaw plugins install /opt/nemoclaw' "$DOCKERFILE" \
        || die "Couldn't locate anchor 'openclaw plugins install /opt/nemoclaw' in Dockerfile — patch manually:
  COPY --chown=sandbox:sandbox scripts/kars-mesh/ /sandbox/.openclaw-data/extensions/kars-mesh/"
    python3 - "$DOCKERFILE" <<'PY'
import sys, re, pathlib
p = pathlib.Path(sys.argv[1])
src = p.read_text()
anchor_re = re.compile(r'(.*openclaw plugins install /opt/nemoclaw[^\n]*\n)')
insert = """
# Install Kars Mesh plugin (federation + cloud offload)
COPY --chown=sandbox:sandbox scripts/kars-mesh/ /sandbox/.openclaw-data/extensions/kars-mesh/
"""
new = anchor_re.sub(lambda m: m.group(1) + insert, src, count=1)
if new == src:
    sys.exit("failed to insert COPY line")
p.write_text(new)
PY
fi

# ── 3. Patch dist/lib/sandbox-build-context.js (idempotent) ─────────────────
CTX="$NEMOCLAW_PATH/dist/lib/sandbox-build-context.js"
if grep -qF 'kars-mesh' "$CTX"; then
    log "sandbox-build-context.js already references kars-mesh — skipping"
else
    log "Patching sandbox-build-context.js — adding mesh plugin staging"
    python3 - "$CTX" <<'PY'
import sys, re, pathlib
p = pathlib.Path(sys.argv[1])
src = p.read_text()
anchor_re = re.compile(
    r'(fs\.copyFileSync\(path\.join\(rootDir, "scripts", "nemoclaw-start\.sh"\),[^\n]*\n)'
)
insert = '''    // Kars mesh plugin (if present in scripts/)
    const meshPluginSrc = path.join(rootDir, "scripts", "kars-mesh");
    if (fs.existsSync(meshPluginSrc)) {
        fs.cpSync(meshPluginSrc, path.join(stagedScriptsDir, "kars-mesh"), { recursive: true });
    }
'''
new = anchor_re.sub(lambda m: m.group(1) + insert, src, count=1)
if new == src:
    sys.exit("failed to insert mesh plugin staging — anchor not found")
p.write_text(new)
PY
fi

# ── 4. Install kars-mesh policy preset (so `nemoclaw onboard` lists it) ──
PRESETS_DIR="$NEMOCLAW_PATH/nemoclaw-blueprint/policies/presets"
PRESET_DST="$PRESETS_DIR/kars-mesh.yaml"
PLUGIN_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PRESET_SRC="$PLUGIN_ROOT/nemoclaw/setup.sh"

if [ ! -d "$PRESETS_DIR" ]; then
    log "NemoClaw presets dir not found ($PRESETS_DIR) — skipping preset install"
elif [ -f "$PRESET_DST" ] && grep -qF 'Kars mesh federation' "$PRESET_DST"; then
    log "kars-mesh preset already installed — skipping"
else
    log "Installing kars-mesh preset into NemoClaw blueprint"
    if [ ! -x "$PRESET_SRC" ]; then
        chmod +x "$PRESET_SRC" 2>/dev/null || true
    fi
    NEMOCLAW_PRESETS="$PRESETS_DIR" bash "$PRESET_SRC" --install
fi

log "✅ Patched. Rebuild the NemoClaw sandbox image:"
log "   cd $NEMOCLAW_PATH && npm run build && docker build -t nemoclaw:mesh ."
log ""
log "The 'kars-mesh' preset will now appear in \`nemoclaw onboard\` policy selection."
