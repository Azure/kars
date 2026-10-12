#!/bin/sh
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

set -eu
test "$(go env GOVERSION)" = go1.27.2
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
go mod edit \
    -require=github.com/containerd/containerd@v1.7.33 \
    -require=github.com/go-jose/go-jose/v4@v4.1.4 \
    -require=github.com/moby/spdystream@v0.5.1 \
    -require=go.opentelemetry.io/otel/sdk@v1.43.0 \
    -require=golang.org/x/crypto@v0.55.0 \
    -require=golang.org/x/net@v0.60.0 \
    -require=golang.org/x/text@v0.39.0 \
    -require=google.golang.org/grpc@v1.83.2 \
    -require=oras.land/oras-go/v2@v2.6.2
go build -mod=mod -trimpath -buildvcs=false -p=2 -ldflags="-s -w" \
    -o "$build/headlamp-server" ./cmd/
go test -mod=mod -p=2 ./cmd ./pkg/config ./pkg/plugins
cp go.mod "$build/headlamp.go.mod"
cp go.sum "$build/headlamp.go.sum"
chmod 0555 "$build/headlamp-server"
go version -m "$build/headlamp-server"
