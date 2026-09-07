#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# Sourced only by the existing disposable Kind harness.
test_sre_namespace_ownership() {
    local context="kind-kars-e2e" system_uid sandbox_uid namespace_uid claimed_uid backlink source_ns
    local k=(kubectl --context "$context")
    system_uid=$("${k[@]}" get namespace kars-system -o jsonpath='{.metadata.uid}') || {
        fail "Cannot read the disposable Kind namespace"; return 1;
    }
    if ! helm upgrade kars "$ROOT_DIR/deploy/helm/kars" \
        --kube-context "$context" --namespace kars-system --reuse-values \
        --set sre.enabled=true --set-string runtimes.hermes.image=kars-sandbox-e2e:dev \
        --wait --timeout 3m; then
        fail "Fresh SRE enable failed"; return 1;
    fi
    local deadline=$(($(date +%s) + 120))
    local materialized=0
    while [ "$(date +%s)" -lt "$deadline" ]; do
        if "${k[@]}" get deployment sre -n kars-sre >/dev/null 2>&1 \
            && "${k[@]}" get serviceaccount sre-writer -n kars-sre >/dev/null 2>&1; then
            materialized=1
            break
        fi
        sleep 2
    done
    if [ "$materialized" -ne 1 ]; then
        "${k[@]}" get karssandbox sre -n kars-system -o yaml || true
        fail "SRE did not materialize its deployment and writer account after namespace claiming"
        return 1
    fi
    sandbox_uid=$("${k[@]}" get karssandbox sre -n kars-system -o jsonpath='{.metadata.uid}') || return 1
    namespace_uid=$("${k[@]}" get namespace kars-sre -o jsonpath='{.metadata.uid}') || return 1
    claimed_uid=$("${k[@]}" get namespace kars-sre -o go-template='{{index .metadata.annotations "kars.azure.com/sandbox-uid"}}') || return 1
    source_ns=$("${k[@]}" get namespace kars-sre -o go-template='{{index .metadata.annotations "kars.azure.com/sandbox-namespace"}}') || return 1
    backlink=$("${k[@]}" get karssandbox sre -n kars-system -o go-template='{{index .metadata.annotations "kars.azure.com/namespace-uid"}}') || return 1
    if [ -z "$sandbox_uid" ] || [ -z "$namespace_uid" ] \
        || [ "$claimed_uid" != "$sandbox_uid" ] || [ "$backlink" != "$namespace_uid" ] \
        || [ "$source_ns" != "kars-system" ]; then
        fail "Fresh SRE runtime lacks exact two-way namespace ownership"; return 1;
    fi
    if [ "$("${k[@]}" get serviceaccount sre-writer -n kars-sre -o jsonpath='{.automountServiceAccountToken}')" != "false" ]; then
        fail "SRE writer account does not disable token automount"; return 1;
    fi
    pass "Fresh SRE install materializes a UID-bound namespace, deployment and non-automounting writer"

    if ! helm upgrade kars "$ROOT_DIR/deploy/helm/kars" \
        --kube-context "$context" --namespace kars-system --reuse-values \
        --set sre.enabled=false --wait --timeout 3m; then
        fail "SRE disable failed"; return 1;
    fi
    if ! "${k[@]}" wait --for=delete karssandbox/sre -n kars-system --timeout=120s \
        || ! "${k[@]}" wait --for=delete namespace/kars-sre --timeout=120s; then
        fail "SRE namespace cleanup did not complete"; return 1;
    fi
    if [ "$("${k[@]}" get namespace kars-system -o jsonpath='{.metadata.uid}')" != "$system_uid" ]; then
        fail "SRE removal changed the core namespace identity"; return 1;
    fi
    pass "SRE removal completes guarded cleanup and preserves the core namespace"
}
