#!/bin/sh
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

set -eu
test "$(go env GOVERSION)" = go1.27.2
case "$(go env GOARCH)" in
    amd64|arm64) ;;
    *) echo "kube-state-metrics requires a native amd64 or arm64 builder" >&2; exit 1 ;;
esac
export CGO_ENABLED=0 GOMAXPROCS=2 GOMEMLIMIT=3GiB GOWORK=off
cd "${1:?build directory is required}"
build="$(pwd)"
mkdir source
tar --no-same-owner -xzf source.tar.gz --strip-components=1 -C source
cd source
grep -Fx 'module k8s.io/kube-state-metrics/v2' go.mod
cp "$build/metrics.go.mod" go.mod
cp "$build/metrics.go.sum" go.sum
go mod download
go mod verify
go list -mod=readonly ./... > "$build/packages.txt"
grep -v '/tests/e2e$' "$build/packages.txt" > "$build/unit-packages.txt"
xargs go test -mod=readonly -p=2 -short -count=1 < "$build/unit-packages.txt"
client_version="$(go list -mod=readonly -m -f '{{.Version}}' k8s.io/client-go)"
go build -mod=readonly -trimpath -buildvcs=false -p=2 \
    -ldflags="-s -w -X github.com/prometheus/common/version.Version=2.19.0 -X github.com/prometheus/common/version.Revision=2ef24549bff9865db214ccb03cc36f1fc214d1be -X github.com/prometheus/common/version.Branch=HEAD -X k8s.io/kube-state-metrics/v2/pkg/app.ClientGoVersion=$client_version" \
    -o "$build/kube-state-metrics" .
go version -m "$build/kube-state-metrics" > "$build/metrics.buildinfo"
"$build/kube-state-metrics" version
chmod 0555 "$build/kube-state-metrics"
cmp "$build/metrics.go.mod" go.mod
cmp "$build/metrics.go.sum" go.sum
mkdir "$build/licenses"
chmod 0755 "$build/licenses"
cp LICENSE "$build/licenses/"
chmod 0444 "$build/licenses/LICENSE"
