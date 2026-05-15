#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# Fast-rebuild — incrementally cross-compile a Rust crate for Linux
# inside a builder container (reusing target cache between runs), then
# overlay the fresh binary onto the existing release image. Saves the
# ~4-5min full Docker rebuild we'd otherwise do for a 10-line change.
#
# Usage: scripts/dev/fast-rebuild.sh <controller|router> [--load <kind-cluster>] [--set-image <ns>/<deploy>]
#
# Examples:
#   scripts/dev/fast-rebuild.sh controller --load azureclaw-dev --set-image azureclaw-system/azureclaw-controller
#   scripts/dev/fast-rebuild.sh router     --load azureclaw-dev
set -euo pipefail

case "${1:-}" in
    controller)
        CRATE=azureclaw-controller
        BASE_IMAGE=azureclaw-controller:dev
        OUT_IMAGE=azureclaw-controller
        BIN_PATH=/usr/local/bin/azureclaw-controller
        ;;
    router)
        CRATE=azureclaw-inference-router
        BASE_IMAGE=azureclaw-inference-router:dev
        OUT_IMAGE=azureclaw-inference-router
        BIN_PATH=/usr/local/bin/azureclaw-inference-router
        ;;
    *)
        echo "usage: $0 <controller|router> [--load <kind-cluster>] [--set-image <ns>/<deploy>]" >&2
        exit 1
        ;;
esac
shift

KIND_CLUSTER=""
SET_IMAGE=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --load) KIND_CLUSTER="$2"; shift 2 ;;
        --set-image) SET_IMAGE="$2"; shift 2 ;;
        *) echo "unknown arg: $1" >&2; exit 1 ;;
    esac
done

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")"/../.. && pwd)"
cd "$REPO_ROOT"

# Match the runtime image's arch (kind on Apple Silicon = arm64).
HOST_ARCH=$(docker inspect "$BASE_IMAGE" --format '{{.Architecture}}' 2>/dev/null || echo "amd64")
case "$HOST_ARCH" in
    arm64) RUST_TARGET=aarch64-unknown-linux-gnu; PLATFORM=linux/arm64 ;;
    amd64) RUST_TARGET=x86_64-unknown-linux-gnu;  PLATFORM=linux/amd64 ;;
    *) echo "unsupported arch: $HOST_ARCH" >&2; exit 1 ;;
esac

CACHE_DIR="$REPO_ROOT/target-linux-$HOST_ARCH"
mkdir -p "$CACHE_DIR"

echo "==> Building $CRATE for $RUST_TARGET (cache: $CACHE_DIR)"
docker run --rm --platform "$PLATFORM" \
    -v "$REPO_ROOT":/src:cached \
    -v "$CACHE_DIR":/src/target:delegated \
    -w /src \
    rust:1.88 \
    bash -c "apt-get update -qq && apt-get install -y -qq pkg-config libssl-dev >/dev/null && cargo build --release --package $CRATE" 2>&1 | tail -10

BIN="$CACHE_DIR/release/$CRATE"
test -x "$BIN" || { echo "FAIL: $BIN not found"; exit 1; }
ls -la "$BIN"

TAG="dev-$(date +%s)"
echo "==> Overlaying onto $BASE_IMAGE → $OUT_IMAGE:$TAG"
WORK=$(mktemp -d)
cp "$BIN" "$WORK/$CRATE"
cat > "$WORK/Dockerfile" <<EOF
FROM $BASE_IMAGE
COPY $CRATE $BIN_PATH
EOF
docker build --platform "$PLATFORM" -t "$OUT_IMAGE:$TAG" "$WORK" 2>&1 | tail -3
rm -rf "$WORK"

if [[ -n "$KIND_CLUSTER" ]]; then
    echo "==> Loading $OUT_IMAGE:$TAG into kind cluster $KIND_CLUSTER"
    kind load docker-image "$OUT_IMAGE:$TAG" --name "$KIND_CLUSTER" 2>&1 | tail -2
fi

if [[ -n "$SET_IMAGE" ]]; then
    NS="${SET_IMAGE%%/*}"
    DEPLOY="${SET_IMAGE#*/}"
    # Try common container names
    for CN in controller inference-router; do
        if kubectl set image -n "$NS" "deploy/$DEPLOY" "$CN=$OUT_IMAGE:$TAG" --record=false 2>/dev/null; then
            echo "==> Set $NS/$DEPLOY container=$CN to $OUT_IMAGE:$TAG"
            break
        fi
    done
    kubectl rollout status -n "$NS" "deploy/$DEPLOY" --timeout=90s || true
fi

echo "==> Done: $OUT_IMAGE:$TAG"
