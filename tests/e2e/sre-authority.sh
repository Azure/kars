#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# Called only by the explicit disposable Kind harness. No live/default context.
sre_migration_helm_wait_arg() {
    case "$1" in
        v3.*) printf '%s\n' --wait ;;
        v4.*) printf '%s\n' --wait=legacy ;;
        *) printf '%s\n' "Unsupported Helm version for staged SRE acceptance" >&2; return 1 ;;
    esac
}

sre_hermes_image_matches() {
    local image="$1" configured="$2" runtime="$3"
    [ "$runtime" = "Hermes" ] && [ -n "$image" ] || return 1
    if [ -n "$configured" ]; then
        [ "$image" = "$configured" ]
    else
        [[ "$image" == *hermes* ]]
    fi
}

sre_authority_phase() {
    local phase="$1" output result=0 line
    info "SRE authority acceptance: ${phase}"
    output=$(python3 "$SCRIPT_DIR/sre-authority.py" "$phase") || result=$?
    while IFS= read -r line; do
        case "$line" in
            "SRE-PASS "*) pass "${line#SRE-PASS }" ;;
            "SRE-FAIL "*) fail "${line#SRE-FAIL }" ;;
            *) [ -z "$line" ] || printf '%s\n' "$line" ;;
        esac
    done <<< "$output"
    return "$result"
}

prepare_sre_authority_legacy() {
    sre_authority_phase prepare || return
    SRE_LEGACY_PREPARED=1
}

test_sre_authority_migration() {
    sre_authority_phase legacy
}

sre_authority_cleanup() {
    python3 "$SCRIPT_DIR/sre-authority.py" cleanup
}
