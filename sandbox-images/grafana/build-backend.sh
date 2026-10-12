#!/bin/sh
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
set -eu
out=$1
inputs=$2
mkdir -p "$out/source"
tar --no-same-owner -xzf "$inputs/source.tar.gz" --strip-components=1 -C "$out/source"
cd "$out/source"
export CGO_ENABLED=0 GOMAXPROCS=2 GOMEMLIMIT=3GiB GOGC=50
export GOPROXY=https://proxy.golang.org GOSUMDB=sum.golang.org GONOSUMDB= GONOPROXY= GOPRIVATE=
test "$(go env GOVERSION)" = go1.27.2
go work edit -replace=golang.org/x/net=golang.org/x/net@v0.60.0
go work edit -replace=github.com/grafana/tempo=github.com/grafana/tempo@v1.5.1-0.20260317172644-4aeafc237b8d
go work edit -replace=github.com/prometheus/prometheus=github.com/prometheus/prometheus@v0.312.1-0.20260612131846-2ad3a8717015
go work edit -replace=github.com/grafana/loki/v3=github.com/grafana/loki/v3@v3.6.17
go work edit -replace=github.com/grafana/grafana-prometheus-datasource/pkg/promlib=github.com/grafana/grafana-prometheus-datasource/pkg/promlib@v0.0.11
# Retain Grafana's query-schema API instead of taking promlib's unrelated SDK bump.
go work edit -replace=github.com/grafana/grafana-plugin-sdk-go=github.com/grafana/grafana-plugin-sdk-go@v0.291.0
python3 -B "$inputs/patch-parser.py" "$out/source"
go test -p 1 -trimpath -buildvcs=false -gcflags=all=-dwarf=false -tags=oss \
  ./pkg/tsdb/tempo ./pkg/tsdb/prometheus ./pkg/tsdb/loki \
  ./pkg/tsdb/grafana-pyroscope-datasource ./apps/dashvalidator/pkg/validator/prometheus \
  github.com/grafana/grafana-prometheus-datasource/pkg/promlib/models \
  ./pkg/registry/apis/query/queryschema -count=1
go mod verify
go clean -cache
GOCACHEPROG="python3 -B $inputs/no-cache.py" go build -p 2 -trimpath -buildvcs=false -gcflags=all=-dwarf=false -tags=oss \
  -ldflags="-s -w -X main.version=13.0.10 -X main.commit=952c9f4a26f5fa9be8dd49e979328966e8f1ee24 -X main.buildBranch=release-13.0.10" \
  -o "$out/grafana" ./pkg/cmd/grafana
go version -m "$out/grafana" > "$out/grafana.buildinfo"
"$out/grafana" server -v
go mod verify
cp go.work "$out/go.work"
cp LICENSE "$out/LICENSE"
find . -type f \( -name go.mod -o -name go.sum -o -name go.work -o -name go.work.sum \) -print0 \
  | tar --null -T - -czf "$out/module-locks.tar.gz"
