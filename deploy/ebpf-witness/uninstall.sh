#!/usr/bin/env bash
# Kars eBPF datapath-completeness witness — uninstaller.
# Removes the continuous (headless) witness instances and the Inspektor Gadget
# DaemonSet. Safe to run whether or not --continuous was used at install time.
set -euo pipefail

GADGET_NS="${KARS_GADGET_NAMESPACE:-gadget}"

if [[ -n "${KARS_GADGET_BIN:-}" ]]; then GADGET=("${KARS_GADGET_BIN}")
elif command -v kubectl-gadget >/dev/null 2>&1; then GADGET=(kubectl-gadget)
elif kubectl gadget version >/dev/null 2>&1; then GADGET=(kubectl gadget)
else
  echo "Inspektor Gadget client not found; nothing to uninstall." >&2
  exit 0
fi

echo "==> removing continuous witness instances (if any)"
for name in kars-witness-dns kars-witness-tcp; do
  # `delete` by name is idempotent; ignore "not found".
  "${GADGET[@]}" delete "$name" --gadget-namespace "${GADGET_NS}" 2>/dev/null || true
done

echo "==> removing witness aggregator (Deployment + RBAC + script + verdict CM)"
here="$(cd "$(dirname "$0")" && pwd)"
kubectl delete -f "${here}/aggregator.yaml" --ignore-not-found 2>/dev/null || true
kubectl delete configmap kars-witness-aggregator-script -n "${GADGET_NS}" --ignore-not-found 2>/dev/null || true
kubectl delete configmap kars-datapath-witness -n kars-system --ignore-not-found 2>/dev/null || true

echo "==> removing Inspektor Gadget DaemonSet from namespace '${GADGET_NS}'"
"${GADGET[@]}" undeploy --gadget-namespace "${GADGET_NS}"

echo "eBPF datapath witness removed."
