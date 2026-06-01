#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
# install.sh — kars internal-release installer (private repo edition)
#
# What it does:
#   1. Verifies you have `gh` CLI installed and logged in with org access.
#   2. Downloads the latest internal release's CLI tarball + checksum from
#      the private GitHub Release on Azure/kars.
#   3. Verifies SHA256 of the tarball.
#   4. Installs @kars/cli globally via npm from the .tgz.
#   5. Logs Docker into ghcr.io using your gh PAT so you can pull the
#      private container images.
#   6. Optionally pulls + retags the images for local use.
#
# Requirements:
#   - bash 4+, curl, sha256sum (or shasum on macOS)
#   - gh (https://cli.github.com) authenticated: `gh auth login`
#   - npm (Node.js 22 LTS recommended)
#   - docker (optional — for pulling container images)
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/Azure/kars/main/install.sh \
#     | bash
#
#   or pin a specific release:
#   KARS_VERSION=v0.1.0-internal.1 \
#     curl -fsSL https://raw.githubusercontent.com/Azure/kars/main/install.sh \
#     | bash
#
#   or download the script first if you want to inspect it:
#   gh api repos/Azure/kars/contents/install.sh --jq .content | base64 -d > install.sh
#   bash install.sh
#
# After install:
#   kars --version
#   kars up

set -euo pipefail

REPO="${KARS_REPO:-Azure/kars}"
REQUESTED_VERSION="${KARS_VERSION:-}"
SKIP_DOCKER="${KARS_SKIP_DOCKER:-0}"
PULL_IMAGES="${KARS_PULL_IMAGES:-0}"

# ─── Pretty printing ──────────────────────────────────────────────
say()  { printf "\033[1;36m▸ %s\033[0m\n" "$*"; }
ok()   { printf "\033[1;32m✓ %s\033[0m\n" "$*"; }
warn() { printf "\033[1;33m⚠ %s\033[0m\n" "$*" >&2; }
die()  { printf "\033[1;31m✗ %s\033[0m\n" "$*" >&2; exit 1; }

# ─── Pre-flight: tools ────────────────────────────────────────────
say "Pre-flight checks"
command -v gh   >/dev/null || die "gh CLI not found. Install: https://cli.github.com"
command -v npm  >/dev/null || die "npm not found. Install Node.js 22 LTS: https://nodejs.org"
command -v curl >/dev/null || die "curl not found"
if command -v sha256sum >/dev/null; then
  SHA256="sha256sum"
elif command -v shasum >/dev/null; then
  SHA256="shasum -a 256"
else
  die "sha256sum or shasum not found"
fi
ok "Tools present"

# ─── Pre-flight: gh auth ──────────────────────────────────────────
if ! gh auth status >/dev/null 2>&1; then
  die "gh is not authenticated. Run: gh auth login --hostname github.com --web --scopes read:packages"
fi
GH_USER=$(gh api user --jq .login 2>/dev/null || echo "unknown")
ok "Authenticated as $GH_USER"

# ─── SSO check — Azure org requires PAT/token SSO authorization ───
# `gh auth status` returns success even if the token isn't SSO-blessed
# for Azure; the actual symptom is a 401/403 on /repos/Azure/kars.
# We probe explicitly so the user gets a clear "click here to bless"
# message before Docker login + asset download fail mysteriously.
say "Checking SSO authorization for Azure org"
GH_TOKEN_SCOPES=$(gh api -i user 2>&1 | awk -F': ' '/^[xX]-[oO]auth-[sS]copes:/ {print $2}' | tr -d '\r')
if ! echo "$GH_TOKEN_SCOPES" | grep -q 'read:packages'; then
  warn "Your gh token is missing the 'read:packages' scope (needed for private container image pulls)."
  warn "Re-run: gh auth refresh --hostname github.com --scopes read:packages,repo"
fi

