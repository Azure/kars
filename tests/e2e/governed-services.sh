#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# The subshell keeps private fixture credentials and its cleanup trap local.
test_governed_services() (
    set +x
    local k=(kubectl --context kind-kars-e2e)
    local scratch forward_pid="" port="" token agent_token scope request_id new_scope code
    local sandbox_uid namespace_uid
    local stage=setup category=command expected_status=0 actual_status=0
    local token_present=false agent_token_present=false tokens_distinct=false
    local sandbox_present=false namespace_present=false forward_started=false
    local scope_changed=false sandbox_preserved=false telemetry_scope_matches=false
    scratch=$(mktemp -d) || return 1
    exec 3>&2
    exec 2>"$scratch/commands.log"
    governed_failure() {
        printf 'GOVERNED-SERVICES-FAILURE {"stage":"%s","category":"%s","expectedHttpStatus":%s,"httpStatus":%s,"operatorTokenPresent":%s,"agentTokenPresent":%s,"tokensDistinct":%s,"sandboxUidPresent":%s,"namespaceUidPresent":%s,"forwardStarted":%s,"scopeChanged":%s,"sandboxPreserved":%s,"telemetryScopeMatches":%s}\n' \
            "$stage" "$category" "$expected_status" "$actual_status" \
            "$token_present" "$agent_token_present" "$tokens_distinct" \
            "$sandbox_present" "$namespace_present" "$forward_started" \
            "$scope_changed" "$sandbox_preserved" "$telemetry_scope_matches" >&3
    }
    service_stage() {
        stage="$1"
        category=assertion
        expected_status=0
        actual_status=0
    }
    cleanup_governed_smoke() {
        local result=$?
        [ "$result" -eq 0 ] || governed_failure
        if [ -n "$forward_pid" ]; then
            kill "$forward_pid" 2>/dev/null || true
            wait "$forward_pid" 2>/dev/null || true
        fi
        if ! rm -f "$scratch/forward.log" "$scratch/response.json" "$scratch/request.json" "$scratch/commands.log" \
            || ! rmdir "$scratch"; then
            service_stage cleanup
            category=cleanup
            governed_failure
            result=1
        fi
        trap - EXIT
        exit "$result"
    }
    trap cleanup_governed_smoke EXIT

    # Values travel only through the test process and curl's stdin, not argv/logs.
    service_stage operator-token-read
    token=$("${k[@]}" get secret router-services-admin -n kars-e2e-test \
        --request-timeout=20s -o go-template='{{index .data "control-token" | base64decode}}') || return 1
    [ -z "$token" ] || token_present=true
    service_stage agent-token-read
    agent_token=$("${k[@]}" get secret router-admin-token -n kars-e2e-test \
        --request-timeout=20s -o go-template='{{index .data "token" | base64decode}}') || return 1
    [ -z "$agent_token" ] || agent_token_present=true
    [ "$token" = "$agent_token" ] || tokens_distinct=true
    service_stage credential-distinctness
    [ -n "$token" ] && [ -n "$agent_token" ] && [ "$token" != "$agent_token" ] || return 1
    service_stage sandbox-identity-read
    sandbox_uid=$("${k[@]}" get karssandbox e2e-test -n kars-system \
        --request-timeout=20s -o jsonpath='{.metadata.uid}') || return 1
    [ -z "$sandbox_uid" ] || sandbox_present=true
    service_stage namespace-identity-read
    namespace_uid=$("${k[@]}" get namespace kars-e2e-test \
        --request-timeout=20s -o jsonpath='{.metadata.uid}') || return 1
    [ -z "$namespace_uid" ] || namespace_present=true
    service_stage identity-presence
    [ -n "$sandbox_uid" ] && [ -n "$namespace_uid" ] || return 1
    service_stage deployment-read
    "${k[@]}" get deployment e2e-test -n kars-e2e-test --request-timeout=20s -o json \
        >"$scratch/response.json" || return 1
    service_stage private-mount-isolation
    python3 - "$scratch/response.json" <<'PY' || return 1
import json, sys
pod = json.load(open(sys.argv[1]))["spec"]["template"]["spec"]
volume = next(v for v in pod["volumes"] if v.get("secret", {}).get("secretName") == "router-services-admin")
for container in pod["containers"]:
    mounts = [m for m in container.get("volumeMounts", []) if m["name"] == volume["name"]]
    if container["name"] == "inference-router":
        assert len(mounts) == 1 and mounts[0]["mountPath"] == "/etc/kars/services"
        assert mounts[0]["readOnly"] is True
    else:
        assert not mounts
PY

    service_stage port-forward-start
    "${k[@]}" port-forward --address 127.0.0.1 service/e2e-test -n kars-e2e-test :8443 \
        >"$scratch/forward.log" 2>&1 &
    forward_pid=$!
    local deadline=$(($(date +%s) + 30))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        kill -0 "$forward_pid" 2>/dev/null || { category=process-exited; return 1; }
        port=$(sed -n 's/^Forwarding from 127\.0\.0\.1:\([0-9]*\) ->.*/\1/p' "$scratch/forward.log" | head -1)
        [ -z "$port" ] || break
        sleep 1
    done
    [ -n "$port" ] || { category=deadline; return 1; }
    forward_started=true

    service_request() {
        local expected="$1" method="$2" path="$3" bearer="${4:-}"
        expected_status="$expected"
        actual_status=0
        category=http-transport
        local args=(--disable --silent --show-error --noproxy 127.0.0.1 --connect-timeout 5 --max-time 15
            --config - --request "$method" --url "http://127.0.0.1:$port$path"
            --output "$scratch/response.json" --write-out '%{http_code}')
        if [ "$method" = POST ]; then
            args+=(--header 'Content-Type: application/json' --data-binary "@$scratch/request.json")
        fi
        code=$(
            { [ -z "$bearer" ] || printf 'header = "Authorization: Bearer %s"\n' "$bearer"; } \
                | curl "${args[@]}"
        ) || return 1
        case "$code" in
            [1-5][0-9][0-9]) actual_status="$code" ;;
            *) category=invalid-http-status; return 1 ;;
        esac
        if [ "$code" != "$expected" ]; then
            category=http-status
            return 1
        fi
        category=assertion
    }
    service_body() {
        python3 - "$scratch/request.json" "$@" <<'PY'
import json, sys
args = sys.argv[2:]
assert len(args) % 2 == 0
with open(sys.argv[1], "w") as output:
    json.dump(dict(zip(args[::2], args[1::2])), output)
PY
    }
    response_field() {
        python3 - "$scratch/response.json" "$1" <<'PY'
import json, sys
value = json.load(open(sys.argv[1]))
for field in sys.argv[2].split("."):
    value = value[field]
assert isinstance(value, str) and value
print(value)
PY
    }

    service_stage anonymous-read-denial
    service_request 401 GET /internal/access-requests || return 1
    service_stage agent-read-denial
    service_request 401 GET /internal/access-requests "$agent_token" || return 1
    service_stage operator-read
    service_request 200 GET /internal/access-requests "$token" || return 1
    service_stage scope-identity
    python3 - "$scratch/response.json" "$sandbox_uid" "$namespace_uid" <<'PY' || return 1
