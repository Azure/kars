#!/bin/sh
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

set -eu
test "$#" -eq 3
expected="$1"
# The resolver's architecture is provenance, not the native build target.
for toolchain in "$2" "$3"; do
    case "$toolchain" in
        "go version $expected linux/amd64"|"go version $expected linux/arm64") ;;
        *) printf 'Unsupported or unpinned Go toolchain: %s\n' "$toolchain" >&2; exit 1 ;;
    esac
done
