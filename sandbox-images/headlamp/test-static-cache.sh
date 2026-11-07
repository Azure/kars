#!/bin/sh
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

set -eu
cd "$(dirname "$0")"
work="$PWD/.static-cache-tests-$$"
mkdir -m 700 "$work"
trap 'rm -rf -- "$work"' 0
trap 'exit 1' HUP INT TERM
export TMPDIR="$work" GOTMPDIR="$work" GOCACHE="$work/go-cache"
export GOTOOLCHAIN=local GOPROXY=off GOSUMDB=off GOWORK=off
# Explicit files keep this stdlib-only check separate from upstream dependencies.
go test -count=1 -v static-cache.go static-cache_test.go patch-backend.go patch-backend_test.go oidc-navigation.go oidc-navigation_test.go
