#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# Uses only the existing disposable Kind cluster and its loaded BYO test image.
# The consumer emits fixed markers, never environment values.
wait_for_credential_consumer() {
    local marker="$1" deadline=$(($(date +%s) + 150)) pod namespace
    local k=(kubectl --context kind-kars-e2e)
    while [ "$(date +%s)" -lt "$deadline" ]; do
        namespace=$("${k[@]}" get namespace kars-e2e-source --ignore-not-found \
            -o jsonpath='{.metadata.uid}') || return 1
        if [ -z "$namespace" ]; then
            sleep 2
            continue
        fi
        pod=$("${k[@]}" get pods -n kars-e2e-source -l kars.azure.com/sandbox=e2e-source \
            -o go-template='{{range .items}}{{if not .metadata.deletionTimestamp}}{{.metadata.name}}{{"\n"}}{{end}}{{end}}' \
            2>/dev/null | head -1) || return 1
        if [ -n "$pod" ] && "${k[@]}" logs "$pod" -n kars-e2e-source -c agent --tail=10 2>/dev/null \
            | grep -qx "$marker"; then
            return 0
        fi
        sleep 2
    done
    fail "Credential consumer did not reach $marker"
    return 1
}

test_credential_sources() {
    local k=(kubectl --context kind-kars-e2e)
    local source_uid projection_uid direct_uid namespace_uid projection admission
    namespace_uid=$("${k[@]}" get namespace kars-system -o jsonpath='{.metadata.uid}') || return 1
    if admission=$("${k[@]}" create --dry-run=server -f - 2>&1 <<'YAML'
apiVersion: kars.azure.com/v1alpha1
kind: KarsSandbox
metadata:
  name: e2e-source-overlay
  namespace: kars-system
spec:
  inferenceRef:
    name: e2e-source-inference
  runtime:
    kind: BYO
    byo:
      image: kars-sandbox-e2e:dev
      contractVersion: v1
  sandbox:
    isolation: standard
  credentialsRef:
    name: kars-credential-source-e2e-source-overlay
    uid: fixture-uid
  upstreamCompatibility:
    sigsAgentSandbox: overlay
    upstreamSandboxRef:
      name: upstream-fixture
YAML
    ); then
        fail "Admission accepted source credentials on an overlay-managed runtime"; return 1
    fi
    if ! printf '%s\n' "$admission" | grep -q "credentialsRef requires a controller-managed runtime"; then
        printf '%s\n' "$admission"
        fail "Overlay admission failed for a reason other than the intended source guard"; return 1
    fi
    pass "The API server compiles the source schema and rejects overlay credentials for the intended reason"
    if ! "${k[@]}" create -f - <<'YAML'
apiVersion: v1
kind: Secret
metadata:
  name: kars-credential-source-e2e-source
  namespace: kars-system
  annotations:
    kars.azure.com/credential-purpose: agent-source-v1
    kars.azure.com/credential-target: e2e-source
    kars.azure.com/credential-workspace: kars-system
    kars.azure.com/credential-binding-intent: explicit-reference-v1
type: Opaque
stringData:
  BRAVE_API_KEY: fixture-one
  TAVILY_API_KEY: fixture-auxiliary
---
apiVersion: kars.azure.com/v1alpha1
kind: InferencePolicy
metadata:
  name: e2e-source-inference
  namespace: kars-system
spec:
  appliesTo:
    sandboxName: e2e-source
  modelPreference:
    primary:
      provider: azure-openai
      deployment: gpt-4.1
YAML
    then
        fail "Cannot create the credential-source fixtures"; return 1
    fi
    source_uid=$("${k[@]}" get secret kars-credential-source-e2e-source -n kars-system \
        -o jsonpath='{.metadata.uid}') || return 1
    [ -n "$source_uid" ] || { fail "Source fixture omitted its UID"; return 1; }
    if ! "${k[@]}" create -f - <<YAML
apiVersion: kars.azure.com/v1alpha1
kind: KarsSandbox
metadata:
  name: e2e-source
  namespace: kars-system
spec:
  inferenceRef:
    name: e2e-source-inference
  credentialsRef:
    name: kars-credential-source-e2e-source
    uid: $source_uid
  runtime:
    kind: BYO
    byo:
      image: kars-sandbox-e2e:dev
      contractVersion: v1
      command: ["/bin/sh", "-c"]
      args:
        - |
          case "\${BRAVE_API_KEY-}" in
            fixture-one)
              [ "\${TAVILY_API_KEY-}" = fixture-auxiliary ] && echo CREDENTIAL_FIXTURE_INITIAL ;;
            fixture-two)
              [ -z "\${TAVILY_API_KEY+x}" ] && echo CREDENTIAL_FIXTURE_ROTATED ;;
            fixture-legacy)
              echo CREDENTIAL_FIXTURE_LEGACY ;;
          esac
          exec sleep infinity
  sandbox:
    isolation: standard
  resources:
    requests: {cpu: 100m, memory: 128Mi}
    limits: {cpu: 500m, memory: 256Mi}
