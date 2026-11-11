#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
# ci/check-agt-released.sh
#
# Runs daily (see .github/workflows/check-agt-released.yml) to detect
# when Microsoft AGT publishes a release containing both the Rust crate
# pin in Cargo.toml and the TypeScript SDK pin in vendor/agt/pin.json.
#
# Opens a GitHub issue the moment a fixed release is available so we
# don't sit on the git pin longer than necessary. Closes the issue if
# one already exists and the pin is gone.
#
# Strategy:
#   1. Read both pinned commit SHAs.
#   2. Check whether a release tag contains both commits.
#   3. File an issue only after both pins have shipped.
#
# Exit codes:
#   0 — no released version yet (status quo) OR issue already exists
#   0 — newly filed issue
#   1 — unexpected error
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

CARGO="Cargo.toml"
[ -f "$CARGO" ] || { echo "no Cargo.toml — wrong directory" >&2; exit 1; }

# Extract the Rust crate pin from [patch.crates-io] and the TypeScript SDK pin.
RUST_PINNED_SHA=$(grep -oE 'agentmesh.*git.*rev = "([a-f0-9]{40})"' "$CARGO" \
  | head -1 \
  | grep -oE '[a-f0-9]{40}' \
  | head -1 || true)
SDK_PINNED_SHA=$(jq -r '.sha // empty' vendor/agt/pin.json)

if [ -z "$RUST_PINNED_SHA" ] || [ -z "$SDK_PINNED_SHA" ]; then
  echo "::notice::One or both AGT pins are absent — nothing to check."
  exit 0
fi

echo "Pinned AGT Rust commit: $RUST_PINNED_SHA"
echo "Pinned AGT TypeScript SDK commit: $SDK_PINNED_SHA"

tag_contains_commit() {
  local sha="$1"
  local tag="$2"
  local status
  status=$(gh api "repos/microsoft/agent-governance-toolkit/compare/$sha...$tag" \
    --jq '.status' 2>/dev/null || echo "")
  case "$status" in
    ahead|identical) return 0 ;;
    *) return 1 ;;
  esac
}

RELEASED_TAG=""
while read -r TAG; do
  [ -z "$TAG" ] && continue
  if tag_contains_commit "$RUST_PINNED_SHA" "$TAG" \
    && tag_contains_commit "$SDK_PINNED_SHA" "$TAG"; then
    RELEASED_TAG="$TAG"
    echo "Found released tag containing both AGT pins: $TAG"
    break
  fi
done < <(gh api 'repos/microsoft/agent-governance-toolkit/releases?per_page=10' \
           --jq '.[] | select(.draft == false) | .tag_name')

if [ -z "$RELEASED_TAG" ]; then
  echo "::notice::Both AGT pins are not yet in the same release — keeping the git override and SDK tarball."
  exit 0
fi

# Released! File a tracking issue (idempotent — search first).
EXISTING=$(gh issue list \
  --repo "${GITHUB_REPOSITORY:-Azure/kars}" \
  --state open \
  --label release \
  --search "AGT $RELEASED_TAG flip git pin in:title" \
  --json number --jq '.[0].number // empty' 2>/dev/null || echo "")

if [ -n "$EXISTING" ]; then
  echo "::notice::Tracking issue #$EXISTING already open for $RELEASED_TAG"
  exit 0
fi

TITLE="AGT $RELEASED_TAG released — flip git pin in Cargo.toml + revendor SDK"
BODY=$(cat <<MD
AGT upstream just released **$RELEASED_TAG**, which contains both commits
currently pinned by Kars:

- Rust crate pin: \`$RUST_PINNED_SHA\`
- TypeScript SDK pin: \`$SDK_PINNED_SHA\`
- Released tag: \`$RELEASED_TAG\`

Both pins can now be replaced with the release. To-do:

### Rust side
1. \`Cargo.toml\` — set \`agentmesh\` and \`agentmesh-mcp\` to the released
   version (\`$RELEASED_TAG\` without the \`v\` prefix)
2. Delete the \`[patch.crates-io]\` block at the bottom of \`Cargo.toml\`
3. \`cargo update -p agentmesh -p agentmesh-mcp\`
4. \`cargo check --workspace\` to verify

### npm side
1. Rebuild and revendor the AGT TypeScript SDK from upstream
   $RELEASED_TAG (run \`scripts/vendor-agt-sdk.sh\` once we add it,
   or manually clone + npm pack + commit the tarball)
2. Or, if the released npm \`@microsoft/agent-governance-sdk@$RELEASED_TAG\`
   now contains the fix, switch \`mesh-plugin/package.json\` and
   \`runtimes/openclaw/package.json\` back to the published version
   and remove \`vendor/agt/\` entirely

### Auto-filed by
\`.github/workflows/check-agt-released.yml\` on $(date -u +%Y-%m-%d)
MD
)

echo "Filing tracking issue: $TITLE"
gh issue create \
  --repo "${GITHUB_REPOSITORY:-Azure/kars}" \
  --label release \
  --label onboarding \
  --title "$TITLE" \
  --body "$BODY"
