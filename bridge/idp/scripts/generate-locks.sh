#!/bin/sh
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

set -eu
. /packaging/locks/inputs.lock
sh /packaging/scripts/verify-source-toolchain.sh "$GO_VERSION" go.mod api/v2/go.mod
export GOFLAGS=-mod=mod
mkdir -p /out/api/v2

# Use the same reviewed crypto request in both modules, including maintenance candidates.
crypto_request=$(grep '^golang.org/x/crypto@' /packaging/locks/requests.txt)
test "$(printf '%s\n' "$crypto_request" | wc -l)" -eq 1
# Update the nested API as well as the application; the local replace is retained.
(
    cd api/v2
    go mod edit -go=1.26.0 -toolchain="$GO_VERSION"
    go get google.golang.org/grpc@v1.84.0 "$crypto_request" golang.org/x/net@v0.62.0
    go mod tidy
    # The API imports no crypto packages: tidy drops this policy pin, so retain it
    # after pruning rather than falling back to x/net's older module requirement.
    go get "$crypto_request"
    go mod download
    go mod verify
    go list -m -json all > /out/api-modules.json
)
# Keep the released root module's Go requirement; never force it downward.
go mod edit -toolchain="$GO_VERSION"
set --
while IFS= read -r request; do
    test -n "$request"
    set -- "$@" "$request"
done < /packaging/locks/requests.txt
go get "$@"
go mod tidy
go mod download
go mod verify
sha256sum --check --strict /packaging/source.sha256 > /dev/null
go list -m -json all > /out/modules.json
go mod graph > /out/graph.txt
go version > /out/toolchain.txt
cp go.mod go.sum /out/
cp api/v2/go.mod api/v2/go.sum /out/api/v2/
cp /packaging/locks/inputs.lock /packaging/locks/requests.txt /out/
cp -R /packaging/upstream /out/upstream
: > /out/dependencies.patch
for file in go.mod go.sum api/v2/go.mod api/v2/go.sum; do
    set +e
    diff -u --label "upstream/$file" --label "kars/$file" \
        "/packaging/upstream/$file.snapshot" "/out/$file" >> /out/dependencies.patch
    status=$?
    set -e
    test "$status" -le 1
done
(
    cd /out
    find . -type f ! -name SHA256SUMS -print0 | LC_ALL=C sort -z \
        | xargs -0 sha256sum > SHA256SUMS
)
# ACR --no-push runs can return this public artifact via their build log.
tar --sort=name --mtime="@$SOURCE_DATE_EPOCH" --owner=0 --group=0 \
    -C /out -cf /work/dex-locks.tar .
gzip -n /work/dex-locks.tar
printf '\nKARS_DEX_LOCKS_BASE64_BEGIN\n'
base64 -w 0 /work/dex-locks.tar.gz
printf '\nKARS_DEX_LOCKS_BASE64_END\n'
sha256sum /work/dex-locks.tar.gz
