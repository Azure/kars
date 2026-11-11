#!/bin/sh
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

set -eu
recipe="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
test "$(go env GOVERSION)" = go1.27.4
case "$(go env GOARCH)" in
    amd64|arm64) ;;
    *) echo "Headlamp requires a native amd64 or arm64 builder" >&2; exit 1 ;;
esac
export CGO_ENABLED=0 GOMAXPROCS=2 GOMEMLIMIT=3GiB GOWORK=off USER="${USER:-headlamp}"
cd "${1:?build directory is required}"
build="$(pwd)"
mkdir source
tar --no-same-owner -xzf source.tar.gz --strip-components=2 -C source \
    headlamp-066c5be9deb14b12bcef3485b531c64a84a2a219/backend
cd source
HEADLAMP_UPSTREAM_SOURCE="$PWD/cmd/headlamp.go" go test "$recipe/patch-backend.go" "$recipe/patch-backend_test.go"
go run "$recipe/patch-backend.go" "$PWD"
cp "$recipe/static-cache.go" cmd/kars_static_cache.go
cp "$recipe/static-cache_test.go" cmd/kars_static_cache_test.go
cp "$recipe/oidc-navigation.go" cmd/kars_oidc_navigation.go
cp "$recipe/oidc-navigation_test.go" cmd/kars_oidc_navigation_test.go
go mod edit \
    -require=github.com/containerd/containerd@v1.7.36 \
    -require=github.com/go-jose/go-jose/v4@v4.1.6 \
    -require=github.com/klauspost/compress@v1.18.7 \
    -require=github.com/moby/spdystream@v0.5.1 \
    -require=go.opentelemetry.io/otel/sdk@v1.45.0 \
    -require=go.opentelemetry.io/otel/exporters/otlp/otlptrace@v1.45.0 \
    -require=go.opentelemetry.io/otel/exporters/otlp/otlptrace/otlptracegrpc@v1.45.0 \
    -require=go.opentelemetry.io/otel/exporters/otlp/otlptrace/otlptracehttp@v1.45.0 \
    -require=golang.org/x/crypto@v0.58.1-0.20261009181718-2f1f834330b2 \
    -require=golang.org/x/net@v0.62.0 \
    -require=golang.org/x/text@v0.39.0 \
    -require=google.golang.org/grpc@v1.83.2 \
    -require=helm.sh/helm/v3@v3.21.4 \
    -require=oras.land/oras-go/v2@v2.6.2
go list -mod=mod -deps ./cmd/ > "$build/headlamp.deps.txt"
if grep -Eq '^golang.org/x/crypto/openpgp(/|$)' "$build/headlamp.deps.txt"; then
    echo "Deprecated OpenPGP remains in the Headlamp server dependency graph" >&2
    exit 1
fi
go build -mod=mod -trimpath -buildvcs=false -p=2 \
    -ldflags="-s -w -X github.com/kubernetes-sigs/headlamp/backend/pkg/kubeconfig.Version=0.41.0" \
    -o "$build/headlamp-server" ./cmd/
test "$("$build/headlamp-server" -version)" = "Headlamp 0.41.0 (linux/$(go env GOARCH))"
go test -mod=mod -p=2 ./cmd ./pkg/config ./pkg/plugins ./pkg/kubeconfig ./pkg/helm ./pkg/telemetry helm.sh/helm/v3/pkg/provenance
cp go.mod "$build/headlamp.go.mod"
cp go.sum "$build/headlamp.go.sum"
chmod 0555 "$build/headlamp-server"
go version -m "$build/headlamp-server"
