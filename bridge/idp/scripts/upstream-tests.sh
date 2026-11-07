#!/bin/sh
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

set -eu
# Qualify a virtual test-only addition without changing the reviewed source tree.
(cd /packaging/patches && sha256sum --check --strict SHA256SUMS)
sha256sum --check --strict /packaging/source.sha256 > /dev/null
test ! -e connector/google/kars_discovery_test.go
go test -list '^TestNewAuthenticateMessage_ChallengeTargetInfoOffsetOverflowNoPanics$' \
    github.com/Azure/go-ntlmssp > /out/doc/ntlm-test-list.txt
grep -Fx 'TestNewAuthenticateMessage_ChallengeTargetInfoOffsetOverflowNoPanics' /out/doc/ntlm-test-list.txt
if ! go test -json -count=1 -race github.com/Azure/go-ntlmssp/... > /out/doc/ntlm-tests.json; then
    cat /out/doc/ntlm-tests.json
    exit 1
fi
cat /out/doc/ntlm-tests.json
if ! go test -overlay=/packaging/patches/google-discovery-overlay.json -json -count=1 -race ./... > /out/doc/upstream-tests.json; then
    cat /out/doc/upstream-tests.json
    exit 1
fi
sha256sum --check --strict /packaging/source.sha256 > /dev/null
cd api/v2
if ! go test -json -count=1 -race ./... > /out/doc/api-tests.json; then
    cat /out/doc/api-tests.json
    exit 1
fi
