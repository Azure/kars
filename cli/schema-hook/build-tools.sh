#!/bin/sh
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

set -eu
test "$(go env GOVERSION)" = go1.27.2
case "$(go env GOARCH)" in
    amd64|arm64) ;;
    *) echo "Schema tools require native amd64 or arm64" >&2; exit 1 ;;
esac
export CGO_ENABLED=0 GOMAXPROCS=2 GOMEMLIMIT=3GiB GOWORK=off
cd "${1:?tool build directory is required}"
tools="$(pwd)"
mkdir helm-source kubectl-source
tar --no-same-owner -xzf helm.tar.gz --strip-components=1 -C helm-source
tar --no-same-owner -xzf kubernetes.tar.gz --strip-components=1 -C kubectl-source

cd "$tools/helm-source"
go mod edit -require=golang.org/x/net@v0.60.0
go build -mod=mod -trimpath -buildvcs=false -p=2 \
    -ldflags="-s -w -X helm.sh/helm/v4/internal/version.version=v4.3.0 -X helm.sh/helm/v4/internal/version.metadata=kars.1 -X helm.sh/helm/v4/internal/version.gitCommit=bec5b06ed841fe5269972d864d5177944fd5970f -X helm.sh/helm/v4/internal/version.gitTreeState=dirty" \
    -o "$tools/helm" ./cmd/helm
go test -mod=mod -p=2 ./internal/version
cp LICENSE "$tools/helm.LICENSE"

cd "$tools/kubectl-source"
go mod edit -require=golang.org/x/net@v0.60.0
go build -mod=mod -trimpath -buildvcs=false -p=2 \
    -ldflags="-s -w -X k8s.io/component-base/version.gitVersion=v1.35.9+kars.1 -X k8s.io/component-base/version.gitMajor=1 -X k8s.io/component-base/version.gitMinor=35 -X k8s.io/component-base/version.gitCommit=329000efd0b26bd78155e51e4df449130f0fca00 -X k8s.io/component-base/version.gitTreeState=dirty" \
    -o "$tools/kubectl" ./cmd/kubectl
go test -mod=mod -p=2 k8s.io/kubectl/pkg/cmd/version
cp LICENSE "$tools/kubectl.LICENSE"
chmod 0555 "$tools/helm" "$tools/kubectl"
go version -m "$tools/helm" "$tools/kubectl"
