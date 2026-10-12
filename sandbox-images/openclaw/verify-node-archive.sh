#!/bin/sh
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
set -eu

if [ "$#" -ne 2 ]; then
    echo "Usage: verify-node-archive.sh ARCHIVE CHECKSUM_MANIFEST" >&2
    exit 2
fi

archive=$1
manifest=$2
name=${archive##*/}
expected=
while read -r digest filename; do
    if [ "$filename" = "$name" ]; then
        if [ -n "$expected" ]; then
            echo "Duplicate Node.js archive checksum: $name" >&2
            exit 1
        fi
        expected=$digest
    fi
done < "$manifest"

if [ -z "$expected" ]; then
    echo "Missing pinned Node.js archive checksum: $name" >&2
    exit 1
fi

printf '%s  %s\n' "$expected" "$archive" | sha256sum --check --strict -
