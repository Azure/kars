#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# install.sh — public, no-auth installer for the kars CLI (`kars-runtime` on npm).
#
#   curl -fsSL https://raw.githubusercontent.com/Azure/kars/main/install.sh | bash
#
# Installs the latest signed public release of the kars CLI from the GitHub
# Release. No GitHub login, no Azure account, no org membership — every
# artefact is cosign-signed + SBOM'd + SLSA-attested.
#
# Pin a specific version:
#   KARS_VERSION=v0.1.0 bash -c "$(curl -fsSL https://raw.githubusercontent.com/Azure/kars/main/install.sh)"
#
# After install:  kars dev --release
set -euo pipefail

REPO="${KARS_REPO:-Azure/kars}"
VERSION="${KARS_VERSION:-latest}"

command -v npm  >/dev/null 2>&1 || { echo "ERR: npm not found. Install Node.js 22+ (https://nodejs.org)." >&2; exit 1; }
command -v curl >/dev/null 2>&1 || { echo "ERR: curl not found." >&2; exit 1; }

if [ "$VERSION" = "latest" ]; then
  API="https://api.github.com/repos/${REPO}/releases/latest"
else
  API="https://api.github.com/repos/${REPO}/releases/tags/${VERSION}"
fi

# Resolve the CLI tarball asset URL dynamically (asset name carries the
# version, and may be kars-runtime-*.tgz or an older kars-cli-*.tgz).
URL="$(curl -fsSL "$API" \
  | grep -oE '"browser_download_url"[[:space:]]*:[[:space:]]*"[^"]*\.tgz"' \
  | grep -iE 'kars' \
  | head -1 \
  | sed -E 's/.*"(https[^"]+)".*/\1/')"

if [ -z "${URL:-}" ]; then
  echo "ERR: could not find a CLI tarball in the ${VERSION} release of ${REPO}." >&2
  echo "     See https://github.com/${REPO}/releases" >&2
  exit 1
fi

echo "Installing the kars CLI from:"
echo "  ${URL}"
npm install -g "${URL}"

echo ""
echo "✓ kars CLI installed.  Next:"
echo "    kars dev --release      # run from published, signed images (Docker or kind)"
