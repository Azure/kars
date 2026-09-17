#!/bin/sh
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

set -eu
# Dependency manifests are verified separately against the generated Go locks.
find . -type f ! -path './go.mod' ! -path './go.sum' \
    ! -path './api/v2/go.mod' ! -path './api/v2/go.sum' \
    -print0 | LC_ALL=C sort -z | xargs -0 sha256sum
