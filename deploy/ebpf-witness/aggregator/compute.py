#!/usr/bin/env python3
"""Kars datapath-witness verdict computation.

Reads kernel-observed egress captured from the continuous Inspektor Gadget
instances (DNS_FILE = trace_dns, TCP_FILE = trace_tcp), cross-checks it against
each sandbox's controller-declared egress allowlist (the
`karssandbox-<name>-egress-allowlist` ConfigMap the controller publishes), and
emits the witness document consumed by the Bridge and by
`witness-verify.sh`. Verdict per sandbox:

  COMPLIANT        every external host observed is in the declared allowlist
  BEYOND-DECLARED  the kernel observed egress to a host NOT declared
  LEARN            no host allowlist published (learn / unconstrained baseline)
"""
import json
import os
import re
import subprocess
import sys
from datetime import datetime, timezone


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
    if "." not in h:  # bare single-label = cluster search-domain lookup
        return True
    return False


def is_private_addr(a):
    a = a or ""
    if a.startswith(("10.", "127.", "169.254.", "192.168.")):
        return True
    if a.startswith("172."):
        try:
            if 16 <= int(a.split(".")[1]) <= 31:
                return True
        except Exception:
            pass
    if a == "::1" or a.startswith(("fc", "fd", "fe80")):
        return True
    return False


def kubectl_json(args):
    try:
        out = subprocess.check_output(["kubectl", *args], stderr=subprocess.DEVNULL)
        return json.loads(out)
    except Exception:
        return None


def main():
    # observed DNS query names (external) per namespace
    observed_dns = {}
    for e in load_events(os.environ.get("DNS_FILE", "")):
        ns = k8s_ns(e)
        name = (e.get("name") or "").rstrip(".")
        if not ns or not name or e.get("qr") == "R":
            continue
        if is_internal_host(name):
            continue
        observed_dns.setdefault(ns, set()).add(name.lower())

    # observed external TCP connects per namespace
    observed_connects = {}
    for e in load_events(os.environ.get("TCP_FILE", "")):
        ns = k8s_ns(e)
        if not ns:
            continue
        dst = e.get("dst") or {}
        addr = dst.get("addr") if isinstance(dst, dict) else None
        dst_k8s = dst.get("k8s") if isinstance(dst, dict) else None
        dst_ns = dst_k8s.get("namespace") if isinstance(dst_k8s, dict) else None
        if addr and not is_private_addr(addr) and not dst_ns:
            observed_connects[ns] = observed_connects.get(ns, 0) + 1

    # declared egress allowlists
    declared = {}
    cms = kubectl_json(["get", "cm", "-A", "-o", "json"]) or {"items": []}
    pat = re.compile(r"^karssandbox-(.+)-egress-allowlist$")
    for item in cms.get("items", []):
        md = item.get("metadata", {})
        m = pat.match(md.get("name", ""))
        if not m:
            continue
        ns = md.get("namespace", "")
        hosts = set()
        body = (item.get("data") or {}).get("allowlist.json")
        if body:
            try:
                for ep in json.loads(body).get("endpoints", []):
                    h = (ep.get("host") or "").rstrip(".").lower()
                    if h:
                        hosts.add(h)
            except Exception:
                pass
        declared[ns] = {"sandbox": m.group(1), "hosts": hosts}

    candidate_ns = set(declared) | set(observed_dns) | set(observed_connects)
    candidate_ns = {n for n in candidate_ns if n.startswith("kars-") or n in declared}

    records = []
    for ns in sorted(candidate_ns):
        dec = declared.get(ns, {"sandbox": ns[5:] if ns.startswith("kars-") else ns, "hosts": set()})
        dhosts = dec["hosts"]
        ohosts = observed_dns.get(ns, set())
        connects = observed_connects.get(ns, 0)
        beyond = sorted(ohosts - dhosts)
        unused = sorted(dhosts - ohosts)
        if ns not in declared or not dhosts:
            verdict = "LEARN"
        elif beyond:
            verdict = "BEYOND-DECLARED"
        else:
            verdict = "COMPLIANT"
        records.append({
            "namespace": ns,
            "sandbox": dec["sandbox"],
            "declared_hosts": sorted(dhosts),
            "observed_dns": sorted(ohosts),
            "observed_connects": connects,
            "beyond_declared": beyond,
            "unused_declared": unused,
            "verdict": verdict,
        })

    doc = {
        "generated_at": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        "window_seconds": int(os.environ.get("WITNESS_WINDOW", "15")),
        "gadget": "inspektor-gadget",
        "sandboxes": records,
    }
    json.dump(doc, sys.stdout)


if __name__ == "__main__":
    main()
