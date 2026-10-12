#!/bin/sh
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

set -eu
test "$(go env GOVERSION)" = go1.27.2
case "$(go env GOARCH)" in
    amd64|arm64) ;;
    *) echo "Node exporter requires a native amd64 or arm64 builder" >&2; exit 1 ;;
esac
export CGO_ENABLED=0 GOMAXPROCS=2 GOMEMLIMIT=3GiB GOWORK=off
cd "${1:?build directory is required}"
build="$(pwd)"
mkdir source
tar --no-same-owner -xzf source.tar.gz --strip-components=1 -C source
cd source
test "$(cat VERSION)" = 1.11.1
cp "$build/node-exporter.go.mod" go.mod
cp "$build/node-exporter.go.sum" go.sum
go mod download
go mod verify
bash ./ttar -C collector/fixtures -x -f collector/fixtures/sys.ttar
bash ./ttar -C collector/fixtures -x -f collector/fixtures/udev.ttar
go test -mod=readonly -p=2 -short -count=1 ./...
go build -mod=readonly -trimpath -buildvcs=false -p=2 -tags=netgo \
    -ldflags="-s -w -X github.com/prometheus/common/version.Version=1.11.1 -X github.com/prometheus/common/version.Revision=0dd664dece3f8319f6bec5a221acd2c7ad13a23d -X github.com/prometheus/common/version.Branch=kars-security-backport -X github.com/prometheus/common/version.BuildUser=kars -X github.com/prometheus/common/version.BuildDate=20261010" \
    -o "$build/node_exporter" .
go version -m "$build/node_exporter" > "$build/node-exporter.buildinfo"
"$build/node_exporter" --version
chmod 0555 "$build/node_exporter"
cmp "$build/node-exporter.go.mod" go.mod
cmp "$build/node-exporter.go.sum" go.sum
mkdir "$build/licenses"
chmod 0755 "$build/licenses"
cp LICENSE NOTICE "$build/licenses/"
chmod 0444 "$build/licenses/LICENSE" "$build/licenses/NOTICE"