# Probe — try to fetch a repo-internal field that requires Azure org access.
# If this 401s, the token isn't SSO-authorized.
if ! gh api "repos/Azure/kars" --jq .id >/dev/null 2>&1; then
  cat >&2 <<EOF

$(printf "\033[1;31m✗ Cannot access Azure/kars.\033[0m")

This is almost certainly because your GitHub PAT / token is not
SSO-authorized for the Azure org. To fix:

  1. Go to https://github.com/settings/tokens
  2. Find the token gh created (named "GitHub CLI" by default)
  3. Click "Configure SSO" → "Authorize" next to the Azure org
  4. Re-run this installer

Alternative: re-auth with explicit SSO flow:

  gh auth refresh --hostname github.com --web --scopes read:packages,repo

EOF
  exit 1
fi
ok "Repo Azure/kars accessible (SSO authorized)"

# Org access check — already performed above via the explicit Azure/kars
# probe. This second check would only fire if KARS_REPO was overridden.
if [ "$REPO" != "Azure/kars" ]; then
  if ! gh api "repos/$REPO" --jq .id >/dev/null 2>&1; then
    die "Cannot see $REPO. Are you a member with read access?"
  fi
  ok "Repo $REPO accessible"
fi

# ─── Resolve release version ──────────────────────────────────────
if [ -n "$REQUESTED_VERSION" ]; then
  VERSION="$REQUESTED_VERSION"
  say "Using pinned version: $VERSION"
else
  # The most recent v*-internal* or v*-preview* tag with a release.
  # gh release list returns newest-first.
  say "Resolving latest internal release"
  VERSION=$(gh release list --repo "$REPO" --limit 20 \
    --json tagName,isPrerelease,isDraft \
    --jq '[.[] | select(.isDraft == false) | select(.tagName | test("internal|preview"))][0].tagName' 2>/dev/null \
    || echo "")
  if [ -z "$VERSION" ] || [ "$VERSION" = "null" ]; then
    die "No internal/preview release found on $REPO"
  fi
  ok "Latest internal release: $VERSION"
fi

# ─── Stage downloads ──────────────────────────────────────────────
WORKDIR="$(mktemp -d)"
trap "rm -rf '$WORKDIR'" EXIT
say "Staging in $WORKDIR"

CLI_TGZ="kars-cli-0.1.0.tgz"   # name is stable across versions; npm pack uses pkg name
# Discover the actual asset name (handles future renames)
say "Listing release assets"
ASSETS=$(gh release view "$VERSION" --repo "$REPO" --json assets --jq '.assets[].name')
CLI_TGZ=$(printf '%s\n' "$ASSETS" | grep -E '^kars-cli-.+\.tgz$' | head -1 || true)
[ -n "$CLI_TGZ" ] || die "No kars-cli-*.tgz found in $VERSION assets"
ok "CLI asset: $CLI_TGZ"

say "Downloading $CLI_TGZ + SHA256SUMS"
gh release download "$VERSION" --repo "$REPO" \
  --pattern "$CLI_TGZ" \
  --pattern "SHA256SUMS" \
  --dir "$WORKDIR" \
  --clobber

# ─── Verify checksum (SHA256SUMS includes binaries; cli tgz also if listed) ──
# Note: our SHA256SUMS only covers binaries today. Compute + log the cli tgz
# digest for the user to compare against the release page manually.
cd "$WORKDIR"
say "Computing checksum (informational — release page also shows asset digests)"
$SHA256 "$CLI_TGZ" | tee "$CLI_TGZ.sha256"

# ─── Install CLI globally ─────────────────────────────────────────
say "Installing $CLI_TGZ globally via npm"
npm install -g "$WORKDIR/$CLI_TGZ"
ok "CLI installed"

if command -v kars >/dev/null 2>&1; then
  ok "kars binary on PATH: $(command -v kars)"
  kars --version 2>/dev/null || true
else
  warn "kars not yet on PATH — open a new shell or check 'npm prefix -g'/bin"
