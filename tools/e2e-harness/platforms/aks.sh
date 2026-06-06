# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# platforms/aks.sh — AKS-targeting platform helper for the e2e-harness.
#
# Assumes the caller is already kubectl-logged into a cluster with the
# kars helm chart installed (i.e. CRDs present, controller running).
# Use `kars up` to provision that cluster ahead of time; this file
# does NOT touch the Azure subscription.
#
# Sourced by drive.sh, which exports SCENARIO_DIR, MANIFESTS_DIR,
# PROMPT_FILE, SCENARIO_SANDBOX, OUT_DIR, WATCHDOG_SECS, and the
# scenario's SCENARIO_INCOMING_* knobs. Sub-agent lists and grep
# patterns are read from the bash globals the scenario's config.sh
# already set.

platform_preflight() {
    command -v kubectl >/dev/null || { log "ERR kubectl not on PATH"; exit 1; }
    command -v kars >/dev/null || { log "ERR kars CLI not on PATH"; exit 1; }
    kubectl config current-context >/dev/null || {
        log "ERR no current kubectl context — run 'kars up' first"; exit 1
    }
    for crd in karssandboxes.kars.azure.com \
               inferencepolicies.kars.azure.com \
               toolpolicies.kars.azure.com \
               karsmemories.kars.azure.com \
               mcpservers.kars.azure.com; do
        kubectl get crd "$crd" >/dev/null 2>&1 || {
            log "ERR CRD ${crd} missing — helm chart not installed"; exit 1
        }
    done
    log "AKS preflight ok — kubectl context: $(kubectl config current-context)"
}

