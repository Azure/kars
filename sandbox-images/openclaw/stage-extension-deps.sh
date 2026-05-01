#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
# Ensure every dependency declared by every bundled OpenClaw extension is
# resolvable in the pre-staged node_modules tree.
#
# Background. OpenClaw's `openclaw doctor --fix` pre-stages plugin runtime
# deps for the *configured* extensions. But OpenClaw's own bundled chunks
# (e.g. dist/oauth-*.js, used by `memory-core`) have static `require()`
# chains into deps that are only declared by an *unconfigured* extension's
# manifest (e.g. the `xai` plugin declares `@mariozechner/pi-ai`, and the
# OpenClaw oauth chunk transitively needs it whether xai is enabled or
# not). When `memory-core` loads in the node-host process, the require
# chain fails with `Cannot find module '@mariozechner/pi-ai/dist/oauth.js'`
# and the node-host crashes.
#
# Fix. After `openclaw doctor`, scan every bundled extension's
# package.json, take the union of declared `dependencies`, and `npm
# install --no-save` anything that isn't already present in the stage
# tree's node_modules. Idempotent: missing-set is empty on a clean run.
#
# This runs at base-image build time only. The threat model is identical
# to the existing `npm install` steps in Dockerfile.base — full network
# at build, frozen-in-image at runtime.
#
# SPDX-FileCopyrightText: Copyright (c) Microsoft Corporation
# SPDX-License-Identifier: MIT
set -euo pipefail

EXT_DIR="${1:-/usr/local/lib/node_modules/openclaw/dist/extensions}"
STAGE_ROOT="${2:-/opt/openclaw-stage}"

if [ ! -d "$EXT_DIR" ]; then
  echo "stage-extension-deps: no extensions dir at $EXT_DIR — skipping"
  exit 0
fi

# Pick the (single) staged version dir. Doctor produces one per OpenClaw
# version; we only support one OpenClaw per image.
stage_dir=$(find "$STAGE_ROOT" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | head -n1 || true)
if [ -z "$stage_dir" ]; then
  echo "stage-extension-deps: no version dir under $STAGE_ROOT — doctor staged nothing, skipping"
  exit 0
fi
mkdir -p "$stage_dir/node_modules"

# Compute missing deps: union of all extension manifest `dependencies`,
# minus what's already resolvable in the stage tree.
missing=$(node -e '
  const fs = require("fs");
  const path = require("path");
  const extDir = process.argv[1];
  const stageNm = process.argv[2];
  const want = new Map();
  for (const d of fs.readdirSync(extDir)) {
    const pj = path.join(extDir, d, "package.json");
    if (!fs.existsSync(pj)) continue;
    let manifest;
    try { manifest = JSON.parse(fs.readFileSync(pj, "utf8")); }
    catch (e) { continue; }
    const deps = manifest.dependencies || {};
    for (const [name, range] of Object.entries(deps)) {
      // Last writer wins; differing ranges across manifests are rare and
      // npm will pick a satisfying version.
      want.set(name, range);
    }
  }
  const out = [];
  for (const [name, range] of want) {
    if (!fs.existsSync(path.join(stageNm, name, "package.json"))) {
      out.push(name + "@" + range);
    }
  }
  process.stdout.write(out.join("\n"));
' "$EXT_DIR" "$stage_dir/node_modules")

if [ -z "$missing" ]; then
  echo "stage-extension-deps: all extension manifest deps already resolvable"
  exit 0
fi

echo "stage-extension-deps: filling gaps in $stage_dir/node_modules:"
echo "$missing" | sed 's/^/  - /'

# Install into the existing stage tree without writing a package.json.
# npm install with no package.json creates one transparently and removes
# it after we move the deps out... actually it keeps it. We accept a
# minimal package.json artifact in the stage dir — it doesn't affect
# resolution.
cd "$stage_dir"
[ -f package.json ] || echo '{"name":"openclaw-stage","private":true}' > package.json

# shellcheck disable=SC2086
xargs -r npm install --no-save --no-audit --no-fund --no-progress --omit=dev <<<"$missing"

echo "stage-extension-deps: completed; stage size $(du -sh "$stage_dir" | cut -f1)"