import json, sys
response = json.load(open(sys.argv[1]))
identity = response["scope"]["identity"]
assert identity["sandbox"] == {"namespace": "kars-system", "name": "e2e-test", "uid": sys.argv[2]}
assert identity["namespace_uid"] == sys.argv[3]
assert identity.get("task") is None
assert response["enforcement_changed"] is False
PY
    service_stage scope-id
    scope=$(response_field scope.id) || return 1
    service_stage access-request-body
    service_body scope_id "$scope" kind egress target example.invalid reason fixture || return 1
    service_stage access-request
    service_request 202 POST /v1/access-request || return 1
    service_stage request-id
    request_id=$(response_field request.request_id) || return 1
    service_stage decision-body
    service_body scope_id "$scope" request_id "$request_id" verdict approved || return 1
    service_stage agent-decision-denial
    service_request 401 POST /internal/access-requests/decision "$agent_token" || return 1
    service_stage operator-decision
    service_request 200 POST /internal/access-requests/decision "$token" || return 1
    service_stage decision-response
    python3 - "$scratch/response.json" <<'PY' || return 1
import json, sys
response = json.load(open(sys.argv[1]))
assert response["request"]["status"] == "approved"
assert response["enforcement_changed"] is False
PY
    service_stage reset-body
    service_body scope_id "$scope" assignment_id fixture-assignment || return 1
    service_stage agent-reset-denial
    service_request 401 POST /internal/access-requests/reset "$agent_token" || return 1
    service_stage operator-reset
    service_request 200 POST /internal/access-requests/reset "$token" || return 1
    service_stage reset-scope-id
    new_scope=$(response_field scope.id) || return 1
    service_stage reset-scope-change
    [ "$new_scope" = "$scope" ] || scope_changed=true
    [ "$new_scope" != "$scope" ] || return 1
    service_stage reset-sandbox-preservation
    [ "$(response_field scope.identity.sandbox.uid)" = "$sandbox_uid" ] || return 1
    sandbox_preserved=true
    service_stage stale-decision-body
    service_body scope_id "$scope" request_id "$request_id" verdict approved || return 1
    service_stage stale-decision-denial
    service_request 409 POST /internal/access-requests/decision "$token" || return 1
    service_stage stale-request-body
    service_body scope_id "$scope" kind egress target example.invalid || return 1
    service_stage stale-request-denial
    service_request 409 POST /v1/access-request || return 1
    service_stage telemetry-read
    service_request 200 GET /telemetry/cursor || return 1
    service_stage telemetry-scope
    [ "$(response_field scope_id)" = "$new_scope" ] || return 1
    telemetry_scope_matches=true
)