platform_apply() {
    log "applying ${MANIFESTS_DIR}/*.yaml in lexical order"
    for f in "${MANIFESTS_DIR}"/*.yaml; do
        [ -e "$f" ] || continue
        log "  -> $(basename "$f")"
        kubectl apply -f "$f" >>"${OUT_DIR}/apply.log" 2>&1 || {
            tail -n 40 "${OUT_DIR}/apply.log"; exit 2
        }
    done
}

platform_credentials() {
    if [ -n "${TELEGRAM_BOT_TOKEN:-}" ]; then
        log "creating ${SCENARIO_SANDBOX}-credentials secret with TELEGRAM_BOT_TOKEN"
        kubectl create secret generic "${SCENARIO_SANDBOX}-credentials" \
            --namespace "kars-${SCENARIO_SANDBOX}" \
            --from-literal=TELEGRAM_BOT_TOKEN="${TELEGRAM_BOT_TOKEN}" \
            --dry-run=client -o yaml | kubectl apply -f -
    else
        log "no TELEGRAM_BOT_TOKEN set — Telegram acceptance check will be skipped"
    fi
}

platform_wait_for_sandbox() {
    # Sticky-inbox guard: sandbox pods accumulate state across runs (the
    # AGT mesh inbox holds peer messages until the container restarts).
    # A stale `from_agent: analyst` message left by a prior run can make
    # the new run's mesh_await(senders=['analyst']) match instantly,
    # which causes the consumer agent to skip waiting and surface a
    # false-positive `blocked: file_read unavailable` to the parent
    # (the file simply hasn't arrived yet from THIS run's analyst). We
    # rollout-restart the parent and any declared sub-agents to force
    # fresh containers before posting the prompt. Disable by setting
    # SKIP_SANDBOX_RESTART=1 (e.g. for iterating on the harness itself).
    if [ "${SKIP_SANDBOX_RESTART:-0}" != "1" ]; then
        log "rollout-restarting sandbox pods for clean inbox state"
        local targets=("${SCENARIO_SANDBOX}" "${SCENARIO_SUB_SANDBOXES[@]}")
        for name in "${targets[@]}"; do
            local ns="kars-${name}"
            kubectl get deploy -n "${ns}" "${name}" >/dev/null 2>&1 || continue
            kubectl rollout restart -n "${ns}" "deploy/${name}" \
                >>"${OUT_DIR}/apply.log" 2>&1 || true
        done
    else
        log "SKIP_SANDBOX_RESTART=1 — reusing existing pods (inbox may be stale)"
    fi

    log "waiting for KarsSandbox/${SCENARIO_SANDBOX} → Ready (timeout 600s)"
    kubectl wait --for=condition=Ready \
        "karssandbox/${SCENARIO_SANDBOX}" \
        --namespace kars-system \
        --timeout=600s || { log "ERR sandbox not Ready in time"; exit 3; }

    if [ "${SKIP_SANDBOX_RESTART:-0}" != "1" ]; then
        # `kubectl wait deploy --for=Available` can flip true on the OLD
        # replicaset while the rollout-restart's NEW replicaset is still
        # spinning up — the harness then port-forwards onto a terminating
        # pod and gets "Empty reply from server". Block on rollout status
        # for the PARENT deploy first so we are talking to the new pod.
        kubectl rollout status \
            -n "kars-${SCENARIO_SANDBOX}" \
            "deploy/${SCENARIO_SANDBOX}" \
            --timeout=300s >>"${OUT_DIR}/apply.log" 2>&1 || true
    fi

    kubectl wait --for=condition=Available \
        "deploy/${SCENARIO_SANDBOX}" \
        --namespace "kars-${SCENARIO_SANDBOX}" \
        --timeout=300s || { log "ERR deployment not Available in time"; exit 3; }

    if [ "${SKIP_SANDBOX_RESTART:-0}" != "1" ]; then
        # Also wait for sub-agent deployments to settle after the
        # rollout-restart so they are reachable by the time the parent
        # spawns its first peer message.
        for name in "${SCENARIO_SUB_SANDBOXES[@]}"; do
            local ns="kars-${name}"
            kubectl get deploy -n "${ns}" "${name}" >/dev/null 2>&1 || continue
            kubectl rollout status -n "${ns}" "deploy/${name}" \
                --timeout=300s >>"${OUT_DIR}/apply.log" 2>&1 || true
        done
    fi
    log "sandbox Ready"
}

platform_post_prompt() {
    log "posting ${SCENARIO} prompt to ${SCENARIO_SANDBOX} gateway"

    # Hermes runtime has no HTTP gateway on port 18789 — the daemon
    # only accepts inputs through messaging channels (telegram etc.).
    # Scenarios targeting Hermes set SCENARIO_PROMPT_DRIVER=hermes-exec
    # to invoke `hermes -z` (one-shot agent mode) via `kubectl exec
    # -c agent`. The exec-ban VAP only targets the literal container
    # name `openclaw`, so this is policy-compliant.
    if [ "${SCENARIO_PROMPT_DRIVER:-port-forward}" = "hermes-exec" ]; then
        _platform_post_prompt_hermes_exec
        return $?
    fi

    # Operator-mode delivery, matching `kars connect`'s security model:
    #   1. Read the gateway-token Secret (RBAC-gated, namespaced).
    #   2. `kubectl port-forward` deploy/<name> :18789.
    #   3. POST the prompt to `/v1/chat/completions` with bearer auth.
    #
    # We must NOT `kubectl exec -c openclaw` — that's blocked by the
    # `kars-sandbox-exec-ban` ValidatingAdmissionPolicy by design.
    local ns="kars-${SCENARIO_SANDBOX}"
    local local_port="${GATEWAY_LOCAL_PORT:-28789}"

    log "fetching gateway token from Secret ${ns}/gateway-token"
    local token_b64
    token_b64=$(kubectl get secret -n "${ns}" gateway-token \
        -o jsonpath='{.data.token}' 2>/dev/null || true)
    if [ -z "${token_b64}" ]; then
        log "ERR gateway-token Secret missing or empty in ${ns}"; exit 4
    fi
    local gateway_token
    gateway_token=$(printf '%s' "${token_b64}" | base64 -d | tr -d '\n')
    if [ -z "${gateway_token}" ]; then
        log "ERR gateway token decoded to empty string"; exit 4
    fi

    log "starting kubectl port-forward localhost:${local_port} → ${ns}/${SCENARIO_SANDBOX}:18789"
    kubectl port-forward -n "${ns}" "deploy/${SCENARIO_SANDBOX}" \
        "${local_port}:18789" \
        > "${OUT_DIR}/port-forward.log" 2>&1 &
    local pf_pid=$!
    # shellcheck disable=SC2064
    trap "kill ${pf_pid} 2>/dev/null || true" EXIT INT TERM

    local i=0
    while [ $i -lt 30 ]; do
        if curl -sf --max-time 1 "http://127.0.0.1:${local_port}/healthz" \
                > /dev/null 2>&1; then
            break
        fi
        i=$((i+1)); sleep 1
    done
    if [ $i -ge 30 ]; then
        log "ERR port-forward never started serving HTTP on localhost:${local_port}"
        cat "${OUT_DIR}/port-forward.log" >&2 || true
        exit 4
    fi
    log "gateway reachable at localhost:${local_port}"

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
            "http://127.0.0.1:${local_port}/v1/chat/completions" \
        | tee "${OUT_DIR}/response.json"
    local rc=${PIPESTATUS[0]}
    kill "${pf_pid}" 2>/dev/null || true

    if [ "${rc}" -eq 124 ]; then
        log "ERR prompt timed out after ${WATCHDOG_SECS}s"; exit 4
    elif [ "${rc}" -ne 0 ]; then
        log "ERR gateway request failed rc=${rc}"; exit 4
    fi

    # Extract the assistant text into transcript.log for verify.py.
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

# Hermes-aware prompt driver. Used when SCENARIO_PROMPT_DRIVER=hermes-exec.
# Pre-step: if SCENARIO_DAEMON_SUB / SCENARIO_DAEMON_SCRIPT are set, copy
# the daemon source into the sub-sandbox, start it in the background,
# and wait for SCENARIO_DAEMON_READY_MARKER on its stdout before posting
# the parent prompt.
_platform_post_prompt_hermes_exec() {
    local parent_ns="kars-${SCENARIO_SANDBOX}"
    local parent_pod
    # Same Running+Ready guard as the sub-pod selection below — picks
    # a fresh replicaset member after a rollout-restart instead of
    # latching onto a Terminating pod that's about to disappear.
    local parent_wait=0
    while [ $parent_wait -lt 60 ]; do
        parent_pod=$(kubectl get pod -n "${parent_ns}" \
            -l "kars.azure.com/sandbox=${SCENARIO_SANDBOX}" \
            --field-selector=status.phase=Running \
            -o jsonpath='{.items[0].metadata.name}' 2>/dev/null)
        if [ -n "${parent_pod}" ]; then
            local ready
            ready=$(kubectl get pod -n "${parent_ns}" "${parent_pod}" \
                -o jsonpath='{.status.containerStatuses[?(@.name=="agent")].ready}' 2>/dev/null)
            if [ "${ready}" = "true" ]; then
                break
            fi
        fi
        sleep 1
        parent_wait=$((parent_wait+1))
    done
    if [ -z "${parent_pod}" ]; then
        log "ERR parent pod not found in ${parent_ns} (timed out after 60s waiting for Running+Ready)"; exit 4
    fi

    # ── Optional: start the echo / responder daemon on the sub-sandbox ──
    local daemon_pid=""
    if [ -n "${SCENARIO_DAEMON_SUB:-}" ] && [ -n "${SCENARIO_DAEMON_SCRIPT:-}" ]; then
        local sub_ns="kars-${SCENARIO_DAEMON_SUB}"
        local sub_pod
        # Filter on field-selector=status.phase=Running so a rollout-
        # restart-in-flight Terminating replica doesn't get picked as
        # `.items[0]` — exec against a Terminating pod returns
        # `container not found ("agent")` mid-cp and aborts the
        # harness. Wait up to 60s for a fresh Running pod after the
        # platform's rollout-restart.
        local sub_wait=0
        while [ $sub_wait -lt 60 ]; do
            sub_pod=$(kubectl get pod -n "${sub_ns}" \
                -l "kars.azure.com/sandbox=${SCENARIO_DAEMON_SUB}" \
                --field-selector=status.phase=Running \
                -o jsonpath='{.items[0].metadata.name}' 2>/dev/null)
            if [ -n "${sub_pod}" ]; then
                # Also confirm the agent container is reporting Ready,
                # not just the pod phase. A pod can be phase=Running
                # while its init/agent container is still spinning up.
                local ready
                ready=$(kubectl get pod -n "${sub_ns}" "${sub_pod}" \
                    -o jsonpath='{.status.containerStatuses[?(@.name=="agent")].ready}' 2>/dev/null)
                if [ "${ready}" = "true" ]; then
                    break
                fi
            fi
            sleep 1
            sub_wait=$((sub_wait+1))
        done
        if [ -z "${sub_pod}" ]; then
            log "ERR daemon sub pod not found in ${sub_ns} (timed out after 60s waiting for Running+Ready)"; exit 4
        fi
        local script_path="${SCENARIO_DIR}/${SCENARIO_DAEMON_SCRIPT}"
        if [ ! -f "${script_path}" ]; then
            log "ERR daemon script not found: ${script_path}"; exit 4
        fi
        log "copying ${SCENARIO_DAEMON_SCRIPT} → ${sub_ns}/${sub_pod}"
        local b64
        b64=$(base64 -i "${script_path}" | tr -d '\n')
        # The agent container has no /tmp/tar; push via python3 + base64.
        kubectl exec -n "${sub_ns}" "${sub_pod}" -c agent -- python3 -c \
            "import base64; open('/tmp/scenario_daemon.py','wb').write(base64.b64decode('${b64}'))" \
            >/dev/null
        # Wipe any stale identity from a previous run so DID derives fresh.
        kubectl exec -n "${sub_ns}" "${sub_pod}" -c agent -- \
            rm -f /sandbox/.agt/identity.json /sandbox/.hermes/.agt/identity.json \
            >/dev/null 2>&1 || true
        local daemon_log="${OUT_DIR}/daemon-${SCENARIO_DAEMON_SUB}.log"
        log "starting daemon on ${sub_ns}/${sub_pod} → ${daemon_log}"
        (
            kubectl exec -n "${sub_ns}" "${sub_pod}" -c agent -- \
                env "NAME=${SCENARIO_DAEMON_SUB}" python3 /tmp/scenario_daemon.py
        ) >"${daemon_log}" 2>&1 &
        daemon_pid=$!
        # shellcheck disable=SC2064
        trap "kill ${daemon_pid} 2>/dev/null || true" EXIT INT TERM

        local marker="${SCENARIO_DAEMON_READY_MARKER:-READY}"
        log "waiting for daemon marker '${marker}' (max 60s)"
        local i=0
        while [ $i -lt 60 ]; do
            if grep -qE "${marker}" "${daemon_log}" 2>/dev/null; then
                break
            fi
            sleep 1
            i=$((i+1))
        done
        if [ $i -ge 60 ]; then
            log "ERR daemon never produced '${marker}' marker"
            tail -20 "${daemon_log}" >&2 || true
            exit 4
        fi
        log "daemon ready"

        # Also wipe parent's stale identity so its fresh hermes -z run
        # derives a fresh DID and avoids the 409-already-registered path
        # being the FIRST request the harness sees.
        kubectl exec -n "${parent_ns}" "${parent_pod}" -c agent -- \
            rm -f /sandbox/.agt/identity.json /sandbox/.hermes/.agt/identity.json \
            >/dev/null 2>&1 || true
    fi

    # ── Drive the parent with `hermes -z <prompt>` ──
    log "running hermes -z on ${parent_ns}/${parent_pod}"
    local transcript="${OUT_DIR}/transcript.log"
    : >"${transcript}"
    local prompt_text
    prompt_text=$(cat "${PROMPT_FILE}")
    # HERMES_HOME and HOME must both be set explicitly — kubectl exec
    # does NOT inherit the container's image-level ENV by default, so
    # the entrypoint's HERMES_HOME=/sandbox/.hermes export is gone.
    # Hermes' ensure_hermes_home() falls back to ``$HOME/.hermes`` and
    # the running container's HOME defaults to `/` (rootfs is read-only,
    # so mkdir of `/.hermes` ENOENTs). Setting HOME=/sandbox and
    # HERMES_HOME=/sandbox/.hermes routes Hermes into the writable
    # sandbox volume — same path the long-running gateway daemon uses
    # under PID 1.
    run_with_watchdog "${WATCHDOG_SECS}" \
        kubectl exec -n "${parent_ns}" "${parent_pod}" -c agent -- \
            env HOME=/sandbox \
                HERMES_HOME=/sandbox/.hermes \
                HERMES_PROFILE="${SCENARIO_SANDBOX}" \
            hermes -z "${prompt_text}" \
        | tee -a "${transcript}"
    local rc=${PIPESTATUS[0]}

    if [ "${rc}" -eq 124 ]; then
        log "ERR hermes -z timed out after ${WATCHDOG_SECS}s"
        [ -n "${daemon_pid}" ] && kill "${daemon_pid}" 2>/dev/null
        exit 4
    elif [ "${rc}" -ne 0 ]; then
        log "ERR hermes -z exited rc=${rc}"
        [ -n "${daemon_pid}" ] && kill "${daemon_pid}" 2>/dev/null
        exit 4
    fi

    if [ -n "${daemon_pid}" ]; then
        log "stopping daemon (pid=${daemon_pid})"
        kill "${daemon_pid}" 2>/dev/null || true
        wait "${daemon_pid}" 2>/dev/null || true
    fi
    log "prompt completed — transcript at ${transcript}"
}

platform_collect_artifacts() {
    # Capture sub-agent gateway logs and writer's incoming/ directory listing.
    # The interesting plugin chatter (mesh_transfer_file, file_transfer_ack,
    # foundry_image_generation) lives in /tmp/gateway.log inside each
    # sandbox's openclaw container — NOT in kubectl logs stdout. We use the
    # `kars.azure.com/break-glass=true` namespace label to bypass the
    # exec-ban ValidatingAdmissionPolicy briefly, then remove the label.
    if [ "${#SCENARIO_SUB_SANDBOXES[@]}" -eq 0 ] \
       && [ -z "${SCENARIO_INCOMING_SANDBOX}" ]; then
        log "scenario has no sub-agents or incoming dirs declared; skipping collect"
        return 0
    fi

    log "collecting post-run artifacts (gateway tails + incoming dir if any)"
    for sub in "${SCENARIO_SUB_SANDBOXES[@]}" "${SCENARIO_INCOMING_SANDBOX}"; do
        [ -z "$sub" ] && continue
        kubectl label namespace "kars-${sub}" \
            kars.azure.com/break-glass=true --overwrite \
            >/dev/null 2>&1 || true
    done
    sleep 2  # let admission refresh

    for sub in "${SCENARIO_SUB_SANDBOXES[@]}"; do
        local pod
        pod=$(kubectl get pod -n "kars-${sub}" \
            -l "kars.azure.com/sandbox=${sub}" \
            -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || echo "")
        [ -z "$pod" ] && continue
        # Resolve the scenario's per-sub grep pattern (array name is
        # SCENARIO_GREP_PATTERNS_<subname>).
        local arr_name="SCENARIO_GREP_PATTERNS_${sub}"
        local -n patterns_ref="${arr_name}" 2>/dev/null || patterns_ref=()
        local pat="${patterns_ref[0]:-mesh_transfer_file|file_transfer_ack}"

        # Container name and gateway-log path differ across runtimes:
        #   - OpenClaw: container=openclaw, log=/tmp/gateway.log
        #   - Hermes:   container=agent,    log=/sandbox/.hermes/logs/gateway.log
        # The scenario's SCENARIO_RUNTIME hint (set in config.sh) tells
        # us which to use; absent it, default to the OpenClaw layout so
        # pre-Hermes scenarios are unaffected.
        local sub_container sub_log_path
        case "${SCENARIO_RUNTIME:-openclaw}" in
            hermes)
                sub_container="agent"
                sub_log_path="/sandbox/.hermes/logs/gateway.log"
                ;;
            *)
                sub_container="openclaw"
                sub_log_path="/tmp/gateway.log"
                ;;
        esac
        kubectl exec -n "kars-${sub}" "$pod" -c "${sub_container}" -- \
            sh -c "grep -E '${pat}' '${sub_log_path}' 2>/dev/null || true" \
            >"${OUT_DIR}/${sub}-gateway.log" || true
    done

    if [ -n "${SCENARIO_INCOMING_SANDBOX}" ] \
       && [ -n "${SCENARIO_INCOMING_PATH}" ]; then
        local pod
        pod=$(kubectl get pod -n "kars-${SCENARIO_INCOMING_SANDBOX}" \
            -l "kars.azure.com/sandbox=${SCENARIO_INCOMING_SANDBOX}" \
            -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || echo "")
        if [ -n "$pod" ]; then
            kubectl exec -n "kars-${SCENARIO_INCOMING_SANDBOX}" "$pod" \
                -c openclaw -- ls -la "${SCENARIO_INCOMING_PATH}" 2>/dev/null \
                >"${OUT_DIR}/${SCENARIO_INCOMING_SANDBOX}-incoming.txt" || true

            # Copy any image artefacts (hero/scorecard PNGs etc.) the
            # writer received into OUT_DIR so render_html.py can rewrite
            # the markdown image refs to local files. Use `tar` over
            # `kubectl exec` to avoid the per-file `kubectl cp` round
            # trips and to skip files the writer hasn't produced (the
            # incoming/ dir may legitimately be empty for some scenarios).
            local incoming_clean="${SCENARIO_INCOMING_PATH%/}"
            kubectl exec -n "kars-${SCENARIO_INCOMING_SANDBOX}" "$pod" \
                -c openclaw -- sh -c \
                "cd '${incoming_clean}' 2>/dev/null && tar cf - *.png *.jpg *.jpeg *.gif *.svg 2>/dev/null" \
                2>/dev/null \
                | tar xf - -C "${OUT_DIR}" 2>/dev/null || true
        fi
    fi

    # ── Pull the final delivered artifact (e.g. parent's brief.md) ──
    # The parent agent's transcript can be content_filter-truncated by
    # Foundry on a long verbatim echo; the artifact in the parent's
    # incoming/ dir is the ground truth of what was actually delivered
    # over the mesh. Lands in OUT_DIR as `final-artifact.<ext>` so
    # verify.py can pick it up regardless of scenario.
    if [ -n "${SCENARIO_FINAL_ARTIFACT_SANDBOX:-}" ] \
       && [ -n "${SCENARIO_FINAL_ARTIFACT_PATH:-}" ]; then
        # Re-apply break-glass on the parent's namespace (it wasn't in
        # the sub-loop above; the parent isn't a sub-agent).
        kubectl label namespace "kars-${SCENARIO_FINAL_ARTIFACT_SANDBOX}" \
            kars.azure.com/break-glass=true --overwrite \
            >/dev/null 2>&1 || true
        sleep 2
        local parent_pod
        parent_pod=$(kubectl get pod -n "kars-${SCENARIO_FINAL_ARTIFACT_SANDBOX}" \
            -l "kars.azure.com/sandbox=${SCENARIO_FINAL_ARTIFACT_SANDBOX}" \
            -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || echo "")
        if [ -n "$parent_pod" ]; then
            local ext="${SCENARIO_FINAL_ARTIFACT_PATH##*.}"
            [ "$ext" = "${SCENARIO_FINAL_ARTIFACT_PATH}" ] && ext="md"
            kubectl exec -n "kars-${SCENARIO_FINAL_ARTIFACT_SANDBOX}" \
                "$parent_pod" -c openclaw -- \
                cat "${SCENARIO_FINAL_ARTIFACT_PATH}" 2>/dev/null \
                >"${OUT_DIR}/final-artifact.${ext}" || true
            # Empty file means the parent never received the artifact —
            # remove it so verify.py falls back to the transcript cleanly.
            [ -s "${OUT_DIR}/final-artifact.${ext}" ] \
                || rm -f "${OUT_DIR}/final-artifact.${ext}"
        fi
        kubectl label namespace "kars-${SCENARIO_FINAL_ARTIFACT_SANDBOX}" \
            kars.azure.com/break-glass- \
            >/dev/null 2>&1 || true
    fi

    for sub in "${SCENARIO_SUB_SANDBOXES[@]}" "${SCENARIO_INCOMING_SANDBOX}"; do
        [ -z "$sub" ] && continue
        kubectl label namespace "kars-${sub}" \
            kars.azure.com/break-glass- \
            >/dev/null 2>&1 || true
    done

    # ── Build trace.jsonl that verify.py's check suite consumes ──────────
    # We dump time-windowed kubectl logs for every source verify.py is
    # interested in (ROUTER, RELAY, CTRL, POD-*) as JSONL lines tagged
    # with `src`. The window starts at RUN_START_TS (captured by drive.sh
    # before manifests were applied), so log noise from prior runs on the
    # same long-lived pods is excluded — this is the key fix for the
    # router_lines-empty drift that made image/MCP/code-exec checks fall
    # to 0 despite the run actually succeeding.
    local trace="${OUT_DIR}/trace.jsonl"
    : >"${trace}"
    _emit_logs_as_trace() {
        local src="$1" ns="$2" selector="$3"
        kubectl logs -n "${ns}" "${selector}" \
            --since-time="${RUN_START_TS}" \
            --all-containers=true --prefix=false --tail=-1 \
            2>/dev/null \
        | python3 -c '
import json, sys
src = sys.argv[1]
for line in sys.stdin:
    line = line.rstrip("\n")
    if not line:
        continue
    print(json.dumps({"src": src, "msg": line}))
' "${src}" >>"${trace}" || true
    }
    # Per-sandbox routers (each sandbox has its own inference-router sidecar)
    for s in "${SCENARIO_SANDBOX}" "${SCENARIO_SUB_SANDBOXES[@]}"; do
        [ -z "$s" ] && continue
        _emit_logs_as_trace "ROUTER" "kars-${s}" "deploy/${s}"
    done
    # Cluster-shared services
    _emit_logs_as_trace "RELAY" "agentmesh" "deploy/agentmesh-relay"
    _emit_logs_as_trace "REGISTRY" "agentmesh" "deploy/agentmesh-registry"
    _emit_logs_as_trace "CTRL" "kars-system" "deploy/kars-controller"
    log "trace.jsonl assembled ($(wc -l <"${trace}" | tr -d ' ') lines)"
    log "artifacts collected"
}
