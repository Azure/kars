#!/bin/sh
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
set -eu

case "${1:-}" in
    amd64) triple=x86_64-linux-gnu; libc=libc6-dev-amd64-cross; machine='Advanced Micro Devices X86-64' ;;
    arm64) triple=aarch64-linux-gnu; libc=libc6-dev-arm64-cross; machine=AArch64 ;;
    *) echo "Unsupported Go tools target architecture: ${1:-missing}" >&2; exit 1 ;;
esac
: "${2:?Go tools output directory is required}"
export GOOS=linux GOARCH="$1" GOTOOLCHAIN=auto GOMAXPROCS=2
unset GOBIN
host="$(go env GOHOSTOS)/$(go env GOHOSTARCH)"
compiler="gcc-${triple}"
CC="${triple}-gcc"
if [ "$host" = "$GOOS/$GOARCH" ]; then
    compiler=gcc
    libc=libc6-dev
    CC=gcc
fi
export CC
apt-get update
apt-get install -y --no-install-recommends "$compiler" "$libc" binutils ca-certificates git
rm -rf /var/lib/apt/lists/*

for package in \
    github.com/steipete/blucli/cmd/blu \
    github.com/steipete/eightctl/cmd/eightctl \
    github.com/steipete/gifgrep/cmd/gifgrep \
    github.com/steipete/ordercli/cmd/ordercli \
    github.com/steipete/sonoscli/cmd/sonos \
    github.com/steipete/camsnap/cmd/camsnap \
    github.com/steipete/goplaces/cmd/goplaces \
    github.com/steipete/songsee/cmd/songsee; do
    CGO_ENABLED=0 go install -p 2 "${package}@latest"
done

# go-sqlite3 compiles its bundled SQLite with the target C compiler. Pure-Go
# DNS/user lookups avoid glibc NSS dependencies in the otherwise static binary.
CGO_ENABLED=1 go install -p 2 -tags "sqlite_omit_load_extension,netgo,osusergo" \
    -ldflags='-linkmode external -extldflags "-static"' \
    github.com/openclaw/wacli/cmd/wacli@latest

bin_dir="$(go env GOPATH)/bin"
if [ "$host" != "$GOOS/$GOARCH" ]; then
    bin_dir="${bin_dir}/${GOOS}_${GOARCH}"
fi
mkdir -p "$2"
for tool in blu eightctl gifgrep ordercli sonos camsnap goplaces songsee wacli; do
    binary="$bin_dir/$tool"
    test -x "$binary"
    header=$(LC_ALL=C readelf -h "$binary")
    printf '%s\n' "$header" | grep -F "Machine:" | grep -F "$machine" >/dev/null
    program_headers=$(LC_ALL=C readelf -l "$binary")
    dynamic=$(LC_ALL=C readelf -d "$binary")
    if printf '%s\n%s\n' "$program_headers" "$dynamic" | grep -Eq 'INTERP|\(NEEDED\)'; then
        echo "Go tool is not statically linked: $tool" >&2
        exit 1
    fi
    cp "$binary" "$2/$tool"
done
