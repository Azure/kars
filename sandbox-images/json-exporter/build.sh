#!/bin/sh
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

set -eu
test "$(go env GOVERSION)" = go1.27.2
case "$(go env GOARCH)" in
    amd64|arm64) ;;
    *) echo "JSON exporter requires a native amd64 or arm64 builder" >&2; exit 1 ;;
esac
export CGO_ENABLED=0 GOMAXPROCS=2 GOMEMLIMIT=3GiB GOWORK=off
cd "${1:?build directory is required}"
build="$(pwd)"
mkdir source
tar --no-same-owner -xzf source.tar.gz --strip-components=1 -C source
cd source
grep -Fx 'module github.com/prometheus-community/json_exporter' go.mod
cp "$build/json-exporter.go.mod" go.mod
cp "$build/json-exporter.go.sum" go.sum
go mod download
go mod verify
go test -mod=readonly -p=2 -short -count=1 ./...
go build -mod=readonly -trimpath -buildvcs=false -p=2 \
    -ldflags="-s -w -X github.com/prometheus/common/version.Version=0.7.0 -X github.com/prometheus/common/version.Revision=06fb506a4c5d242186f198b9c8bf072212f3a134 -X github.com/prometheus/common/version.Branch=HEAD" \
    -o "$build/json_exporter" .
go version -m "$build/json_exporter" > "$build/json-exporter.buildinfo"
"$build/json_exporter" --version
chmod 0555 "$build/json_exporter"
cmp "$build/json-exporter.go.mod" go.mod
cmp "$build/json-exporter.go.sum" go.sum
mkdir "$build/licenses"
chmod 0755 "$build/licenses"
cp LICENSE "$build/licenses/"
chmod 0444 "$build/licenses/LICENSE"