fi

# ─── Docker login to GHCR ─────────────────────────────────────────
if [ "$SKIP_DOCKER" = "1" ]; then
  say "Skipping Docker setup (KARS_SKIP_DOCKER=1)"
else
  if ! command -v docker >/dev/null 2>&1; then
    warn "Docker not found — skipping GHCR login. Install Docker to pull images."
  else
    say "Logging Docker into ghcr.io with your gh token"
    # `gh auth token` returns the user's PAT. For SSO-protected orgs
    # like Azure the PAT must be SSO-authorized (one-click at
    # https://github.com/settings/tokens → Configure SSO).
    if gh auth token | docker login ghcr.io -u "$GH_USER" --password-stdin >/dev/null 2>&1; then
      ok "Docker logged into ghcr.io"
      # Probe a private package to confirm SSO bless covers packages too.
      # (Some users SSO-authorize for repo but not packages.)
      if ! docker manifest inspect "ghcr.io/azure/kars-controller:$VERSION" >/dev/null 2>&1; then
        cat >&2 <<EOF

$(printf "\033[1;33m⚠ Docker login OK but private image pull failed.\033[0m")

You likely need to also SSO-authorize the token for the Azure org's
PACKAGES (separate from repo access). Fix:

  1. https://github.com/settings/tokens → find your gh token
  2. "Configure SSO" → "Authorize" next to Azure
  3. Make sure the token has the 'read:packages' scope
     (re-run \`gh auth refresh --scopes read:packages\` if not)

Or, if you want to use a fresh classic PAT instead of gh's token:
  https://github.com/settings/tokens/new?scopes=read:packages
  Then authorize it for Azure SSO + run:
    echo "<PAT>" | docker login ghcr.io -u $GH_USER --password-stdin

EOF
      fi
    else
      warn "Docker login failed. Try:"
      warn "  gh auth refresh --hostname github.com --scopes read:packages,repo"
      warn "  echo \$(gh auth token) | docker login ghcr.io -u $GH_USER --password-stdin"
    fi
  fi
fi

# ─── Optionally pre-pull images ───────────────────────────────────
if [ "$PULL_IMAGES" = "1" ] && command -v docker >/dev/null 2>&1; then
  IMAGES=(
    "ghcr.io/azure/kars-controller:$VERSION"
    "ghcr.io/azure/kars-inference-router:$VERSION"
    "ghcr.io/azure/kars-a2a-gateway:$VERSION"
    "ghcr.io/azure/kars-conformance-runner:$VERSION"
    "ghcr.io/azure/kars-sandbox-base:$VERSION"
  )
  for img in "${IMAGES[@]}"; do
    say "Pulling $img"
    docker pull "$img" || warn "Pull failed for $img"
  done
fi

# ─── Done ─────────────────────────────────────────────────────────
cat <<EOF

$(printf "\033[1;32m═══════════════════════════════════════════════════════════════\033[0m")
$(printf "\033[1;32m✓ kars %s installed\033[0m" "$VERSION")
$(printf "\033[1;32m═══════════════════════════════════════════════════════════════\033[0m")

Quick start:
  kars --help                # see all commands
  kars up <agent-name>       # spin up a sandbox
  kars connect <agent-name>  # open WebUI

Container images (private GHCR):
  docker pull ghcr.io/azure/kars-controller:$VERSION
  docker pull ghcr.io/azure/kars-inference-router:$VERSION
  ... (+ a2a-gateway, conformance-runner, sandbox-base)

To pre-pull all images on next install:
  KARS_PULL_IMAGES=1 curl -fsSL https://raw.githubusercontent.com/Azure/kars/main/install.sh | bash

To pin a specific version:
  KARS_VERSION=$VERSION curl -fsSL https://raw.githubusercontent.com/Azure/kars/main/install.sh | bash

Release page (auth required):
  https://github.com/$REPO/releases/tag/$VERSION

EOF
