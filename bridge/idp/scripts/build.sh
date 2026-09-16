#!/bin/sh
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

set -eu
. /packaging/locks/inputs.lock
export SOURCE_DATE_EPOCH
mkdir -p /out/doc /out/etc /out/data
sha256sum --check --strict /packaging/source.sha256 > /dev/null
# No CGO disablement, custom build tags, static libc or copied library closure.
go build -trimpath -buildvcs=false -ldflags="-w -buildid= -X main.version=$DEX_VERSION" \
    -o /out/dex ./cmd/dex
go version -m /out/dex > /out/doc/build-info.txt
readelf --wide --dynamic /out/dex > /out/doc/elf-dynamic.txt
readelf --wide --program-headers /out/dex > /out/doc/elf-program-headers.txt
readelf --wide --version-info /out/dex > /out/doc/elf-versions.txt
grep -q 'CGO_ENABLED=1' /out/doc/build-info.txt
cp LICENSE /out/doc/DEX-LICENSE
if test -f NOTICE; then cp NOTICE /out/doc/DEX-NOTICE; fi
cp /packaging/NOTICE /out/doc/KARS-NOTICE
cp /packaging/LICENSE /out/doc/KARS-LICENSE
cp /packaging/README.md /out/doc/PACKAGING-README.md
cp -R /locks /out/doc/locks
cp -R /packaging/patches /out/doc/source-patches
cp /packaging/source.upstream.sha256 /packaging/source.sha256 /out/doc/
go list -deps -json=ImportPath,Module ./cmd/dex > /out/doc/runtime-packages.json
go run /packaging/scripts/notices.go /out/doc/runtime-packages.json /out/doc/third-party
sha256sum --check --strict /packaging/source.sha256 > /dev/null
cmp go.mod /locks/go.mod
cmp go.sum /locks/go.sum
cmp api/v2/go.mod /locks/api/v2/go.mod
cmp api/v2/go.sum /locks/api/v2/go.sum
