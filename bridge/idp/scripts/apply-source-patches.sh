#!/bin/sh
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

set -eu
packaging="${1:-/packaging}"
patches="$packaging/patches"
(cd "$patches" && sha256sum --check --strict SHA256SUMS)
sha256sum --check --strict --quiet "$packaging/source.upstream.sha256"
sha256sum --check --strict "$patches/upstream.sha256"
test ! -e server/authflow/kars_compat_test.go
test ! -e connector/saml/kars_compat_test.go
test ! -e server/grants/kars_refresh_test.go
# Exact pre/post hashes prohibit drift; no fuzzy, reversed or repeated patches.
while IFS= read -r reviewed; do
    case "$reviewed" in
        0001-refresh-storage-deadlock.patch) ;;
        *) echo "Unreviewed patch: $reviewed" >&2; exit 1 ;;
    esac
    patch --batch --forward --fuzz=0 -p1 < "$patches/$reviewed"
done < "$patches/series"
cp "$patches/server_compat_test.go" server/authflow/kars_compat_test.go
cp "$patches/saml_compat_test.go" connector/saml/kars_compat_test.go
cp "$patches/refresh_compat_test.go" server/grants/kars_refresh_test.go
sha256sum --check --strict "$patches/patched.sha256"

# Replace only the reviewed paths; retain every other original inventory entry.
awk 'NR == FNR { replaced[$2] = 1; print; next } !($2 in replaced)' \
    "$patches/patched.sha256" "$packaging/source.upstream.sha256" \
    | LC_ALL=C sort -k2 > "$packaging/source.sha256"
sha256sum --check --strict --quiet "$packaging/source.sha256"
sh "$packaging/scripts/source-inventory.sh" > "$packaging/source.actual.sha256"
cmp "$packaging/source.sha256" "$packaging/source.actual.sha256"
