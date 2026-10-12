#!/bin/sh
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

set -eu
test "$(go env GOVERSION)" = go1.27.2
case "$(go env GOARCH)" in
    amd64|arm64) ;;
    *) echo "Prometheus requires a native amd64 or arm64 builder" >&2; exit 1 ;;
esac
export CGO_ENABLED=0 GOMAXPROCS=2 GOMEMLIMIT=3GiB GOWORK=off USER=prometheus
cd "${1:?build directory is required}"
build="$(pwd)"
mkdir source data
chmod 0770 data
tar --no-same-owner -xzf source.tar.gz --strip-components=1 -C source
cd source
test "$(cat VERSION)" = 3.11.3
git apply --check --exclude=go.mod --exclude=go.sum "$build/moby.patch"
git apply --exclude=go.mod --exclude=go.sum "$build/moby.patch"
git apply --check "$build/moby-api.patch"
git apply "$build/moby-api.patch"
cp "$build/prometheus.go.mod" go.mod
cp "$build/prometheus.go.sum" go.sum
tar --no-same-owner -xzf "$build/ui.tar.gz" -C web/ui
test -s web/ui/static/react-app/index.html
test -s web/ui/static/mantine-ui/index.html
bash scripts/compress_assets.sh
go test -mod=readonly -p=2 -count=1 ./discovery/moby ./config
ldflags="-s -w -X github.com/prometheus/common/version.Version=3.11.3 -X github.com/prometheus/common/version.Revision=eb173f5256d4022afba1e9bc3d19740a76859fae -X github.com/prometheus/common/version.Branch=kars-security-backport -X github.com/prometheus/common/version.BuildUser=kars -X github.com/prometheus/common/version.BuildDate=20261010"
for binary in prometheus promtool; do
    go build -mod=readonly -trimpath -buildvcs=false -p=2 -tags=netgo,builtinassets \
        -ldflags="$ldflags" -o "$build/$binary" "./cmd/$binary"
    go version -m "$build/$binary" > "$build/$binary.buildinfo"
    if grep -F 'github.com/docker/docker' "$build/$binary.buildinfo"; then
        echo "Legacy Docker daemon module remains in binary" >&2
        exit 1
    fi
    "$build/$binary" --version
    chmod 0555 "$build/$binary"
done
cp LICENSE NOTICE "$build/"
cp documentation/examples/prometheus.yml "$build/"
