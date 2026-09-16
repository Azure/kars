#!/bin/sh
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

set -eu
. /packaging/locks/inputs.lock
(cd /locks && sha256sum --check --strict SHA256SUMS)
cmp /packaging/locks/inputs.lock /locks/inputs.lock
cmp /packaging/locks/requests.txt /locks/requests.txt
test "$(cat /locks/toolchain.txt)" = "go version $GO_VERSION linux/$(go env GOARCH)"
for file in go.mod go.sum api/v2/go.mod api/v2/go.sum; do
    cmp "/packaging/upstream/$file" "/locks/upstream/$file"
    cp "/locks/$file" "$file"
done
test "$(go list -m -f '{{.GoVersion}}')" = 1.26.0
replacements="$(go list -m -f '{{if .Replace}}{{.Path}} => {{.Replace.Path}}{{end}}' all)"
test "$(printf '%s\n' "$replacements" | sed '/^$/d')" = 'github.com/dexidp/dex/api/v2 => ./api/v2'
while IFS=@ read -r module version; do
    # Exact requests are the approved selection, not merely scanner floors.
    test "$(go list -m -f '{{.Version}}' "$module")" = "$version"
done < /packaging/locks/requests.txt
(
    cd api/v2
    test "$(go list -m -f '{{.GoVersion}}')" = 1.26.0
    test "$(go list -m -f '{{.Version}}' google.golang.org/grpc)" = v1.83.2
    replacements="$(go list -m -f '{{if .Replace}}{{.Path}}{{end}}' all)"
    test -z "$(printf '%s\n' "$replacements" | sed '/^$/d')"
)
sha256sum --check --strict /packaging/source.sha256 > /dev/null
