#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# install.sh — public, no-auth installer for the kars CLI.
#
#   curl -fsSL https://raw.githubusercontent.com/Azure/kars/main/install.sh | bash
#
# Installs the latest signed public release of @kars/cli from the GitHub
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
CLI_TARBALL="kars-cli-0.1.0.tgz"

if ! command -v npm >/dev/null 2>&1; then
  echo "ERR: npm not found. Install Node.js 22+ first (https://nodejs.org)." >&2
  exit 1
fi

if [ "$VERSION" = "latest" ]; then
  URL="https://github.com/${REPO}/releases/latest/download/${CLI_TARBALL}"
else
  URL="https://github.com/${REPO}/releases/download/${VERSION}/${CLI_TARBALL}"
fi

echo "Installing the kars CLI from:"
echo "  ${URL}"
npm install -g "${URL}"

echo ""
echo "✓ kars CLI installed.  Next:"
echo "    kars dev --release      # run from published, signed images (Docker or kind)"
