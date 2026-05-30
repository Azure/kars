# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# platforms/docker.sh — single-host Docker platform helper.
#
# Docker mode is structurally different from AKS / local-k8s: there is
# no Kubernetes API, no CRDs, no controller, and no per-sandbox
# namespace. The sandbox runs as a docker container with its policy
# bundles loaded from disk (the inference-router reads them on startup,
# same code path as the AKS in-cluster mode — only the source of bytes
# differs).
#
# Consequences for scenarios:
#   * Scenarios that REQUIRE CRDs (InferencePolicy, ToolPolicy,
#     KarsMemory, McpServer, KarsSandbox, EgressApproval) cannot run
#     unmodified on docker. The scenario must either ship a
#     `docker-overlay/` directory that maps the same intent into
#     pre-rendered policy bundles + CLI flags, or skip the scenario
#     on docker entirely.
#   * Sub-agent spawn works on docker (the parent container has the
#     docker socket mounted; spawned children are sibling containers
#     on the same docker network). The AGT relay + registry run as
#     compose-friendly sibling containers when `kars dev` is
#     used to bring the stack up.
#   * NetworkPolicy enforcement does not exist on docker; the iptables
#     egress-guard and the router L7 allow-list ARE active (the
#     guard uses NET_ADMIN inside the container; the allow-list is
#     mounted from the host as a JSON bundle).
#
# Inputs (env):
#   DOCKER_CONTAINER_NAME — name of the parent sandbox container.
#                           Default: ${SCENARIO_SANDBOX}.
#   SKIP_DEV_BRINGUP      — set to 1 to skip the `kars dev
#                           --target docker` step if the container is
#                           already up.

set -euo pipefail

DOCKER_CONTAINER_NAME="${DOCKER_CONTAINER_NAME:-${SCENARIO_SANDBOX}}"

platform_preflight() {
    command -v docker >/dev/null || { log "ERR docker not on PATH"; exit 1; }
    command -v kars >/dev/null || { log "ERR kars CLI not on PATH"; exit 1; }
    docker info >/dev/null 2>&1 || {
        log "ERR docker daemon not reachable — start Docker Desktop / dockerd"
        exit 1
    }

    if [ "${SKIP_DEV_BRINGUP:-0}" = "1" ]; then
        log "SKIP_DEV_BRINGUP=1 — assuming docker container '${DOCKER_CONTAINER_NAME}' already exists"
    elif docker ps --format '{{.Names}}' | grep -qx "${DOCKER_CONTAINER_NAME}"; then
        log "docker container '${DOCKER_CONTAINER_NAME}' already running — skipping bring-up"
    else
        log "bringing up docker sandbox via 'kars dev --target docker --once'"
        # Note: `--once` so the CLI doesn't take over our terminal. The
        # scenario's policies / channels / mcp servers are NOT auto-
        # applied — docker mode requires the scenario to either preload
        # the bundles into the image or provide a docker-overlay/ dir
        # the helper sources. We do not pretend otherwise.
        kars dev --target docker \
            --name "${DOCKER_CONTAINER_NAME}" \
            >>"${OUT_DIR}/dev-bringup.log" 2>&1 || {
                log "ERR kars dev bring-up failed; tail of dev-bringup.log:"
                tail -n 80 "${OUT_DIR}/dev-bringup.log" >&2 || true
                exit 1
            }
    fi

    {
        echo "platform: docker"
        echo "container: ${DOCKER_CONTAINER_NAME}"
        echo "caveats:"
        echo "  - CRD-based scenarios (InferencePolicy, ToolPolicy, etc.)"
        echo "    do NOT apply on docker — policies are loaded from disk"
        echo "    bundles set up by 'kars dev'."
        echo "  - NetworkPolicy enforcement: not applicable. Egress is"
        echo "    enforced by the iptables egress-guard inside the"
        echo "    container and the router L7 allow-list, same as AKS."
        echo "  - Per-sandbox K8s namespaces do not exist. Sub-agents are"
        echo "    sibling containers on the same docker network."
    } >"${OUT_DIR}/platform-notes.txt"

    log "docker preflight ok — container: ${DOCKER_CONTAINER_NAME}"
}

