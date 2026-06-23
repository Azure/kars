#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# cleanup-ghcr-tags.sh — prune non-official tags from the public kars GHCR
# packages so only what we officially release remains.
#
# Deletes (cruft):  sha-<7hex>, main, dev, <branch>, *-amd64, *-arm64,
#                   and (with --drop-old-interims) v*-interim.* tags.
# KEEPS:  latest, clean vMAJOR.MINOR.PATCH tags, cosign sha256-*.sig/.att,
#         and untagged versions (orphaned layers / signature targets).
#
# Needs a token with read:packages + delete:packages:
#   gh auth refresh -h github.com -s read:packages,delete:packages
#
# Usage:
#   ./tools/cleanup-ghcr-tags.sh                  # dry-run
#   APPLY=1 ./tools/cleanup-ghcr-tags.sh          # delete
#   APPLY=1 ./tools/cleanup-ghcr-tags.sh --drop-old-interims
set -euo pipefail

ORG="Azure"
APPLY="${APPLY:-0}"
DROP_OLD_INTERIMS=0
[ "${1:-}" = "--drop-old-interims" ] && DROP_OLD_INTERIMS=1

command -v gh >/dev/null || { echo "ERR: gh CLI not found"; exit 1; }
command -v jq >/dev/null || { echo "ERR: jq not found"; exit 1; }

if ! gh api "/orgs/${ORG}/packages/container/kars-controller/versions?per_page=1" >/dev/null 2>&1; then
  echo "ERR: cannot list package versions for org '${ORG}'."
  echo "     Token needs read:packages (+ delete:packages to delete):"
  echo "       gh auth refresh -h github.com -s read:packages,delete:packages"
  # Common trap: a GH_TOKEN / GITHUB_TOKEN env var overrides the keyring
  # token, and `gh auth refresh` only updates the keyring — so the refresh
  # silently has no effect while the env token keeps its old scopes.
  if [ -n "${GH_TOKEN:-}" ] || [ -n "${GITHUB_TOKEN:-}" ]; then
    echo
    echo "  NOTE: GH_TOKEN/GITHUB_TOKEN is set in your environment. gh uses that"
    echo "        token (not the keyring), and 'gh auth refresh' canNOT add scopes"
    echo "        to an env-var token. Unset it first, then refresh:"
    echo "          unset GH_TOKEN GITHUB_TOKEN"
    echo "          gh auth refresh -h github.com -s read:packages,delete:packages"
    echo "        (or export a token that already has read:packages,delete:packages)"
  fi
  exit 1
fi

PACKAGES=(
  kars-controller kars-inference-router kars-a2a-gateway kars-conformance-runner
  kars-sandbox-base openclaw-sandbox
  kars-agentmesh-relay kars-agentmesh-registry
  kars-runtime-hermes kars-runtime-langgraph kars-runtime-maf-python
  kars-runtime-anthropic kars-runtime-openai-agents kars-runtime-pydantic-ai
)

is_cruft_tag() {
  case "$1" in
    sha-[0-9a-f]*|main|dev) return 0 ;;
    *-amd64|*-arm64)
      # Per-arch tags are cruft EXCEPT when they back a clean published
      # release (vMAJOR.MINOR.PATCH with no pre-release suffix). For the
      # natively-built+merged images (sandbox-base, openclaw-sandbox,
      # relay, registry) the multi-arch vX.Y.Z manifest list references
      # these per-arch images — deleting v0.1.1-arm64 etc. risks breaking
      # the very release `kars dev --release vX.Y.Z` pulls. Keep them.
      local base="$1"
      base="${base%-amd64}"; base="${base%-arm64}"
      case "$base" in
        v[0-9]*.[0-9]*.[0-9]*)
          case "$base" in
            *-*) return 0 ;;   # has a pre-release suffix (interim/rc/…) → cruft
            *)   return 1 ;;   # clean release base → KEEP (load-bearing)
          esac ;;
        *) return 0 ;;          # non-release base (sha/branch) → cruft
      esac ;;
  esac
  if [ "$DROP_OLD_INTERIMS" = 1 ]; then
    case "$1" in v*-interim.*|v*-interim) return 0 ;; esac
  fi
  return 1
}

total_del=0
for pkg in "${PACKAGES[@]}"; do
  echo "── ${pkg} ─────────────────────────────────────────────"
  while IFS=$'\t' read -r id tags; do
    [ -z "${id:-}" ] && continue
    [ -z "${tags:-}" ] && continue
    keep=0
    for t in $tags; do is_cruft_tag "$t" || { keep=1; break; }; done
    [ "$keep" = 1 ] && continue
    echo "  DELETE id=${id}  tags=[${tags}]"
    total_del=$((total_del + 1))
    if [ "$APPLY" = 1 ]; then
      if gh api -X DELETE "/orgs/${ORG}/packages/container/${pkg}/versions/${id}" >/dev/null 2>&1; then
        echo "    ✓ deleted"
      else
        echo "    ✗ delete failed (need delete:packages scope?)"
      fi
    fi
  done < <(gh api --paginate "/orgs/${ORG}/packages/container/${pkg}/versions" \
             --jq '.[] | [(.id|tostring), ((.metadata.container.tags // []) | join(" "))] | @tsv' 2>/dev/null)
done

echo "────────────────────────────────────────────────────────"
if [ "$APPLY" = 1 ]; then
  echo "Deleted ${total_del} version(s)."
else
  echo "DRY RUN — would delete ${total_del} version(s). Re-run with APPLY=1 to apply."
fi
