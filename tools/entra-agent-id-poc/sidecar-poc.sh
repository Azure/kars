#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# POC: run the GA Microsoft Entra SDK for Agent ID as a sidecar container
# and probe its HTTP API. Measures startup + token-acquisition latency so
# we can decide whether kars should adopt the sidecar pattern (vs the
# current `entrypoint.sh:158-235` raw token-exchange bash).
#
# This is INTENTIONALLY hermetic: no Entra credentials configured. The
# sidecar will start up, expose /health, and reject token calls with a
# clear error. The point is to measure the sidecar's startup wall-clock
# and HTTP surface — not to perform a successful token exchange (which
# requires the tenant-side agent identity blueprint we don't have).
#
# Usage:
#   ./sidecar-poc.sh                # full lifecycle + report
#   ./sidecar-poc.sh --keep         # leave the container running
#   docker rm -f kars-entra-sidecar-poc   # cleanup after --keep
set -euo pipefail

KEEP=0
for arg in "$@"; do
    case "$arg" in
        --keep) KEEP=1 ;;
        *) echo "unknown arg: $arg" >&2; exit 2 ;;
    esac
done

IMAGE="mcr.microsoft.com/entra-sdk/auth-sidecar:1.0.0-azurelinux3.0-distroless"
NAME="kars-entra-sidecar-poc"
PORT=15500  # avoid macOS AirPlay (5000) + Headlamp (4466) + sandbox WebUI (18789)

red()    { printf "\033[31m%s\033[0m\n" "$*"; }
green()  { printf "\033[32m%s\033[0m\n" "$*"; }
yellow() { printf "\033[33m%s\033[0m\n" "$*"; }
blue()   { printf "\033[34m%s\033[0m\n" "$*"; }
bold()   { printf "\033[1m%s\033[0m\n" "$*"; }

bold "═══ Entra SDK for Agent ID sidecar POC ($(date)) ═══"
echo

# Cleanup any leftover from prior runs
docker rm -f "$NAME" >/dev/null 2>&1 || true

# ── Phase 1: pull (verify cache hit) ─────────────────────────────
bold "Phase 1: pull sidecar image"
START=$(date +%s.%N)
docker pull "$IMAGE" 2>&1 | tail -3
END=$(date +%s.%N)
T_PULL=$(awk "BEGIN { printf \"%.2f\", $END - $START }")
SIZE=$(docker image inspect "$IMAGE" --format '{{.Size}}' | awk '{ printf "%.1f MB", $1/1024/1024 }')
green "  ✓ image ready in ${T_PULL}s ($SIZE)"
echo

# ── Phase 2: start sidecar with dummy config ─────────────────────
#
# In production kars, these env vars would be set by the controller
# from a per-sandbox Entra agent identity. For the POC we use obvious
# placeholders so any token request fails with a clear identity-error
# (not a config-error), which is the data point we need.
bold "Phase 2: start sidecar"
START=$(date +%s.%N)
docker run -d \
    --name "$NAME" \
    -p "${PORT}:8080" \
    -e "AzureAd__TenantId=00000000-0000-0000-0000-000000000000" \
    -e "AzureAd__ClientId=11111111-1111-1111-1111-111111111111" \
    -e "AzureAd__Instance=https://login.microsoftonline.com/" \
    "$IMAGE" >/dev/null
END=$(date +%s.%N)
T_DOCKER_RUN=$(awk "BEGIN { printf \"%.2f\", $END - $START }")
green "  ✓ docker run returned in ${T_DOCKER_RUN}s"
echo

# ── Phase 3: wait for /health to become ready ─────────────────────
bold "Phase 3: wait for /health"
START=$(date +%s.%N)
HEALTH_OK=0
for attempt in {1..60}; do
    if HEALTH=$(curl -sS --max-time 2 "http://localhost:${PORT}/health" 2>&1); then
        HEALTH_OK=1
        break
    fi
    sleep 0.5
done
END=$(date +%s.%N)
T_READY=$(awk "BEGIN { printf \"%.2f\", $END - $START }")
if [ "$HEALTH_OK" -eq 1 ]; then
    green "  ✓ /health responsive in ${T_READY}s after ${attempt} attempt(s)"
    echo "  body: $HEALTH"
else
    red "  ✗ /health did not respond within 30s"
    echo
    echo "--- last 30 log lines ---"
    docker logs "$NAME" 2>&1 | tail -30
    [ "$KEEP" -eq 0 ] && docker rm -f "$NAME" >/dev/null 2>&1
    exit 1
fi
echo

# ── Phase 4: list API surface (curl what's there) ────────────────
#
# The SDK docs describe `/AuthorizationHeader/{serviceName}` as the
# canonical endpoint. We probe a couple of paths to see what the
# container actually exposes.
bold "Phase 4: probe HTTP surface"
for path in / /openapi/v1.json /swagger /api /AuthorizationHeader/Graph /Validate; do
    CODE=$(curl -sS --max-time 3 -o /dev/null -w "%{http_code}" "http://localhost:${PORT}${path}" 2>&1)
    case "$CODE" in
        200) printf "  ${path}: \033[32m%s\033[0m\n" "$CODE" ;;
        400|401|403|404) printf "  ${path}: \033[33m%s\033[0m\n" "$CODE" ;;
        *)   printf "  ${path}: \033[31m%s\033[0m\n" "$CODE" ;;
    esac
done
echo

# ── Phase 5: try a real (but doomed) token request ────────────────
bold "Phase 5: probe /AuthorizationHeader/Graph"
START=$(date +%s.%N)
RESP=$(curl -sS --max-time 5 \
    "http://localhost:${PORT}/AuthorizationHeader/Graph?AgentIdentity=11111111-1111-1111-1111-111111111111" \
    2>&1 || true)
END=$(date +%s.%N)
T_TOKEN=$(awk "BEGIN { printf \"%.2f\", $END - $START }")
echo "  → call returned in ${T_TOKEN}s"
echo "  → response: $(echo "$RESP" | head -c 300)"
echo

# ── Phase 6: container resource usage ────────────────────────────
bold "Phase 6: container footprint"
docker stats --no-stream --format \
    "  cpu={{.CPUPerc}}  mem={{.MemUsage}}  pids={{.PIDs}}" \
    "$NAME" 2>&1 | head -3
echo

# ── Cleanup ──────────────────────────────────────────────────────
if [ "$KEEP" -eq 1 ]; then
    yellow "Container left running. Stop with:"
    yellow "  docker rm -f $NAME"
    echo
else
    docker rm -f "$NAME" >/dev/null 2>&1
    green "  ✓ cleanup complete"
    echo
fi

# ── Summary ──────────────────────────────────────────────────────
bold "═══ Summary ═══"
echo
printf "  %-50s %s\n" "Phase 1 — pull image (cached):"    "${T_PULL}s"
printf "  %-50s %s\n" "Phase 2 — docker run:"             "${T_DOCKER_RUN}s"
printf "  %-50s %s\n" "Phase 3 — /health responsive:"      "${T_READY}s"
printf "  %-50s %s\n" "Phase 5 — first token call latency:" "${T_TOKEN}s"
echo
yellow "kars integration implications:"
yellow "  • sidecar adds ~$SIZE per sandbox (one more container)"
yellow "  • cold-start to /health: ${T_READY}s (must fit under sandbox readiness probe)"
yellow "  • per-token-acquisition cost (cached creds): ${T_TOKEN}s"
yellow "  • replaces entrypoint.sh:158-235 (~80 LoC of token-exchange bash)"
