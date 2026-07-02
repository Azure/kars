#!/usr/bin/env bash
# Kars eBPF datapath-completeness witness — verifier.
#
# Captures a bounded window of what Kars sandboxes actually send at the kernel
# (DNS host intent + outbound TCP connects) via Inspektor Gadget, then cross-
# checks it against the egress allowlist the controller declared for each
# sandbox (the `karssandbox-<name>-egress-allowlist` ConfigMap). Emits a
# per-sandbox completeness verdict.
#
# Self-contained: works whether or not the continuous (headless) instances from
# install.sh --continuous exist; it runs its own bounded capture. Reads only
# core Kars objects, so it runs on any Kars cluster with NO Kars-Bridge.
#
# Usage:
#   deploy/ebpf-witness/witness-verify.sh                 # human table
#   deploy/ebpf-witness/witness-verify.sh --json          # machine-readable
#   deploy/ebpf-witness/witness-verify.sh --window 30     # capture seconds (default 20)
#   deploy/ebpf-witness/witness-verify.sh --namespace kars-a,kars-b
set -euo pipefail

GADGET_NS="${KARS_GADGET_NAMESPACE:-gadget}"
WINDOW=20
JSON=0
NS_FILTER=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --window) WINDOW="$2"; shift 2 ;;
    --json) JSON=1; shift ;;
    --namespace|-n) NS_FILTER="$2"; shift 2 ;;
    -h|--help) grep '^#' "$0" | sed 's/^# \{0,1\}//' | sed '/^!/d'; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

command -v kubectl >/dev/null 2>&1 || { echo "missing required tool: kubectl" >&2; exit 1; }
command -v python3 >/dev/null 2>&1 || { echo "missing required tool: python3" >&2; exit 1; }

# Locate the gadget client (env override, PATH plugin, or `kubectl gadget`).
if [[ -n "${KARS_GADGET_BIN:-}" ]]; then GADGET=("${KARS_GADGET_BIN}")
elif command -v kubectl-gadget >/dev/null 2>&1; then GADGET=(kubectl-gadget)
elif kubectl gadget version >/dev/null 2>&1; then GADGET=(kubectl gadget)
else
  echo "Inspektor Gadget client not found. Install the witness first:" >&2
  echo "  KARS_EBPF_WITNESS=1 deploy/ebpf-witness/install.sh" >&2
  exit 1
fi

if ! kubectl get ns "${GADGET_NS}" >/dev/null 2>&1; then
  echo "Inspektor Gadget is not deployed (namespace '${GADGET_NS}' missing)." >&2
  echo "  KARS_EBPF_WITNESS=1 deploy/ebpf-witness/install.sh" >&2
  exit 1
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "==> witnessing the kernel datapath for ${WINDOW}s (DNS intent + TCP connects)..." >&2
# Run both traces concurrently for the same window.
"${GADGET[@]}" run trace_dns:latest -A --timeout "${WINDOW}" -o json \
  --gadget-namespace "${GADGET_NS}" >"$TMP/dns.json" 2>/dev/null &
dns_pid=$!
"${GADGET[@]}" run trace_tcp:latest -A --timeout "${WINDOW}" -o json \
  --gadget-namespace "${GADGET_NS}" >"$TMP/tcp.json" 2>/dev/null &
tcp_pid=$!
wait "$dns_pid" || true
wait "$tcp_pid" || true

DNS_FILE="$TMP/dns.json" TCP_FILE="$TMP/tcp.json" \
NS_FILTER="$NS_FILTER" EMIT_JSON="$JSON" \
python3 - <<'PY'
import json, os, re, subprocess, sys

def load_events(path):
    out = []
    try:
        with open(path) as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    v = json.loads(line)
                except Exception:
                    continue
                out.extend(v if isinstance(v, list) else [v])
    except FileNotFoundError:
        pass
    return out

def k8s_ns(e):
    k = e.get("k8s")
    if isinstance(k, dict):
        return k.get("namespace") or ""
    return e.get("namespace") or ""

INTERNAL_SUFFIXES = (".cluster.local", ".svc", ".in-addr.arpa", ".arpa", ".local")
INTERNAL_EXACT = {"kubernetes", "kubernetes.default", "localhost"}

def is_internal_host(h):
    h = h.rstrip(".").lower()
    if not h or h in INTERNAL_EXACT:
        return True
    if any(h.endswith(s) for s in INTERNAL_SUFFIXES):
        return True
    # bare single-label names are cluster-internal search-domain lookups
    if "." not in h:
        return True
    return False

def is_private_addr(a):
    a = a or ""
    if a.startswith(("10.", "127.", "169.254.", "192.168.")):
        return True
    if a.startswith("172."):
        try:
            second = int(a.split(".")[1])
            if 16 <= second <= 31:
                return True
        except Exception:
            pass
    if a in ("::1",) or a.startswith(("fc", "fd", "fe80")):
        return True
    return False

# ---- observed: DNS query names (external) per namespace --------------------
observed_dns = {}   # ns -> set(host)
for e in load_events(os.environ["DNS_FILE"]):
    ns = k8s_ns(e)
    name = (e.get("name") or "").rstrip(".")
    qr = e.get("qr")  # "Q" query / "R" response; count intent (queries)
    if not ns or not name:
        continue
    if qr == "R":
        continue
    if is_internal_host(name):
        continue
    observed_dns.setdefault(ns, set()).add(name.lower())

