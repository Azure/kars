#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# Manual-E2E benchmarking layer.
#
# Emits one JSONL row per measured event to ${MANUAL_E2E_METRICS_FILE}
# (default: ${MANUAL_E2E_OUTDIR:-/tmp/kars-e2e-manual}/metrics.jsonl).
#
# The aggregator (`metrics_summary`) walks that file at end-of-run and
# prints a human-readable table plus min/avg/max/p50/p95 percentiles per
# metric name — so individual `run.sh` invocations can later be compared
# across PRs / cluster sizes / regions for regression tracking.
#
# Conventions:
#   metric=name           camelCase, e.g. ttiSandbox, ttrPodRunning,
#                         ttfrInference, admitKarsSandbox, cleanupNs,
#                         restartCount.
#   unit=ms|count|bytes   string. Default "ms".
#   tags={k=v,...}        free-form context (runtime, scenario, sandbox).
#
# Sample row:
#   {"ts":"2026-05-08T22:10:00Z","scenario":"runtime","metric":"ttiSandbox",
#    "unit":"ms","value":4521,"tags":{"runtime":"openclaw","sandbox":"manual-openclaw"}}

# ── Paths ───────────────────────────────────────────────────────────────
: "${MANUAL_E2E_OUTDIR:=/tmp/kars-e2e-manual}"
: "${MANUAL_E2E_METRICS_FILE:=${MANUAL_E2E_OUTDIR}/metrics.jsonl}"
: "${MANUAL_E2E_RUN_ID:=$(date +%Y%m%dT%H%M%S)-$$}"
export MANUAL_E2E_OUTDIR MANUAL_E2E_METRICS_FILE MANUAL_E2E_RUN_ID
mkdir -p "$MANUAL_E2E_OUTDIR" 2>/dev/null || true

# ── Time helpers ────────────────────────────────────────────────────────
# Millisecond-resolution wall-clock. macOS `date` lacks %N, so we probe
# once and cache the fastest available shell-out: GNU date %s%N ≫ perl
# Time::HiRes ≫ python3.
_metrics_now_ms() {
    case "${_METRICS_BACKEND:-}" in
        date) echo $(($(date +%s%N) / 1000000)); return ;;
        perl) perl -MTime::HiRes -e 'printf "%d\n", Time::HiRes::time*1000'; return ;;
        py)   python3 -c 'import time; print(int(time.time()*1000))'; return ;;
    esac
    if date +%s%N 2>/dev/null | grep -qE '^[0-9]+$'; then
        export _METRICS_BACKEND=date
    elif command -v perl >/dev/null 2>&1; then
        export _METRICS_BACKEND=perl
    else
        export _METRICS_BACKEND=py
    fi
    _metrics_now_ms
}

# Start a stopwatch under a label. Stash the start ms in a shell variable
# so we can finish without an extra file. Reusing labels overwrites.
metric_start() {
    local label="$1"
    local var
    var="_M_$(printf '%s' "$label" | tr -c '[:alnum:]' '_')"
    eval "${var}=$(_metrics_now_ms)"
}

# Finish a stopwatch and emit a JSONL row.
#   $1 = label (must match a prior metric_start)
#   $2 = scenario name (e.g. runtime, mesh, governance)
#   $3 = metric name (e.g. ttiSandbox, ttrPodRunning)
#   $4..= tags as k=v pairs
metric_finish() {
    local label="$1" scenario="$2" metric="$3"; shift 3
    local var
    var="_M_$(printf '%s' "$label" | tr -c '[:alnum:]' '_')"
    local start_ms="${!var:-}"
    if [[ -z "$start_ms" ]]; then
        return 0
    fi
    local now_ms duration_ms
    now_ms=$(_metrics_now_ms)
    duration_ms=$((now_ms - start_ms))
    metric_emit "$scenario" "$metric" ms "$duration_ms" "$@"
    unset "$var"
}

# Emit a single measurement directly (for counts / non-timed values).
#   $1 = scenario
#   $2 = metric
#   $3 = unit (ms|count|bytes|...)
#   $4 = numeric value
#   $5..= tags as k=v pairs
metric_emit() {
    local scenario="$1" metric="$2" unit="$3" value="$4"; shift 4
    local tags_json="" sep=""
    while (( $# > 0 )); do
        local kv="$1"; shift
        local k="${kv%%=*}" v="${kv#*=}"
        # Best-effort JSON escape: backslash + double-quote only.
        v="${v//\\/\\\\}"; v="${v//\"/\\\"}"
        tags_json+="${sep}\"${k}\":\"${v}\""
        sep=","
    done
    local ts
    ts=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    printf '{"ts":"%s","runId":"%s","scenario":"%s","metric":"%s","unit":"%s","value":%s,"tags":{%s}}\n' \
        "$ts" "$MANUAL_E2E_RUN_ID" "$scenario" "$metric" "$unit" "$value" "$tags_json" \
        >> "$MANUAL_E2E_METRICS_FILE"
}

# Aggregate the metrics file. Pure-bash + sort/awk: no Python dep.
# Prints a per-(scenario,metric) table with count / min / p50 / avg / p95
# / max in the chosen unit.
metrics_summary() {
    local file="${1:-$MANUAL_E2E_METRICS_FILE}"
    if [[ ! -s "$file" ]]; then
        return 0
    fi
    if ! command -v python3 >/dev/null 2>&1; then
        echo "  (python3 not available — raw metrics at ${file})"
        return 0
    fi
    python3 - "$file" <<'PY'
import json, sys, statistics
from collections import defaultdict

path = sys.argv[1]
buckets = defaultdict(list)
units = {}
with open(path) as f:
    for line in f:
        line = line.strip()
        if not line:
            continue
        try:
            row = json.loads(line)
        except Exception:
            continue
        key = (row.get("scenario","?"), row.get("metric","?"))
        buckets[key].append(row.get("value",0))
        units[key] = row.get("unit","ms")

if not buckets:
    sys.exit(0)

def pct(xs, p):
    xs = sorted(xs)
    if not xs:
        return 0
    k = max(0, min(len(xs)-1, int(round((p/100.0)*(len(xs)-1)))))
    return xs[k]

rows = []
for (scen, metric), vals in sorted(buckets.items()):
    rows.append([
        scen, metric, units[(scen,metric)], len(vals),
        min(vals), pct(vals,50),
        f"{statistics.fmean(vals):.0f}",
        pct(vals,95), max(vals),
    ])

cols = ["scenario","metric","unit","n","min","p50","avg","p95","max"]
widths = [max(len(str(r[i])) for r in [cols] + rows) for i in range(len(cols))]

def fmt(row):
    return "  " + "  ".join(str(v).ljust(widths[i]) for i,v in enumerate(row))

print()
print("──  Benchmark summary  ──────────────────────────────────────────────")
print(fmt(cols))
print("  " + "  ".join("-"*w for w in widths))
for r in rows:
    print(fmt(r))
PY
}
