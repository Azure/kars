#!/usr/bin/env bash
# Kars eBPF datapath-completeness witness — gated installer.
#
# Installs Inspektor Gadget (CNCF, eBPF) so the witness can observe what Kars
# sandboxes actually send on the network at the kernel level, independently of
# the router. OPTIONAL and OFF BY DEFAULT: a plain Kars cluster does not need
# this. Requires explicit opt-in via KARS_EBPF_WITNESS=1 so it never installs
# a privileged DaemonSet silently.
#
# Usage:
#   KARS_EBPF_WITNESS=1 deploy/ebpf-witness/install.sh
#   KARS_EBPF_WITNESS=1 deploy/ebpf-witness/install.sh --continuous
set -euo pipefail

GADGET_NS="${KARS_GADGET_NAMESPACE:-gadget}"
IG_VERSION="${KARS_IG_VERSION:-v0.53.2}"
CONTINUOUS=0
for arg in "$@"; do
  case "$arg" in
    --continuous) CONTINUOUS=1 ;;
    -h|--help)
      grep '^#' "$0" | sed 's/^# \{0,1\}//' | sed '/^!/d'; exit 0 ;;
    *) echo "unknown arg: $arg" >&2; exit 2 ;;
  esac
done

if [[ "${KARS_EBPF_WITNESS:-}" != "1" ]]; then
  cat >&2 <<'EOF'
refusing to install: the eBPF datapath witness is optional and off by default.
It installs a PRIVILEGED Inspektor Gadget DaemonSet (eBPF). Review
deploy/ebpf-witness/README.md, then opt in explicitly:

  KARS_EBPF_WITNESS=1 deploy/ebpf-witness/install.sh [--continuous]
EOF
  exit 1
fi

need() { command -v "$1" >/dev/null 2>&1 || { echo "missing required tool: $1" >&2; exit 1; }; }
need kubectl

echo "==> preflight: kernel BTF (eBPF CO-RE) on nodes"
# Best-effort: warn (don't hard-fail) if we can't introspect the node kernel.
if kubectl get nodes -o name >/dev/null 2>&1; then
  echo "    (Inspektor Gadget itself validates per-node eBPF support at deploy time.)"
else
  echo "    WARN: cannot list nodes; ensure kubeconfig points at the target cluster." >&2
fi

# ---- kubectl gadget client -------------------------------------------------
GADGET_BIN=""
if command -v kubectl-gadget >/dev/null 2>&1; then
  GADGET_BIN="kubectl-gadget"
elif kubectl gadget version >/dev/null 2>&1; then
  GADGET_BIN="kubectl gadget"
else
  echo "==> installing kubectl gadget client ${IG_VERSION}"
  os="$(uname -s | tr '[:upper:]' '[:lower:]')"
  arch="$(uname -m)"; case "$arch" in x86_64|amd64) arch=amd64;; arm64|aarch64) arch=arm64;; esac
  tmp="$(mktemp -d)"
  url="https://github.com/inspektor-gadget/inspektor-gadget/releases/download/${IG_VERSION}/kubectl-gadget-${os}-${arch}-${IG_VERSION}.tar.gz"
  echo "    fetching $url"
  curl -sSL "$url" -o "$tmp/ig.tgz"
  tar -xzf "$tmp/ig.tgz" -C "$tmp" kubectl-gadget
  dest="${KARS_GADGET_BIN_DIR:-/usr/local/bin}"
  if install -m 0755 "$tmp/kubectl-gadget" "$dest/kubectl-gadget" 2>/dev/null; then
    echo "    installed to $dest/kubectl-gadget"
  else
    dest="$HOME/.local/bin"; mkdir -p "$dest"
    install -m 0755 "$tmp/kubectl-gadget" "$dest/kubectl-gadget"
    echo "    installed to $dest/kubectl-gadget (add it to PATH)"
    export PATH="$dest:$PATH"
  fi
  rm -rf "$tmp"
  GADGET_BIN="kubectl-gadget"
fi
echo "    using client: $GADGET_BIN"

# ---- deploy the DaemonSet --------------------------------------------------
echo "==> deploying Inspektor Gadget DaemonSet into namespace '${GADGET_NS}'"
$GADGET_BIN deploy --gadget-namespace "${GADGET_NS}"

