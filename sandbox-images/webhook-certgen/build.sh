#!/bin/sh
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

set -eu
test "$(go env GOVERSION)" = go1.27.2
case "$(go env GOARCH)" in
    amd64|arm64) ;;
    *) echo "Webhook certgen requires a native amd64 or arm64 builder" >&2; exit 1 ;;
esac
export CGO_ENABLED=0 GOMAXPROCS=2 GOMEMLIMIT=3GiB GOWORK=off
cd "${1:?build directory is required}"
build="$(pwd)"
mkdir source
tar --no-same-owner -xzf source.tar.gz --strip-components=1 -C source
cd source
grep -Fx 'module github.com/jkroepke/kube-webhook-certgen' go.mod
cp "$build/certgen.go.mod" go.mod
cp "$build/certgen.go.sum" go.sum
go mod download
go mod verify
go test -mod=readonly -p=2 -short -count=1 ./...
go build -mod=readonly -trimpath -buildvcs=false -p=2 \
    -ldflags="-s -w -X github.com/jkroepke/kube-webhook-certgen/core.Version=1.8.3 -X github.com/jkroepke/kube-webhook-certgen/core.BuildTime=2026-05-21T22:38:40Z" \
    -o "$build/kube-webhook-certgen" .
go version -m "$build/kube-webhook-certgen" > "$build/certgen.buildinfo"
"$build/kube-webhook-certgen" --help
"$build/kube-webhook-certgen" version | grep -Fx '1.8.3'
chmod 0555 "$build/kube-webhook-certgen"
cmp "$build/certgen.go.mod" go.mod
cmp "$build/certgen.go.sum" go.sum
mkdir "$build/licenses"
chmod 0755 "$build/licenses"
cp LICENSE "$build/licenses/"
chmod 0444 "$build/licenses/LICENSE"
