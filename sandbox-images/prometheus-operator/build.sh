#!/bin/sh
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

set -eu
test "$(go env GOVERSION)" = go1.27.2
case "$(go env GOARCH)" in
    amd64|arm64) ;;
    *) echo "Prometheus Operator requires a native amd64 or arm64 builder" >&2; exit 1 ;;
esac
export CGO_ENABLED=0 GOMAXPROCS=2 GOMEMLIMIT=3GiB GOWORK=off
cd "${1:?build directory is required}"
build="$(pwd)"
mkdir source
tar --no-same-owner -xzf source.tar.gz --strip-components=1 -C source
cd source
test "$(cat VERSION)" = 0.90.1
test "$(grep -Fc 'parsedExpr, err := parser.ParseExpr(expr)' pkg/namespacelabeler/labeler.go)" = 1
sed -i 's/parser\.ParseExpr(expr)/parser.NewParser(parser.Options{}).ParseExpr(expr)/' pkg/namespacelabeler/labeler.go
test "$(grep -Fc 'parsedExpr, err := parser.NewParser(parser.Options{}).ParseExpr(expr)' pkg/namespacelabeler/labeler.go)" = 1
test "$(grep -Fc 'rulefmt.Parse(content, false, validationScheme)' pkg/operator/rules.go)" = 1
test "$(grep -Fc '"github.com/prometheus/prometheus/model/rulefmt"' pkg/operator/rules.go)" = 1
sed -i 's|"github.com/prometheus/prometheus/model/rulefmt"|"github.com/prometheus/prometheus/model/rulefmt"\n\t"github.com/prometheus/prometheus/promql/parser"|' pkg/operator/rules.go
sed -i 's/rulefmt.Parse(content, false, validationScheme)/rulefmt.Parse(content, false, validationScheme, parser.NewParser(parser.Options{}))/' pkg/operator/rules.go
test "$(grep -Fc 'rulefmt.Parse(content, false, validationScheme, parser.NewParser(parser.Options{}))' pkg/operator/rules.go)" = 1
cp "$build/operator.go.mod" go.mod
cp "$build/operator.go.sum" go.sum
go mod download
go mod verify
go test -mod=readonly -p=2 -short -count=1 \
    ./pkg/operator ./pkg/prometheus/... ./pkg/namespacelabeler ./pkg/alertmanager/... ./cmd/operator ./cmd/prometheus-config-reloader
ldflags="-s -w -X github.com/prometheus/common/version.Version=0.90.1 -X github.com/prometheus/common/version.Revision=32d1b3dfa05d070762450efe9624bb2483c782be -X github.com/prometheus/common/version.Branch=kars-security-backport -X github.com/prometheus/common/version.BuildUser=kars -X github.com/prometheus/common/version.BuildDate=20261010"
for binary in operator prometheus-config-reloader; do
    go build -mod=readonly -trimpath -buildvcs=false -p=2 -tags=netgo \
        -ldflags="$ldflags" -o "$build/$binary" "./cmd/$binary"
    go version -m "$build/$binary" > "$build/$binary.buildinfo"
    "$build/$binary" --version
    chmod 0555 "$build/$binary"
done
cmp "$build/operator.go.mod" go.mod
cmp "$build/operator.go.sum" go.sum
mkdir "$build/licenses"
chmod 0755 "$build/licenses"
cp LICENSE "$build/licenses/"
chmod 0444 "$build/licenses/LICENSE"
