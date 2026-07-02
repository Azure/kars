#!/bin/sh
# Kars datapath-witness aggregator loop.
#
# Continuously snapshots the kernel-observed egress from the persistent (headless)
# Inspektor Gadget instances created by install.sh --continuous, cross-checks it
# against each sandbox's controller-declared egress allowlist, and publishes a
# per-sandbox verdict to the `kars-datapath-witness` ConfigMap in kars-system.
#
# The Bridge (and any consumer) then just reads that ConfigMap — no eBPF/gadget
# dependency in the reader. On a plain Kars cluster with no Bridge:
#   kubectl -n kars-system get cm kars-datapath-witness -o jsonpath='{.data.witness\.json}'
set -eu

CM_NS="${WITNESS_CM_NAMESPACE:-kars-system}"
CM_NAME="${WITNESS_CM_NAME:-kars-datapath-witness}"
WINDOW="${WITNESS_WINDOW:-15}"
INTERVAL="${WITNESS_INTERVAL:-30}"
DNS_INSTANCE="${WITNESS_DNS_INSTANCE:-kars-witness-dns}"
TCP_INSTANCE="${WITNESS_TCP_INSTANCE:-kars-witness-tcp}"
GADGET_NS="${WITNESS_GADGET_NAMESPACE:-gadget}"

export WITNESS_WINDOW="$WINDOW"

echo "kars datapath-witness aggregator starting (window=${WINDOW}s interval=${INTERVAL}s -> ${CM_NS}/${CM_NAME})"

while true; do
  : >/tmp/dns.json
  : >/tmp/tcp.json

  # Snapshot both continuous instances concurrently for one window. `attach`
  # replays the server-side event buffer then streams; `timeout` bounds it.
  timeout "$WINDOW" kubectl-gadget attach "$DNS_INSTANCE" --gadget-namespace "$GADGET_NS" -o json >/tmp/dns.json 2>/dev/null &
  timeout "$WINDOW" kubectl-gadget attach "$TCP_INSTANCE" --gadget-namespace "$GADGET_NS" -o json >/tmp/tcp.json 2>/dev/null &
  wait 2>/dev/null || true

  # Compute the verdict (reads declared allowlists via kubectl using our SA).
  if DNS_FILE=/tmp/dns.json TCP_FILE=/tmp/tcp.json python3 /opt/witness/compute.py >/tmp/witness.json 2>/tmp/compute.err; then
    kubectl create configmap "$CM_NAME" -n "$CM_NS" \
      --from-file=witness.json=/tmp/witness.json \
      --dry-run=client -o yaml | kubectl apply -f - >/dev/null 2>&1 || true
    kubectl label configmap "$CM_NAME" -n "$CM_NS" --overwrite \
      app.kubernetes.io/managed-by=kars-datapath-witness \
      app.kubernetes.io/part-of=kars >/dev/null 2>&1 || true
    echo "$(date -u +%FT%TZ) published witness ($(wc -c </tmp/witness.json) bytes)"
  else
    echo "$(date -u +%FT%TZ) compute failed:"; sed 's/^/  /' /tmp/compute.err || true
  fi

  sleep "$INTERVAL"
done
