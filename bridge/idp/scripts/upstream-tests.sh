#!/bin/sh
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

set -eu
go test -list '^TestNewAuthenticateMessage_ChallengeTargetInfoOffsetOverflowNoPanics$' \
    github.com/Azure/go-ntlmssp > /out/doc/ntlm-test-list.txt
grep -Fx 'TestNewAuthenticateMessage_ChallengeTargetInfoOffsetOverflowNoPanics' /out/doc/ntlm-test-list.txt
if ! go test -json -count=1 -race github.com/Azure/go-ntlmssp/... > /out/doc/ntlm-tests.json; then
    cat /out/doc/ntlm-tests.json
    exit 1
fi
cat /out/doc/ntlm-tests.json
if ! go test -json -count=1 -race ./... > /out/doc/upstream-tests.json; then
    cat /out/doc/upstream-tests.json
    exit 1
fi
cd api/v2
if ! go test -json -count=1 -race ./... > /out/doc/api-tests.json; then
    cat /out/doc/api-tests.json
    exit 1
fi
