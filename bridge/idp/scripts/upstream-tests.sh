#!/bin/sh
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

set -eu
if ! go test -json -count=1 -race ./... > /out/doc/upstream-tests.json; then
    cat /out/doc/upstream-tests.json
    exit 1
fi
cd api/v2
if ! go test -json -count=1 -race ./... > /out/doc/api-tests.json; then
    cat /out/doc/api-tests.json
    exit 1
fi
