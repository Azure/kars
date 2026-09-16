#!/bin/sh
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

set -eu
. /packaging/locks/inputs.lock
test "$(go env GOVERSION)" = "$GO_VERSION"
curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
    "https://codeload.github.com/dexidp/dex/tar.gz/$DEX_COMMIT" -o /tmp/dex.tar.gz
printf '%s  %s\n' "$DEX_ARCHIVE_SHA256" /tmp/dex.tar.gz | sha256sum --check --strict
tar -xzf /tmp/dex.tar.gz --strip-components=1 -C /src/dex
rm /tmp/dex.tar.gz
sh /packaging/scripts/source-inventory.sh > /packaging/source.upstream.sha256
mkdir -p /packaging/upstream/api/v2
cp go.mod go.sum /packaging/upstream/
cp api/v2/go.mod api/v2/go.sum /packaging/upstream/api/v2/
sh /packaging/scripts/apply-source-patches.sh
