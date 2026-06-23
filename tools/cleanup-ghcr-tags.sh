#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# cleanup-ghcr-tags.sh — prune non-official tags from the public kars GHCR
# packages so only what we officially release remains.
#
# Deletes (cruft):
#   - CI build-cache tags:  sha-<7hex>, main, dev, <branch>
#   - per-arch build intermediates:  *-amd64, *-arm64
#   - (optional, with --drop-old-interims) every v*-interim.* release tag
#
# KEEPS (official):
#   - latest
#   - clean release tags vMAJOR.MINOR.PATCH  (e.g. v0.1.0)
#   - cosign signatures / attestations:  sha256-*.sig, sha256-*.att
#   - the newest interim (unless --drop-old-interims)
#
# Requirements: a token with `delete:packages` (+ `read:packages`):
#   gh auth refresh -h github.com -s delete:packages,read:packages
#   # or: export GH_TOKEN=<PAT with delete:packages>
#
# Usage:
#   ./tools/cleanup-ghcr-tags.sh                 # dry-run (prints what it WOULD delete)
#   APPLY=1 ./tools/cleanup-ghcr-tags.sh         # actually delete
#   APPLY=1 ./tools/cleanup-ghcr-tags.sh --drop-old-interims
set -euo pipefail

ORG="Azure"
APPLY="${APPLY:-0}"
DROP_OLD_INTERIMS=0
[ "${1:-}" = "--drop-old-interims" ] && DROP_OLD_INTERIMS=1

# The official package set we publish.
PACKAGES=(
  kars-controller kars-inference-router kars-a2a-gateway kars-conformance-runner
  kars-sandbox-base openclaw-sandbox
  kars-agentmesh-relay kars-agentmesh-registry
  kars-runtime-hermes kars-runtime-langgraph kars-runtime-maf-python
  kars-runtime-anthropic kars-runtime-openai-agents kars-runtime-pydantic-ai
)

is_cruft_tag() {
  local t="$1"
  case "$t" in
    sha-[0-9a-f]*|main|dev) return 0 ;;          # CI build-cache tags
    *-amd64|*-arm64)        return 0 ;;          # per-arch intermediates
  esac
  if [ "$DROP_OLD_INTERIMS" = 1 ]; then
    case "$t" in v*-interim.*|v*-interim) return 0 ;; esac
  fi
  return 1
}

total_del=0
for pkg in "${PACKAGES[@]}"; do
  echo "── $pkg ─────────────────────────────────────────────"
  # Page through all versions.
  page=1
  while :; do
    versions=$(gh api "/orgs/${ORG}/packages/container/${pkg}/versions?per_page=100&page=${page}" 2>/dev/null || echo '[]')
    count=$(echo "$versions" | jq 'length')
    [ "$count" -eq 0 ] && break
    while IFS=$'\t' read -r id tags; do
      [ -z "$id" ] && continue
      # A version is deletable only if it has tags AND every tag is cruft.
      # (Untagged versions — orphaned layers / sig targets — are left alone.)
      [ -z "$tags" ] && continue
      keep=0
      for t in $tags; do is_cruft_tag "$t" || { keep=1; break; }; done
      if [ "$keep" = 0 ]; then
        echo "  DELETE id=$id  tags=[$tags]"
        total_del=$((total_del+1))
        if [ "$APPLY" = 1 ]; then
          gh api -X DELETE "/orgs/${ORG}/packages/container/${pkg}/versions/${id}" >/dev/null \
            && echo "    ✓ deleted" || echo "    ✗ delete failed (need delete:packages?)"
        fi
      fi
    done < <(echo "$versions" | jq -r '.[] | [(.id|tostring), ((.metadata.container.tags // []) | join(" "))] | @tsv')
    page=$((page+1))
  done
done

echo "────────────────────────────────────────────────────────"
if [ "$APPLY" = 1 ]; then
  echo "Deleted $total_del version(s)."
else
  echo "DRY RUN — would delete $total_del version(s). Re-run with APPLY=1 to apply."
fi
