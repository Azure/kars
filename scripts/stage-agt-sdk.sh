#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
AGT_REPO="${KARS_AGT_REPO:-$HOME/agent-governance-toolkit}"
AGT_URL="${KARS_AGT_URL:-https://github.com/pallakatos/agent-governance-toolkit.git}"
AGT_SHA="${KARS_AGT_SHA:-c1ef74efdadd46546bc772053487c379dd825ae5}"

if [[ ! -d "$AGT_REPO/.git" ]]; then
  git clone --filter=blob:none "$AGT_URL" "$AGT_REPO"
fi
git -C "$AGT_REPO" fetch --depth 1 origin "$AGT_SHA"
git -C "$AGT_REPO" checkout --detach "$AGT_SHA"

TS_DIR="$AGT_REPO/agent-governance-typescript"
STAMP="$TS_DIR/.kars-sdk-sha"
TARBALL="$(find "$TS_DIR" -maxdepth 1 -name 'microsoft-agent-governance-sdk-*.tgz' | head -1 || true)"
if [[ ! -f "$STAMP" || "$(tr -d '\n' < "$STAMP")" != "$AGT_SHA" || -z "$TARBALL" ]]; then
  (
    cd "$TS_DIR"
    npm ci
    npm run build
    rm -f microsoft-agent-governance-sdk-*.tgz
    npm pack --silent
    printf '%s\n' "$AGT_SHA" > "$STAMP"
  )
  TARBALL="$(find "$TS_DIR" -maxdepth 1 -name 'microsoft-agent-governance-sdk-*.tgz' | head -1)"
fi

mkdir -p "$ROOT/.agt-sdk"
find "$ROOT/.agt-sdk" -maxdepth 1 \( -name '*.tgz' -o -name '*.tar.gz' \) -delete
cp "$TARBALL" "$ROOT/.agt-sdk/"
basename "$TARBALL" > "$ROOT/.agt-sdk/name"
echo "Staged AGT SDK $(basename "$TARBALL") from ${AGT_SHA:0:8}"
