#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

managed_mcp_phase() {
    local phase="$1" output result=0 line
    output=$(python3 "$SCRIPT_DIR/managed-mcp.py" "$phase") || result=$?
    while IFS= read -r line; do
        case "$line" in
            "MCP-PASS "*) pass "${line#MCP-PASS }" ;;
            "MCP-FAIL "*) fail "${line#MCP-FAIL }" ;;
            *) [ -z "$line" ] || printf '%s\n' "$line" ;;
        esac
    done <<< "$output"
    return "$result"
}

prepare_managed_mcp() {
    managed_mcp_phase prepare
}

test_managed_mcp() {
    managed_mcp_phase test
}
