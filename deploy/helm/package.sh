#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# Package the kars Helm chart locally — for release engineering
# and local testing only. The packaged tarball is *not* published from
# this script; publishing to a registry is a separate, deliberate step
# performed by a maintainer with the appropriate credentials.
#
# Usage:
#   bash deploy/helm/package.sh                 # → ./dist/charts/
#   DEST=/tmp/foo bash deploy/helm/package.sh   # custom destination
#
# The script:
#   1. Runs `helm lint` and fails fast on any error.
#   2. Calls `helm package` to produce kars-<version>.tgz.
#   3. Computes a sha256 alongside the tarball.
#   4. Prints the resulting paths.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CHART_DIR="${ROOT}/deploy/helm/kars"
DEST="${DEST:-${ROOT}/dist/charts}"

if ! command -v helm >/dev/null 2>&1; then
    echo "ERROR: helm not found on PATH" >&2
    exit 1
fi

mkdir -p "$DEST"

echo "▶ helm lint ${CHART_DIR}"
helm lint "$CHART_DIR"

echo "▶ helm package ${CHART_DIR} → ${DEST}"
helm package "$CHART_DIR" --destination "$DEST"

# Find the just-produced tarball (the one with the latest mtime).
tarball="$(ls -1t "${DEST}"/kars-*.tgz | head -1)"
if [[ -z "$tarball" ]]; then
    echo "ERROR: package did not produce a tarball" >&2
    exit 1
fi

echo "▶ sha256 ${tarball}"
if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$tarball" | tee "${tarball}.sha256"
else
    shasum -a 256 "$tarball" | tee "${tarball}.sha256"
fi

echo
echo "Packaged chart: ${tarball}"
echo "Digest:         ${tarball}.sha256"