platform_apply() {
    # Docker has no CRDs to apply. Scenarios designed for docker may
    # ship a `manifests-docker/` directory with shell snippets the
    # helper sources (e.g. for setting up extra sibling containers).
    # If the scenario doesn't provide one, we no-op cleanly.
    local docker_overlay="${SCENARIO_DIR}/manifests-docker"
    if [ -d "${docker_overlay}" ]; then
        log "applying docker overlay from ${docker_overlay}"
        for f in "${docker_overlay}"/*.sh; do
            [ -e "$f" ] || continue
            log "  -> $(basename "$f")"
            bash "$f" >>"${OUT_DIR}/apply.log" 2>&1 || {
                tail -n 40 "${OUT_DIR}/apply.log"; exit 2
            }
        done
    else
        log "scenario has no manifests-docker/ overlay — skipping apply"
        log "(K8s manifests in ${MANIFESTS_DIR} are not applicable to docker; scenario must provide a docker overlay if it needs setup)"
    fi
}

platform_credentials() {
    # For docker, channel secrets are passed as env vars to the
    # container via `kars dev` flags rather than as a K8s Secret.
    # The bring-up step has already wired them; nothing to do here.
    if [ -n "${TELEGRAM_BOT_TOKEN:-}" ]; then
        log "TELEGRAM_BOT_TOKEN present — assumed already passed to 'kars dev' at bring-up"
    fi
}

platform_wait_for_sandbox() {
    # Wait for the gateway to be reachable. The dev bring-up exposes
    # 18789 on the host (the openclaw container forwards there).
    log "waiting for gateway on localhost:18789 (timeout 120s)"
    local i=0
    while [ $i -lt 120 ]; do
        if curl -sf --max-time 1 "http://127.0.0.1:18789/healthz" \
                >/dev/null 2>&1; then
            log "gateway responding"
            return 0
        fi
        i=$((i+1)); sleep 1
    done
    log "ERR gateway never responded on localhost:18789 within 120s"
    docker logs --tail=80 "${DOCKER_CONTAINER_NAME}" >&2 2>/dev/null || true
    exit 3
}

platform_post_prompt() {
    log "posting ${SCENARIO} prompt to docker sandbox on localhost:18789"

    # Read the gateway-token from inside the container (matches what
    # `kars connect` does — the token lives at /tmp/gateway-token
    # inside the openclaw container, written by entrypoint.sh).
    local gateway_token
    gateway_token=$(docker exec "${DOCKER_CONTAINER_NAME}" \
        sh -c 'cat /tmp/gateway-token 2>/dev/null || true' \
        | tr -d '\n' || true)
    if [ -z "${gateway_token}" ]; then
        log "ERR could not read /tmp/gateway-token from ${DOCKER_CONTAINER_NAME}"
        exit 4
    fi

    local session_id="${SCENARIO}-$(date -u +%Y%m%dT%H%M%SZ)"
    log "session_id=${session_id}"

    local body_file="${OUT_DIR}/request.json"
    python3 - "${PROMPT_FILE}" "${session_id}" > "${body_file}" <<'PY'
import json, sys
prompt = open(sys.argv[1]).read()
session_id = sys.argv[2]
print(json.dumps({
    "model": "openclaw",
    "messages": [{"role": "user", "content": prompt}],
    "stream": False,
    "user": session_id,
}))
PY

    run_with_watchdog "${WATCHDOG_SECS}" \
        curl -sS --no-buffer --fail-with-body \
            -H "Authorization: Bearer ${gateway_token}" \
            -H "Content-Type: application/json" \
            --data-binary "@${body_file}" \
            "http://127.0.0.1:18789/v1/chat/completions" \
        | tee "${OUT_DIR}/response.json"
    local rc=${PIPESTATUS[0]}

    if [ "${rc}" -eq 124 ]; then
        log "ERR prompt timed out after ${WATCHDOG_SECS}s"; exit 4
    elif [ "${rc}" -ne 0 ]; then
        log "ERR gateway request failed rc=${rc}"; exit 4
    fi

    python3 - "${OUT_DIR}/response.json" "${OUT_DIR}/transcript.log" <<'PY'
import json, sys
resp = json.load(open(sys.argv[1]))
out = open(sys.argv[2], 'w')
for choice in resp.get('choices', []):
    msg = choice.get('message', {})
    txt = msg.get('content', '')
    if isinstance(txt, list):
        txt = ''.join(p.get('text','') for p in txt if isinstance(p, dict))
    out.write(txt + '\n')
out.close()
PY
    log "prompt completed — transcript at ${OUT_DIR}/transcript.log"
}

platform_collect_artifacts() {
    # Pull /tmp/gateway.log from the parent container and from any
    # sub-agent containers the scenario declared. Sub-agent containers
    # on docker share the docker network with the parent and are
    # typically named `<parent>-<sub>` (matches the spawn helper's
    # container naming convention).
    log "collecting gateway logs from docker containers"

    docker exec "${DOCKER_CONTAINER_NAME}" \
        sh -c 'cat /tmp/gateway.log 2>/dev/null || true' \
        >"${OUT_DIR}/${SCENARIO_SANDBOX}-gateway.log" || true

    # Synthesize a trace.jsonl from each container's inference-router
    # log so verify.py's router_lines-based checks (e.g. image_calls,
    # container_hits) work on docker the same way they do on K8s where
    # monitor.sh produces this file from `kubectl logs`. monitor.sh is
    # kubectl-based and skipped on docker (see run.sh), so we build it
    # here.
    local trace_file="${OUT_DIR}/trace.jsonl"
    : >"${trace_file}"
    _append_router_lines() {
        local cname="$1"
        docker exec "${cname}" \
            sh -c 'cat /tmp/inference-router.log 2>/dev/null || true' \
            | while IFS= read -r line; do
                [ -z "$line" ] && continue
                # router emits structured JSON whose "fields.message"
                # carries the human-readable text. Extract message plus
                # any per-event fields (path, url, deployment) that
                # verify.py's checks regex against (e.g. image_calls
                # match on "images/generations" or "gpt-image-1").
                python3 -c '
import json, sys
try:
    d = json.loads(sys.argv[1])
    f = d.get("fields", {})
    parts = [f.get("message", "")]
    for k in ("path","url","deployment","model"):
        v = f.get(k)
        if v: parts.append(f"{k}={v}")
    print(json.dumps({"src":"ROUTER","msg":" ".join(parts)}))
except Exception:
    pass
' "$line" 2>/dev/null
            done >>"${trace_file}"
    }
    _append_router_lines "${DOCKER_CONTAINER_NAME}"

    for sub in "${SCENARIO_SUB_SANDBOXES[@]}"; do
        # Try plain name first, then prefixed name.
        local cname
        cname=$(docker ps --format '{{.Names}}' \
            | grep -E "(^${sub}$|-${sub}$)" | head -1 || true)
        if [ -z "${cname}" ]; then
            log "  sub-agent container for '${sub}' not found — skipping"
            continue
        fi
        log "  ${sub} ← ${cname}"
        # Resolve the scenario's per-sub grep pattern (same shape as AKS).
        local arr_name="SCENARIO_GREP_PATTERNS_${sub}"
        local -n patterns_ref="${arr_name}" 2>/dev/null || patterns_ref=()
        local pat="${patterns_ref[0]:-mesh_transfer_file|file_transfer_ack}"
        docker exec "${cname}" sh -c \
            "grep -E '${pat}' /tmp/gateway.log 2>/dev/null || true" \
            >"${OUT_DIR}/${sub}-gateway.log" || true
        _append_router_lines "${cname}"
    done

    if [ -n "${SCENARIO_INCOMING_SANDBOX}" ] \
       && [ -n "${SCENARIO_INCOMING_PATH}" ]; then
        local cname
        cname=$(docker ps --format '{{.Names}}' \
            | grep -E "(^${SCENARIO_INCOMING_SANDBOX}$|-${SCENARIO_INCOMING_SANDBOX}$)" \
            | head -1 || true)
        if [ -n "${cname}" ]; then
            docker exec "${cname}" \
                ls -la "${SCENARIO_INCOMING_PATH}" 2>/dev/null \
                >"${OUT_DIR}/${SCENARIO_INCOMING_SANDBOX}-incoming.txt" || true
        fi
    fi

    log "artifacts collected"
}
