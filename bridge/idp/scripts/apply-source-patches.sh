#!/bin/sh
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

set -eu
packaging="${1:-/packaging}"
patches="$packaging/patches"
(cd "$patches" && sha256sum --check --strict SHA256SUMS)
sha256sum --check --strict --quiet "$packaging/source.upstream.sha256"
sha256sum --check --strict "$patches/upstream.sha256"
while IFS= read -r name; do
    git apply --no-index --check --whitespace=error-all "$patches/$name"
    git apply --no-index --whitespace=error-all "$patches/$name"
done < "$patches/series"
test ! -e server/kars_compat_test.go
test ! -e connector/saml/kars_compat_test.go
cp "$patches/server_compat_test.go" server/kars_compat_test.go
cp "$patches/saml_compat_test.go" connector/saml/kars_compat_test.go
sha256sum --check --strict "$patches/patched.sha256"

# Build the EXPECTED inventory from the verified original plus only the
# reviewed replacement/addition hashes, never from whatever patch produced.
awk 'NR == FNR { replacement[$2] = $1; next }
     { if ($2 in replacement) {
           print replacement[$2] "  " $2; delete replacement[$2]
       } else { print } }
     END { for (name in replacement) print replacement[name] "  " name }' \
    "$patches/patched.sha256" "$packaging/source.upstream.sha256" \
    | LC_ALL=C sort -k2 > "$packaging/source.sha256"
sha256sum --check --strict --quiet "$packaging/source.sha256"
sh "$packaging/scripts/source-inventory.sh" > "$packaging/source.actual.sha256"
cmp "$packaging/source.sha256" "$packaging/source.actual.sha256"
