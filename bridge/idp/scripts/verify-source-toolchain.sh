#!/bin/sh
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

set -eu
test "$#" -ge 2
compiler="$1"
shift
# Inspect source directives before Go can resolve modules or rewrite manifests.
for manifest in "$@"; do
    awk -v compiler="$compiler" '
        $1 == "go" { required = $2; count++ }
        END {
            if (count != 1 || compiler !~ /^go[0-9]+\.[0-9]+\.[0-9]+$/ ||
                required !~ /^[0-9]+\.[0-9]+(\.[0-9]+)?$/) {
                print "Invalid source/toolchain directive: " FILENAME > "/dev/stderr"
                exit 1
            }
            split(substr(compiler, 3), actual, ".")
            split(required, minimum, ".")
            for (i = 1; i <= 3; i++) {
                if (actual[i] + 0 > minimum[i] + 0) exit 0
                if (actual[i] + 0 < minimum[i] + 0) {
                    print FILENAME " requires Go " required "; pinned compiler is " compiler ". Refusing to lower upstream requirements." > "/dev/stderr"
                    exit 1
                }
            }
        }
    ' "$manifest"
done
