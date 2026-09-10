#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# The subshell keeps private fixture credentials and its cleanup trap local.
test_governed_services() (
    set +x
    local k=(kubectl --context kind-kars-e2e)
    local scratch forward_pid="" port="" token agent_token scope request_id new_scope code
    local sandbox_uid namespace_uid
    scratch=$(mktemp -d) || return 1
    cleanup_governed_smoke() {
        if [ -n "$forward_pid" ]; then
            kill "$forward_pid" 2>/dev/null || true
            wait "$forward_pid" 2>/dev/null || true
        fi
        rm -f "$scratch/forward.log" "$scratch/response.json" "$scratch/request.json"
        rmdir "$scratch"
    }
    trap cleanup_governed_smoke EXIT

    # Values travel only through the test process and curl's stdin, not argv/logs.
    token=$("${k[@]}" get secret router-services-admin -n kars-e2e-test \
        --request-timeout=20s -o go-template='{{index .data "control-token" | base64decode}}') || return 1
    agent_token=$("${k[@]}" get secret router-admin-token -n kars-e2e-test \
        --request-timeout=20s -o go-template='{{index .data "token" | base64decode}}') || return 1
    [ -n "$token" ] && [ -n "$agent_token" ] && [ "$token" != "$agent_token" ] || return 1
    sandbox_uid=$("${k[@]}" get karssandbox e2e-test -n kars-system \
        --request-timeout=20s -o jsonpath='{.metadata.uid}') || return 1
    namespace_uid=$("${k[@]}" get namespace kars-e2e-test \
        --request-timeout=20s -o jsonpath='{.metadata.uid}') || return 1
    [ -n "$sandbox_uid" ] && [ -n "$namespace_uid" ] || return 1
    "${k[@]}" get deployment e2e-test -n kars-e2e-test --request-timeout=20s -o json \
        >"$scratch/response.json" || return 1
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

    "${k[@]}" port-forward --address 127.0.0.1 service/e2e-test -n kars-e2e-test :8443 \
        >"$scratch/forward.log" 2>&1 &
    forward_pid=$!
    local deadline=$(($(date +%s) + 30))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        kill -0 "$forward_pid" 2>/dev/null || return 1
        port=$(sed -n 's/^Forwarding from 127\.0\.0\.1:\([0-9]*\) ->.*/\1/p' "$scratch/forward.log" | head -1)
        [ -z "$port" ] || break
        sleep 1
    done
    [ -n "$port" ] || return 1

    service_request() {
        local expected="$1" method="$2" path="$3" bearer="${4:-}"
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
        if [ "$code" != "$expected" ]; then
            printf 'Governed service %s %s returned %s, expected %s\n' "$method" "$path" "$code" "$expected" >&2
            return 1
        fi
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

    service_request 401 GET /internal/access-requests || return 1
    service_request 401 GET /internal/access-requests "$agent_token" || return 1
    service_request 200 GET /internal/access-requests "$token" || return 1
    python3 - "$scratch/response.json" "$sandbox_uid" "$namespace_uid" <<'PY' || return 1
import json, sys
response = json.load(open(sys.argv[1]))
identity = response["scope"]["identity"]
assert identity["sandbox"] == {"namespace": "kars-system", "name": "e2e-test", "uid": sys.argv[2]}
assert identity["namespace_uid"] == sys.argv[3]
assert identity.get("task") is None
assert response["enforcement_changed"] is False
PY
    scope=$(response_field scope.id) || return 1
    service_body scope_id "$scope" kind egress target example.invalid reason fixture || return 1
    service_request 202 POST /v1/access-request || return 1
    request_id=$(response_field request.request_id) || return 1
    service_body scope_id "$scope" request_id "$request_id" verdict approved || return 1
    service_request 401 POST /internal/access-requests/decision "$agent_token" || return 1
    service_request 200 POST /internal/access-requests/decision "$token" || return 1
    python3 - "$scratch/response.json" <<'PY' || return 1
import json, sys
response = json.load(open(sys.argv[1]))
assert response["request"]["status"] == "approved"
assert response["enforcement_changed"] is False
PY
    service_body scope_id "$scope" assignment_id fixture-assignment || return 1
    service_request 401 POST /internal/access-requests/reset "$agent_token" || return 1
    service_request 200 POST /internal/access-requests/reset "$token" || return 1
    new_scope=$(response_field scope.id) || return 1
    [ "$new_scope" != "$scope" ] || return 1
    [ "$(response_field scope.identity.sandbox.uid)" = "$sandbox_uid" ] || return 1
    service_body scope_id "$scope" request_id "$request_id" verdict approved || return 1
    service_request 409 POST /internal/access-requests/decision "$token" || return 1
    service_body scope_id "$scope" kind egress target example.invalid || return 1
    service_request 409 POST /v1/access-request || return 1
    service_request 200 GET /telemetry/cursor || return 1
    [ "$(response_field scope_id)" = "$new_scope" ] || return 1
)
