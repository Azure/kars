#!/usr/bin/env python3
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""Bounded upstream IG samples, scoped controller baselines, one-object publication.

No headless instances, enforcement, shell evaluation, kubectl, or third-party
Python dependencies. Failures publish diagnostics, never compliant empty samples.
"""

import concurrent.futures
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import re
import signal
import ssl
import struct
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone

NAMESPACE = "kars-witness-gadget"
RELEASE = "kars-datapath-witness"
CM_PATH = f"/api/v1/namespaces/kars-system/configmaps/{RELEASE}"
SA_PATH = Path("/var/run/secrets/kubernetes.io/serviceaccount")
MAX_OUTPUT = 8 * 1024 * 1024
MAX_DOCUMENT = 512 * 1024
HEALTH = Path("/tmp/witness-healthy")
MAX_AGE = 180
STOP = False


class WitnessError(Exception):
    """A bounded diagnostic code, safe for publication (never raw logs/secrets)."""


def utc_now():
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def owned(obj):
    metadata = obj.get("metadata", {})
    labels = metadata.get("labels", {})
    annotations = metadata.get("annotations", {})
    return (
        labels.get("kars.azure.com/witness-addon") == "true"
        and labels.get("app.kubernetes.io/managed-by") == "Helm"
        and annotations.get("meta.helm.sh/release-name") == RELEASE
        and annotations.get("meta.helm.sh/release-namespace") == "kars-system"
    )


class Kubernetes:
    def __init__(self):
        host = os.environ["KUBERNETES_SERVICE_HOST"]
        port = os.environ["KUBERNETES_SERVICE_PORT_HTTPS"]
        if ":" in host:
            host = f"[{host}]"
        self.base = f"https://{host}:{port}"
        self.context = ssl.create_default_context(cafile=str(SA_PATH / "ca.crt"))
        # Do not send service-account credentials through an ambient HTTP proxy.
        self.opener = urllib.request.build_opener(
            urllib.request.ProxyHandler({}),
            urllib.request.HTTPSHandler(context=self.context),
        )

    def request(self, path, method="GET", body=None):
        token = (SA_PATH / "token").read_text().strip()  # projected token rotation
        request = urllib.request.Request(
            self.base + path,
            data=None if body is None else json.dumps(body).encode(),
            headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"},
            method=method,
        )
        try:
            with self.opener.open(request, timeout=15) as response:
                raw = response.read(MAX_OUTPUT + 1)
            if len(raw) > MAX_OUTPUT:
                raise WitnessError("api_response_too_large")
            value = json.loads(raw)
            if not isinstance(value, dict):
                raise WitnessError("api_response_not_object")
            return value
        except urllib.error.HTTPError as error:
            raise WitnessError(f"api_http_{error.code}") from error
        except (urllib.error.URLError, TimeoutError, OSError, ValueError) as error:
            raise WitnessError("api_unavailable_or_invalid") from error


def check_btf(path):
    # Check the mounted *host kernel* BTF header, not node-list permissions.
    # This is only a prerequisite check; loading the gadgets can still fail.
    try:
        with open(path, "rb") as source:
            header = source.read(24)
        if len(header) != 24:
            raise WitnessError("btf_header_missing")
        endian = "<" if header[:2] == b"\x9f\xeb" else ">"
        magic, version, flags, header_len, _, type_len, _, str_len = struct.unpack(
            endian + "HBBIIIII", header
        )
        if magic != 0xEB9F or version != 1 or flags != 0 or header_len < 24 or not type_len or not str_len:
            raise WitnessError("btf_header_invalid")
    except OSError as error:
        raise WitnessError("host_kernel_btf_unreadable") from error


def capture(image, window, nodes):
    command = [
        "kubectl-gadget", "run", image, "--gadget-namespace", NAMESPACE,
        "--node", ",".join(nodes), "--all-namespaces", "--timeout", str(window),
        "--output", "json", "--request-timeout", "15s",
    ]
    with tempfile.TemporaryFile() as output, tempfile.TemporaryFile() as errors:
        try:
            process = subprocess.Popen(command, stdout=output, stderr=errors)
        except OSError as error:
            raise WitnessError("gadget_client_unavailable") from error
        started = time.monotonic()
        deadline = started + window + 45
        failure = None
        try:
            while process.poll() is None:
                if STOP:
                    failure = "capture_interrupted"
                elif time.monotonic() >= deadline:
                    failure = "capture_timeout"
                elif os.fstat(output.fileno()).st_size > MAX_OUTPUT or os.fstat(errors.fileno()).st_size > 65536:
                    failure = "capture_output_limit"
                if failure:
                    break
                time.sleep(0.1)
        finally:
            if process.poll() is None:
                process.send_signal(signal.SIGINT)
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
        if failure:
            raise WitnessError(failure)
        output.seek(0)
        errors.seek(0)
        raw = output.read(MAX_OUTPUT + 1)
        diagnostics = errors.read(65537)
        if len(raw) > MAX_OUTPUT or len(diagnostics) > 65536:
            raise WitnessError("capture_output_limit")
        if process.returncode != 0:
            raise WitnessError("capture_failed")
        if time.monotonic() - started < window:
            raise WitnessError("capture_ended_early")
        # The pinned client reports dropped messages and some decode/remote
        # failures as warnings even with exit 0. They invalidate the sample.
        if re.search(rb"(?i)\b(warn(?:ing)?|error|fatal)\b|messages dropped", diagnostics):
            raise WitnessError("capture_diagnostics")
        return parse_events(raw)


def parse_events(raw):
    events = []
    try:
        for line in raw.decode("utf-8").splitlines():
            if not line.strip():
                continue
            value = json.loads(line)
            batch = value if isinstance(value, list) else [value]
            if any(not isinstance(event, dict) for event in batch):
                raise ValueError("event must be an object")
            events.extend(batch)
    except (ValueError, UnicodeError) as error:
        raise WitnessError("capture_invalid_json") from error
    return events


def ready_nodes(api):
    ds = api.request(f"/apis/apps/v1/namespaces/{NAMESPACE}/daemonsets/gadget")
    if not owned(ds):
        raise WitnessError("gadget_ownership_conflict")
    metadata, status = ds["metadata"], ds.get("status", {})
    desired = status.get("desiredNumberScheduled", 0)
    if (
        not desired
        or status.get("observedGeneration", 0) < metadata.get("generation", 1)
        or status.get("numberReady") != desired
        or status.get("updatedNumberScheduled") != desired
        or status.get("numberUnavailable", 0) != 0
    ):
        raise WitnessError("gadget_not_ready")
    pods = api.request(
        f"/api/v1/namespaces/{NAMESPACE}/pods?labelSelector=k8s-app%3Dgadget"
    )
    result = {}
    for pod in pods.get("items", []):
        md = pod.get("metadata", {})
        owners = md.get("ownerReferences", [])
        ready = any(c.get("type") == "Ready" and c.get("status") == "True"
                    for c in pod.get("status", {}).get("conditions", []))
        node = pod.get("spec", {}).get("nodeName")
        if (
            md.get("deletionTimestamp") or not ready or not node or not md.get("uid")
            or not any(o.get("uid") == metadata.get("uid") and o.get("controller") is True for o in owners)
            or md.get("labels", {}).get("kars.azure.com/witness-addon") != "true"
            or node in result
        ):
            raise WitnessError("gadget_pod_set_invalid")
        result[node] = md["uid"]
    if len(result) != desired:
        raise WitnessError("gadget_node_set_incomplete")
    return result


def declarations(api, names):
    records = []
    for name in names:
        sandbox = api.request(
            f"/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karssandboxes/{name}"
        )
        baseline = api.request(
            f"/api/v1/namespaces/kars-{name}/configmaps/karssandbox-{name}-egress-allowlist"
        )
        try:
            if sandbox["metadata"]["name"] != name or sandbox["metadata"]["namespace"] != "kars-system":
                raise ValueError("sandbox identity")
            if baseline["metadata"]["name"] != f"karssandbox-{name}-egress-allowlist" or baseline["metadata"]["namespace"] != f"kars-{name}":
                raise ValueError("baseline identity")
            mode = sandbox["spec"].get("networkPolicy", {}).get("egressMode", "Learn")
            body = json.loads(baseline["data"]["allowlist.json"])
            if (
                mode not in ("Learn", "Strict")
                or type(body["schemaVersion"]) is not int or body["schemaVersion"] != 1
                or not isinstance(body["endpoints"], list)
            ):
                raise ValueError("baseline schema")
            hosts = set()
            for endpoint in body["endpoints"]:
                host = endpoint["host"]
                if (
                    not isinstance(host, str) or not host or len(host) > 253
                    or any(c.isspace() or ord(c) < 32 for c in host)
                ):
                    raise ValueError("baseline host")
                hosts.add(host.lower().rstrip("."))
            records.append({"namespace": f"kars-{name}", "sandbox": name,
                            "egress_mode": mode, "declared_hosts": sorted(hosts)})
        except (KeyError, TypeError, ValueError, AttributeError) as error:
            raise WitnessError("declaration_invalid") from error
    return records


def internal_host(host):
    return (
        "." not in host or host in {"kubernetes.default", "localhost"}
        or host.endswith((".cluster.local", ".svc", ".arpa", ".local"))
        or ".svc.cluster.local." in host or ".cluster.local." in host
    )


def declared_host(host, declared):
    return any(host == item or (item.startswith("*.") and host.endswith(item[1:]))
               for item in declared)


def compute(records, dns, tcp, nodes):
    by_namespace = {record["namespace"]: record for record in records}
    observed_dns = {ns: set() for ns in by_namespace}
    connects = dict.fromkeys(by_namespace, 0)
    event_nodes = set()
    relevant_events = 0
    try:
        for kind, events in (("dns", dns), ("tcp", tcp)):
            for event in events:
                k8s = event["k8s"]
                ns, node = k8s["namespace"], k8s["node"]
                if not isinstance(ns, str) or node not in nodes:
                    raise ValueError("missing attribution")
                if kind == "dns":
                    name, qr = event["name"], event["qr"]
                    if not isinstance(name, str) or len(name) > 254 or qr not in ("Q", "R"):
                        raise ValueError("DNS schema")
                    name = name.lower().rstrip(".")
                    if ns in by_namespace:
                        relevant_events += 1
                        event_nodes.add(node)
                        if qr == "Q" and not internal_host(name):
                            observed_dns[ns].add(name)
                else:
                    event_type = event["type"]
                    if event_type not in ("connect", "accept", "close"):
                        raise ValueError("TCP schema")
                    raw_address = event["dst"]["addr"]
                    if not isinstance(raw_address, str):
                        raise ValueError("TCP address schema")
                    address = ipaddress.ip_address(raw_address)
                    if ns in by_namespace:
                        relevant_events += 1
                        event_nodes.add(node)
                        if event_type == "connect" and address.is_global:
                            connects[ns] += 1
    except (KeyError, TypeError, ValueError) as error:
        raise WitnessError("capture_event_schema_or_attribution") from error
    for record in records:
        ns = record["namespace"]
        observed = observed_dns[ns]
        beyond = sorted(h for h in observed if not declared_host(h, record["declared_hosts"]))
        if record["egress_mode"] == "Learn":
            verdict = "LEARN"
        elif beyond:
            verdict = "BEYOND-DECLARED"
        elif not observed and not connects[ns]:
            verdict = "NO-TRAFFIC"
        else:
            verdict = "NO-BEYOND-OBSERVED"
        record.update(observed_dns=sorted(observed), observed_connects=connects[ns],
                      beyond_declared=beyond,
                      unused_declared=[h for h in record["declared_hosts"] if not any(declared_host(o, [h]) for o in observed)],
                      verdict=verdict)
    return records, relevant_events, sorted(event_nodes)


class Publisher:
    def __init__(self, api):
        self.api = api
        self.uid = None

    def publish(self, document):
        current = self.api.request(CM_PATH)
        md = current.get("metadata", {})
        uid = md.get("uid")
        if (
            not owned(current) or not uid or (self.uid is not None and self.uid != uid)
            or md.get("name") != RELEASE or md.get("namespace") != "kars-system"
        ):
            raise WitnessError("publisher_ownership_or_uid_conflict")
        if not md.get("resourceVersion") or md.get("deletionTimestamp"):
            raise WitnessError("publisher_object_unavailable")
        self.uid = uid
        document["publisher_uid"] = uid
        body = json.dumps(document, separators=(",", ":"))
        if len(body.encode()) > MAX_DOCUMENT:
            raise WitnessError("witness_document_too_large")
        current.setdefault("data", {})["witness.json"] = body
        # PUT's resourceVersion is an optimistic-concurrency fence. Never retry a
        # conflicting write using stale data, never create/adopt/force apply.
        self.api.request(CM_PATH, "PUT", current)


def sample(api, config):
    before = ready_nodes(api)
    declared = declarations(api, config["sandboxes"])
    started = time.monotonic()
    started_at = utc_now()
    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
        dns = pool.submit(capture, config["dns_image"], config["window_seconds"], sorted(before))
        tcp = pool.submit(capture, config["tcp_image"], config["window_seconds"], sorted(before))
        dns_events, tcp_events = dns.result(), tcp.result()
    if ready_nodes(api) != before:
        raise WitnessError("gadget_nodes_changed_during_capture")
    if declarations(api, config["sandboxes"]) != declared:
        raise WitnessError("declaration_changed_during_capture")
    records, count, event_nodes = compute(declared, dns_events, tcp_events, before)
    if time.monotonic() - started > MAX_AGE:
        raise WitnessError("sample_expired_before_publication")
    return {
        "status": "observed" if count else "empty",
        "started_at": started_at,
        "nodes_targeted": sorted(before), "nodes_with_events": event_nodes,
        "event_count": count, "sandboxes": records,
    }


def run_cycle(api, publisher, config, revision, digest):
    document = {
        "schema_version": 1, "gadget": "inspektor-gadget/v0.53.2",
        "release_revision": revision, "config_digest": digest,
        "window_seconds": config["window_seconds"],
        "coverage": "partial", "sandboxes": [],
    }
    try:
        document.update(sample(api, config))
    except WitnessError as error:
        HEALTH.unlink(missing_ok=True)
        print(f"witness sample unavailable: {error}", file=sys.stderr, flush=True)
        document.update(status="unavailable", diagnostic=str(error), sandboxes=[])
    document["generated_at"] = utc_now()
    try:
        publisher.publish(document)
    except WitnessError:
        HEALTH.unlink(missing_ok=True)
        raise
    if document["status"] in ("observed", "empty"):
        HEALTH.write_text(str(time.time()))
    print(f"witness report published: {document['status']}", flush=True)
    return document


def stop(_signal, _frame):
    global STOP
    STOP = True


def main():
    if sys.argv[1:2] == ["--check-btf"]:
        check_btf(sys.argv[2])
        return
    if sys.argv[1:] == ["--health"]:
        try:
            age = time.time() - float(HEALTH.read_text())
        except (OSError, ValueError) as error:
            raise WitnessError("no_successful_publication") from error
        if not 0 <= age <= MAX_AGE:
            raise WitnessError("publication_stale")
        return
    raw = Path("/etc/witness/config.json").read_bytes()
    digest = hashlib.sha256(raw.rstrip(b"\n")).hexdigest()
    if digest != os.environ["WITNESS_CONFIG_DIGEST"]:
        raise WitnessError("runtime_config_digest_mismatch")
    config = json.loads(raw)
    revision = int(os.environ["WITNESS_REVISION"])
    api = Kubernetes()
    publisher = Publisher(api)
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    while not STOP:
        try:
            run_cycle(api, publisher, config, revision, digest)
        except WitnessError as error:
            HEALTH.unlink(missing_ok=True)
            print(f"witness publication failed: {error}", file=sys.stderr, flush=True)
        for _ in range(config["interval_seconds"]):
            if STOP:
                break
            time.sleep(1)
    HEALTH.unlink(missing_ok=True)


if __name__ == "__main__":
    try:
        main()
    except WitnessError as error:
        print(f"witness failed: {error}", file=sys.stderr)
        sys.exit(1)
