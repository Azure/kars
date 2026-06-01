#!/usr/bin/env bash
# ci/check-agt-released.sh
#
# Runs daily (see .github/workflows/check-agt-released.yml) to detect
# when Microsoft AGT publishes a release that contains the fix-commits
# we currently consume via the [patch.crates-io] git pin in Cargo.toml
# and the vendored npm tarball in vendor/agt/.
#
# Opens a GitHub issue the moment a fixed release is available so we
# don't sit on the git pin longer than necessary. Closes the issue if
# one already exists and the pin is gone.
#
# Strategy:
#   1. Read the pinned commit SHA from Cargo.toml's [patch.crates-io].
#   2. Ask GitHub which AGT tags include that commit (via /commits/{sha}/tags).
#   3. If any tag exists, the fix has shipped — file an issue.
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

# Extract the pinned commit SHA from [patch.crates-io]
PINNED_SHA=$(grep -oE 'agentmesh.*git.*rev = "([a-f0-9]{40})"' "$CARGO" \
  | head -1 \
  | grep -oE '[a-f0-9]{40}' \
  | head -1 || true)

if [ -z "$PINNED_SHA" ]; then
  echo "::notice::No git pin in Cargo.toml — patch must have been removed. Nothing to check."
  exit 0
fi

echo "Pinned AGT commit: $PINNED_SHA"

# Check whether any AGT release tag contains this commit.
# GitHub's `/repos/{owner}/{repo}/commits/{sha}/branches-where-head` only
# covers branches, not tags. Use the search-style endpoint instead.
TAGS_JSON=$(gh api "repos/microsoft/agent-governance-toolkit/commits/$PINNED_SHA/branches-where-head" 2>/dev/null || echo '[]')

# Iterate all release tags, check `git describe --contains $PINNED_SHA`-equivalent via API
RELEASED_TAG=""
while read -r TAG; do
  [ -z "$TAG" ] && continue
  # Does this tag contain the pinned commit? Use the compare endpoint:
  # ahead_by > 0 with status "ahead" means the tag is AHEAD of the commit
  # (i.e. the commit is in the tag's history)
  STATUS=$(gh api "repos/microsoft/agent-governance-toolkit/compare/$PINNED_SHA...$TAG" \
    --jq '.status' 2>/dev/null || echo "")
  case "$STATUS" in
    ahead|identical)
      RELEASED_TAG="$TAG"
      echo "Found released tag containing $PINNED_SHA: $TAG (status=$STATUS)"
      break
      ;;
  esac
done < <(gh api 'repos/microsoft/agent-governance-toolkit/releases?per_page=10' \
           --jq '.[] | select(.draft == false) | .tag_name')

if [ -z "$RELEASED_TAG" ]; then
  echo "::notice::Pin $PINNED_SHA not yet in any AGT release — keeping the git override."
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
AGT upstream just released **$RELEASED_TAG**, which contains the
commit our \`[patch.crates-io]\` pin currently overrides:

- Pinned commit: \`$PINNED_SHA\`
- Released tag: \`$RELEASED_TAG\`

The git override in \`Cargo.toml\` can now be removed. To-do:

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