YAML
    then
        fail "Cannot create the source-bound Sandbox"; return 1
    fi
    if [ "$("${k[@]}" get karssandbox e2e-source -n kars-system -o jsonpath='{.spec.credentialsRef.uid}')" != "$source_uid" ]; then
        fail "The API server pruned or changed the credential reference"; return 1
    fi
    wait_for_credential_consumer CREDENTIAL_FIXTURE_INITIAL || return 1
    projection_uid=$("${k[@]}" get secret e2e-source-credential-projection -n kars-e2e-source \
        -o jsonpath='{.metadata.uid}') || return 1
    [ -n "$projection_uid" ] || { fail "Projection fixture omitted its UID"; return 1; }
    pass "A real BYO consumer receives its explicitly UID-bound source collection"

    "${k[@]}" create secret generic e2e-source-credentials -n kars-e2e-source \
        --from-literal=BRAVE_API_KEY=fixture-legacy --from-literal=TAVILY_API_KEY=fixture-legacy-auxiliary \
        >/dev/null || return 1
    direct_uid=$("${k[@]}" get secret e2e-source-credentials -n kars-e2e-source \
        -o jsonpath='{.metadata.uid}') || return 1
    "${k[@]}" patch secret kars-credential-source-e2e-source -n kars-system --type=merge \
        -p '{"stringData":{"BRAVE_API_KEY":"fixture-two"},"data":{"TAVILY_API_KEY":null}}' \
        >/dev/null || return 1
    wait_for_credential_consumer CREDENTIAL_FIXTURE_ROTATED || return 1
    if [ "$("${k[@]}" get secret e2e-source-credential-projection -n kars-e2e-source -o jsonpath='{.metadata.uid}')" != "$projection_uid" ]; then
        fail "Rotation replaced the projection instead of updating the owned Secret"; return 1
    fi
    pass "Source rotation replaces the consumer and removed keys do not fall back to legacy values"

    "${k[@]}" delete secret kars-credential-source-e2e-source -n kars-system --timeout=30s >/dev/null || return 1
    local deadline=$(($(date +%s) + 150)) revoked=0 reason replicas pods keys
    while [ "$(date +%s)" -lt "$deadline" ]; do
        reason=$("${k[@]}" get karssandbox e2e-source -n kars-system \
            -o jsonpath='{.status.conditions[?(@.type=="Degraded")].reason}') || return 1
        replicas=$("${k[@]}" get deployment e2e-source -n kars-e2e-source \
            -o jsonpath='{.spec.replicas}') || return 1
        pods=$("${k[@]}" get pods -n kars-e2e-source -l kars.azure.com/sandbox=e2e-source \
            -o go-template='{{len .items}}') || return 1
        keys=$("${k[@]}" get secret e2e-source-credential-projection -n kars-e2e-source \
            --ignore-not-found -o go-template='{{if .data}}{{len .data}}{{else}}0{{end}}') || return 1
        if [ "$reason" = CredentialSourceUnavailable ] && [ "$replicas" = 0 ] && [ "$pods" = 0 ] \
            && { [ -z "$keys" ] || [ "$keys" = 0 ]; }; then
            revoked=1
            break
        fi
        sleep 2
    done
    if [ "$revoked" != 1 ]; then
        fail "Source deletion did not stop the consumer and revoke its owned projection"; return 1
    fi
    if [ "$("${k[@]}" get secret e2e-source-credentials -n kars-e2e-source -o jsonpath='{.metadata.uid}')" != "$direct_uid" ]; then
        fail "Source revocation changed the unrelated legacy Secret"; return 1
    fi
    pass "Source deletion revokes the consumer without reactivating or deleting legacy credentials"

    "${k[@]}" patch karssandbox e2e-source -n kars-system --type=merge \
        -p '{"spec":{"credentialsRef":null}}' >/dev/null || return 1
    wait_for_credential_consumer CREDENTIAL_FIXTURE_LEGACY || return 1
    projection=$("${k[@]}" get secret e2e-source-credential-projection -n kars-e2e-source \
        --ignore-not-found -o name) || return 1
    if [ -n "$projection" ]; then
        fail "Explicit source opt-out left its owned projection behind"; return 1
    fi
    pass "Explicit opt-out restores the unchanged legacy collection only when requested"

    "${k[@]}" delete karssandbox e2e-source -n kars-system --wait=false >/dev/null || return 1
    "${k[@]}" wait --for=delete namespace/kars-e2e-source --timeout=120s || return 1
    "${k[@]}" delete inferencepolicy e2e-source-inference -n kars-system >/dev/null || return 1
    if [ "$("${k[@]}" get namespace kars-system -o jsonpath='{.metadata.uid}')" != "$namespace_uid" ]; then
        fail "Credential-source cleanup changed the core namespace"; return 1
    fi
    pass "Credential-source lifecycle cleanup preserves the core namespace"
}