# ---- optional continuous (headless) witness + aggregator -------------------
if [[ "$CONTINUOUS" == "1" ]]; then
  echo "==> creating continuous (headless) witness instances"
  # trace_dns = host intent; trace_tcp = actual outbound datapath. An event
  # buffer lets the aggregator replay recent events each cycle via `attach`.
  BUFLEN="${KARS_WITNESS_BUFFER:-4000}"
  # Recreate idempotently (delete-if-exists, then create).
  for inst in kars-witness-dns kars-witness-tcp; do
    $GADGET_BIN delete "$inst" --gadget-namespace "${GADGET_NS}" >/dev/null 2>&1 || true
  done
  $GADGET_BIN run trace_dns:latest -A --detach --event-buffer-length "${BUFLEN}" \
    --gadget-namespace "${GADGET_NS}" --name kars-witness-dns >/dev/null
  $GADGET_BIN run trace_tcp:latest -A --detach --event-buffer-length "${BUFLEN}" \
    --gadget-namespace "${GADGET_NS}" --name kars-witness-tcp >/dev/null
  echo "    headless instances created: kars-witness-dns, kars-witness-tcp"

  # ---- aggregator: publishes the verdict ConfigMap the Bridge reads --------
  here="$(cd "$(dirname "$0")" && pwd)"
  AGG_IMAGE="${KARS_WITNESS_AGGREGATOR_IMAGE:-kars-datapath-witness-aggregator:dev}"
  echo "==> building aggregator image ${AGG_IMAGE}"
  arch="$(uname -m)"; case "$arch" in x86_64|amd64) darch=amd64;; arm64|aarch64) darch=arm64;; *) darch=amd64;; esac
  if command -v docker >/dev/null 2>&1; then
    docker build --platform "linux/${darch}" --build-arg "TARGETARCH=${darch}" \
      -t "${AGG_IMAGE}" -f "${here}/aggregator/Dockerfile" "${here}/aggregator"
    # Load into a kind cluster when the current context is kind-*.
    ctx="$(kubectl config current-context 2>/dev/null || true)"
    if [[ -n "${KARS_WITNESS_KIND_CLUSTER:-}" ]] && command -v kind >/dev/null 2>&1; then
      kind load docker-image "${AGG_IMAGE}" --name "${KARS_WITNESS_KIND_CLUSTER}"
    elif [[ "$ctx" == kind-* ]] && command -v kind >/dev/null 2>&1; then
      kind load docker-image "${AGG_IMAGE}" --name "${ctx#kind-}"
    else
      echo "    NOTE: push ${AGG_IMAGE} to your cluster's registry (non-kind cluster)," >&2
      echo "          or set KARS_WITNESS_AGGREGATOR_IMAGE to a reachable image." >&2
    fi
  else
    echo "    WARN: docker not found — build+load ${AGG_IMAGE} yourself before the aggregator runs." >&2
  fi

  echo "==> deploying witness aggregator (script ConfigMap + RBAC + Deployment)"
  kubectl create configmap kars-witness-aggregator-script -n "${GADGET_NS}" \
    --from-file=publish-witness.sh="${here}/aggregator/publish-witness.sh" \
    --from-file=compute.py="${here}/aggregator/compute.py" \
    --dry-run=client -o yaml | kubectl apply -f - >/dev/null
  kubectl apply -f "${here}/aggregator.yaml" >/dev/null
  kubectl rollout status deploy/kars-witness-aggregator -n "${GADGET_NS}" --timeout=120s || true
  echo "    aggregator publishing kars-system/kars-datapath-witness every ~30s"
  echo "    read:   kubectl -n kars-system get cm kars-datapath-witness -o jsonpath='{.data.witness\\.json}'"
fi

cat <<EOF

eBPF datapath witness installed.

Produce a per-sandbox completeness verdict:
  deploy/ebpf-witness/witness-verify.sh            # table
  deploy/ebpf-witness/witness-verify.sh --json     # machine-readable

Uninstall:
  deploy/ebpf-witness/uninstall.sh
EOF