# ---- observed: external TCP connects per namespace -------------------------
observed_connects = {}  # ns -> count
for e in load_events(os.environ["TCP_FILE"]):
    ns = k8s_ns(e)
    if not ns:
        continue
    dst = e.get("dst") or {}
    addr = dst.get("addr") if isinstance(dst, dict) else None
    dst_k8s = dst.get("k8s") if isinstance(dst, dict) else None
    # external = routable addr with no in-cluster k8s attribution
    dst_ns = dst_k8s.get("namespace") if isinstance(dst_k8s, dict) else None
    if addr and not is_private_addr(addr) and not dst_ns:
        observed_connects[ns] = observed_connects.get(ns, 0) + 1

# ---- declared: egress allowlist ConfigMaps ---------------------------------
def kubectl_json(args):
    try:
        out = subprocess.check_output(["kubectl", *args], stderr=subprocess.DEVNULL)
        return json.loads(out)
    except Exception:
        return None

declared = {}      # ns -> {"sandbox": name, "hosts": set}
cms = kubectl_json(["get", "cm", "-A", "-o", "json"]) or {"items": []}
pat = re.compile(r"^karssandbox-(.+)-egress-allowlist$")
for item in cms.get("items", []):
    md = item.get("metadata", {})
    m = pat.match(md.get("name", ""))
    if not m:
        continue
    ns = md.get("namespace", "")
    sandbox = m.group(1)
    hosts = set()
    body = (item.get("data") or {}).get("allowlist.json")
    if body:
        try:
            doc = json.loads(body)
            for ep in doc.get("endpoints", []):
                h = (ep.get("host") or "").rstrip(".").lower()
                if h:
                    hosts.add(h)
        except Exception:
            pass
    declared[ns] = {"sandbox": sandbox, "hosts": hosts}

# Per-sandbox egress ENFORCEMENT MODE (KarsSandbox spec.networkPolicy.egressMode,
# default "Learn"), keyed by the sandbox namespace `kars-<name>`. A learning-mode
# sandbox is unconstrained by design, so its observations must not be scored
# BEYOND-DECLARED.
modes = {}      # ns -> "Learn" | "Strict"
sboxes = kubectl_json(["get", "karssandbox", "-A", "-o", "json"]) or {"items": []}
for item in sboxes.get("items", []):
    name = (item.get("metadata", {}) or {}).get("name", "")
    if not name:
        continue
    mode = (((item.get("spec", {}) or {}).get("networkPolicy", {}) or {}).get("egressMode") or "Learn")
    modes[f"kars-{name}"] = mode

# ---- assemble report -------------------------------------------------------
ns_filter = [x for x in os.environ.get("NS_FILTER", "").split(",") if x]
candidate_ns = set(declared) | set(observed_dns) | set(observed_connects)
candidate_ns = {n for n in candidate_ns if n.startswith("kars-") or n in declared}
if ns_filter:
    candidate_ns = {n for n in candidate_ns if n in ns_filter}

records = []
for ns in sorted(candidate_ns):
    dec = declared.get(ns, {"sandbox": ns[5:] if ns.startswith("kars-") else ns, "hosts": set()})
    dhosts = dec["hosts"]
    ohosts = observed_dns.get(ns, set())
    connects = observed_connects.get(ns, 0)
    beyond = sorted(ohosts - dhosts)
    unused = sorted(dhosts - ohosts)
    mode = modes.get(ns, "Learn")
    # Only a Strict-mode sandbox with a declared allowlist can be scored
    # COMPLIANT / BEYOND-DECLARED; in learn mode the boundary isn't enforced, so
    # reaching novel hosts is expected, not a completeness gap.
    if str(mode).lower() != "strict" or not dhosts:
        verdict = "LEARN"          # learning / unconstrained — enforcement not active
    elif beyond:
        verdict = "BEYOND-DECLARED"
    else:
        verdict = "COMPLIANT"
    records.append({
        "namespace": ns,
        "sandbox": dec["sandbox"],
        "egress_mode": mode,
        "declared_hosts": sorted(dhosts),
        "observed_dns": sorted(ohosts),
        "observed_connects": connects,
        "beyond_declared": beyond,
        "unused_declared": unused,
        "verdict": verdict,
    })

if os.environ.get("EMIT_JSON") == "1":
    print(json.dumps({"window_captured": True, "sandboxes": records}, indent=2))
    sys.exit(0)

if not records:
    print("No governed sandboxes observed. (No egress-allowlist ConfigMaps and no "
          "kars-* pod DNS/TCP in the capture window.)")
    sys.exit(0)

ICON = {"COMPLIANT": "OK  ", "BEYOND-DECLARED": "WARN", "LEARN": "LEARN"}
print(f"{'VERDICT':6} {'SANDBOX':28} {'DECLARED':8} {'OBS-DNS':7} {'CONNECTS':8}  BEYOND-DECLARED")
print("-" * 96)
for r in records:
    print(f"{ICON.get(r['verdict'], r['verdict']):6} {r['sandbox'][:28]:28} "
          f"{len(r['declared_hosts']):<8} {len(r['observed_dns']):<7} "
          f"{r['observed_connects']:<8}  {', '.join(r['beyond_declared'][:4]) or '-'}")
print()
print("VERDICTS: OK = Strict enforcement and every external host observed is declared; "
      "WARN = Strict enforcement but the kernel saw egress beyond the declared allowlist; "
      "LEARN = learning mode (egressMode != Strict) or no allowlist — enforcement not active.")
print("DNS = host intent; CONNECTS = actual external TCP datapath events. "
      "Enforcement remains the router proxy; this witness only attests.")
PY
